//! Native PDF decoding, rasterization, embedded-image extraction and page serialization.
//! Uses trusted Poppler executables with per-process time/memory/file limits.
//! macOS samples child physical footprint on each 5 ms polling iteration because its
//! `setrlimit(RLIMIT_AS)` implementation rejects limits. This fail-closed
//! sampled guard can overshoot between samples; other Unix hosts retain the
//! kernel-enforced address-space ceiling.
//! No Python process or library is involved; engine-specific text/raster differences remain.
use crate::migration_content::{ContentError, attachments::PdfAttachmentParser};
use crate::migration_knowledge_ingest::{
    IngestError, Result,
    images::{PdfImage, PdfImageBackend},
    parser::{PdfTextBackend, PdfTextDocument},
    pdf_splitter::PdfSplitBackend,
    streaming::{PageContent, PdfSource, StreamingPdfBackend},
    vision::{PdfRasterBackend, RenderedPage},
};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct PopplerPdfConfig {
    pub execution_enabled: bool,
    pub sandbox: Option<crate::migration_knowledge_ingest::parser_sandbox::SandboxPolicy>,
    pub pdfinfo: PathBuf,
    pub pdftotext: PathBuf,
    pub pdftoppm: Option<PathBuf>,
    pub pdfimages: Option<PathBuf>,
    pub pdfseparate: Option<PathBuf>,
    pub pdfunite: Option<PathBuf>,
    pub scratch_root: PathBuf,
    /// Each alternative must be selected explicitly by host configuration.
    pub allow_attachment_alternative: bool,
    pub allow_ingestion_text_alternative: bool,
    pub timeout: Duration,
    pub max_source_bytes: u64,
    pub max_pages: usize,
    pub max_text_bytes: usize,
    pub max_page_characters: usize,
    pub max_render_bytes: usize,
    pub max_render_pixels: u64,
    pub memory_bytes: u64,
    pub max_processes: usize,
}
impl PopplerPdfConfig {
    pub fn new(
        pdfinfo: PathBuf,
        pdftotext: PathBuf,
        pdftoppm: Option<PathBuf>,
        scratch_root: PathBuf,
    ) -> Self {
        Self {
            sandbox: None,
            execution_enabled: true,
            pdfinfo,
            pdftotext,
            pdftoppm,
            pdfimages: None,
            pdfseparate: None,
            pdfunite: None,
            scratch_root,
            allow_attachment_alternative: false,
            allow_ingestion_text_alternative: false,
            timeout: Duration::from_secs(60),
            max_source_bytes: 100 * 1024 * 1024,
            max_pages: 2000,
            max_text_bytes: 48 * 1024 * 1024,
            max_page_characters: 256_000,
            max_render_bytes: 64 * 1024 * 1024,
            max_render_pixels: 64 * 1024 * 1024,
            memory_bytes: 1024 * 1024 * 1024,
            max_processes: 4,
        }
    }
}
#[derive(Clone, Debug)]
pub struct PopplerAvailability {
    pub engine: &'static str,
    pub text_alternative: bool,
    pub attachment_alternative: bool,
    pub rasterization: bool,
    pub strict_pypdf: bool,
    pub embedded_image_extraction: bool,
    pub pdf_page_serialization: bool,
}
#[derive(Clone)]
pub struct PopplerPdfBackend {
    config: Arc<PopplerPdfConfig>,
    active: Arc<AtomicUsize>,
}
impl PopplerPdfBackend {
    pub fn new(mut config: PopplerPdfConfig) -> Result<Self> {
        if !cfg!(unix) {
            return Err(parser("Poppler process isolation requires a Unix host"));
        }
        if config.timeout.is_zero()
            || config.timeout > Duration::from_secs(300)
            || config.max_source_bytes == 0
            || config.max_pages == 0
            || config.max_pages > 2000
            || config.max_text_bytes == 0
            || config.max_page_characters == 0
            || config.max_render_bytes == 0
            || config.max_render_pixels == 0
            || config.memory_bytes < 64 * 1024 * 1024
            || !(1..=32).contains(&config.max_processes)
        {
            return Err(parser("Poppler resource configuration is invalid"));
        }
        let resolve = |path: &Path| match &config.sandbox {
            Some(policy) => policy.executable(path),
            None => executable(path),
        };
        config.pdfinfo = resolve(&config.pdfinfo)?;
        config.pdftotext = resolve(&config.pdftotext)?;
        config.pdftoppm = config.pdftoppm.as_deref().map(resolve).transpose()?;
        config.pdfimages = config.pdfimages.as_deref().map(resolve).transpose()?;
        config.pdfseparate = config.pdfseparate.as_deref().map(resolve).transpose()?;
        config.pdfunite = config.pdfunite.as_deref().map(resolve).transpose()?;
        if !config.scratch_root.is_absolute() {
            return Err(parser("Poppler scratch root must be absolute"));
        }
        // Existing roots must not be symlinks. Per-operation directories are
        // created exclusively with mode0700; no shared file names are reused.
        if let Ok(meta) = fs::symlink_metadata(&config.scratch_root) {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(parser("Poppler scratch root is not a directory"));
            }
        }
        Ok(Self {
            config: Arc::new(config),
            active: Arc::new(AtomicUsize::new(0)),
        })
    }
    pub fn availability(&self) -> PopplerAvailability {
        PopplerAvailability {
            engine: "poppler-process",
            text_alternative: self.config.allow_ingestion_text_alternative,
            attachment_alternative: self.config.allow_attachment_alternative,
            rasterization: self.config.pdftoppm.is_some(),
            strict_pypdf: false,
            embedded_image_extraction: self.config.pdfimages.is_some(),
            pdf_page_serialization: self.config.pdfseparate.is_some()
                && self.config.pdfunite.is_some(),
        }
    }
    fn bytes_source(&self, bytes: &[u8], max: u64) -> Result<PdfSource> {
        if bytes.len() as u64 > max.min(self.config.max_source_bytes) {
            return Err(IngestError::Validation(
                "PDF source exceeds the input byte limit".into(),
            ));
        }
        Ok(PdfSource::Bytes(Arc::new(bytes.to_vec())))
    }
    pub fn open_document(&self, source: &PdfSource) -> Result<PopplerPdfDocument> {
        self.open_bounded(source, self.config.max_source_bytes, self.config.max_pages)
    }
    fn open_bounded(
        &self,
        source: &PdfSource,
        max_bytes: u64,
        max_pages: usize,
    ) -> Result<PopplerPdfDocument> {
        let deadline = Instant::now() + self.config.timeout;
        let scratch = Scratch::new(&self.config.scratch_root)?;
        let input = scratch.path.join("source.pdf");
        copy_source(source, &input, max_bytes.min(self.config.max_source_bytes))?;
        let info = self.info(&input, &scratch.path, None, deadline)?;
        if info.pages > max_pages.min(self.config.max_pages) {
            return Err(IngestError::Validation(
                "PDF exceeds the configured page limit".into(),
            ));
        }
        Ok(PopplerPdfDocument {
            backend: self.clone(),
            scratch,
            input,
            pages: info.pages,
            deadline,
            text_bytes: 0,
        })
    }
    fn info(
        &self,
        input: &Path,
        directory: &Path,
        page: Option<usize>,
        deadline: Instant,
    ) -> Result<PdfInfo> {
        let mut args = vec![OsString::from("-box")];
        if let Some(page) = page {
            args.extend([
                "-f".into(),
                (page + 1).to_string().into(),
                "-l".into(),
                (page + 1).to_string().into(),
            ]);
        }
        args.push(input.as_os_str().to_owned());
        let output = self.run(
            &self.config.pdfinfo,
            &args,
            directory,
            deadline,
            256 * 1024,
            256 * 1024,
        )?;
        parse_info(&output)
    }
    fn page_text(
        &self,
        input: &Path,
        directory: &Path,
        page: usize,
        deadline: Instant,
    ) -> Result<String> {
        let args = vec![
            "-f".into(),
            (page + 1).to_string().into(),
            "-l".into(),
            (page + 1).to_string().into(),
            "-enc".into(),
            "UTF-8".into(),
            "-eol".into(),
            "unix".into(),
            "-raw".into(),
            "-nopgbrk".into(),
            input.as_os_str().to_owned(),
            "-".into(),
        ];
        let cap = self
            .config
            .max_text_bytes
            .min(self.config.max_page_characters.saturating_mul(4));
        let output = self.run(&self.config.pdftotext, &args, directory, deadline, cap, cap)?;
        let text =
            String::from_utf8(output).map_err(|_| parser("Poppler text is not valid UTF-8"))?;
        if text.chars().count() > self.config.max_page_characters {
            return Err(IngestError::Validation(
                "PDF page text exceeds the parser output limit".into(),
            ));
        }
        Ok(text)
    }
    pub fn render_source(
        &self,
        source: &PdfSource,
        page: usize,
        dpi: Option<u32>,
    ) -> Result<RenderedPage> {
        let document = self.open_document(source)?;
        if page >= document.pages {
            return Err(parser("PDF page is out of range"));
        }
        let executable = self
            .config
            .pdftoppm
            .as_ref()
            .ok_or_else(|| parser("PDF rasterization is not configured"))?;
        let dpi = dpi.unwrap_or(72);
        if !(1..=1200).contains(&dpi) {
            return Err(parser(
                "PDF raster DPI is outside the configured native range",
            ));
        }
        let info = self.info(
            &document.input,
            &document.scratch.path,
            Some(page),
            document.deadline,
        )?;
        let (width, height) = info
            .page_size
            .ok_or_else(|| parser("PDF page dimensions are unavailable"))?;
        let scale = f64::from(dpi) / 72.0;
        let pixels = (width * scale).ceil() * (height * scale).ceil();
        if !pixels.is_finite() || pixels < 1.0 || pixels > self.config.max_render_pixels as f64 {
            return Err(IngestError::Validation(
                "PDF raster exceeds the pixel limit".into(),
            ));
        }
        let prefix = document.scratch.path.join("page");
        let args = vec![
            "-f".into(),
            (page + 1).to_string().into(),
            "-l".into(),
            (page + 1).to_string().into(),
            "-singlefile".into(),
            "-cropbox".into(),
            "-r".into(),
            dpi.to_string().into(),
            "-png".into(),
            document.input.as_os_str().to_owned(),
            prefix.as_os_str().to_owned(),
        ];
        self.run(
            executable,
            &args,
            &document.scratch.path,
            document.deadline,
            4096,
            self.config.max_render_bytes,
        )?;
        let png = read_regular_bounded(
            &prefix.with_extension("png"),
            self.config.max_render_bytes as u64,
        )?;
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(parser("Poppler raster is not a PNG"));
        }
        let reader =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png);
        let (width, height) = reader
            .into_dimensions()
            .map_err(|_| parser("Poppler raster dimensions are invalid"))?;
        if width == 0
            || height == 0
            || u64::from(width) * u64::from(height) > self.config.max_render_pixels
        {
            return Err(IngestError::Validation(
                "PDF raster exceeds the pixel limit".into(),
            ));
        }
        Ok(RenderedPage {
            page_index: page,
            png,
            width: width as usize,
            height: height as usize,
        })
    }
    fn run(
        &self,
        executable: &Path,
        args: &[OsString],
        directory: &Path,
        deadline: Instant,
        stdout_cap: usize,
        file_cap: usize,
    ) -> Result<Vec<u8>> {
        if !self.config.execution_enabled {
            return Err(parser("converter execution requires a parse worker role"));
        }
        let _permit = ProcessPermit::acquire(&self.active, self.config.max_processes)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|t| !t.is_zero())
            .ok_or_else(|| parser("PDF operation timed out"))?;
        let executable = match &self.config.sandbox {
            Some(policy) => policy.executable(executable)?,
            None => executable_path_again(executable)?,
        };
        let mut command = if let Some(policy) = &self.config.sandbox {
            policy.command(&executable, args, directory)?
        } else {
            let mut command = Command::new(executable);
            command.args(args);
            command
        };
        command
            .current_dir(directory)
            .env_clear()
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("HOME", directory)
            .env("TMPDIR", directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_child(
            &mut command,
            remaining,
            self.config.memory_bytes,
            file_cap as u64,
        )?;
        let child = command
            .spawn()
            .map_err(|_| parser("Configured Poppler executable could not start"))?;
        let mut child = ChildGuard {
            child,
            finished: false,
        };
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| parser("Poppler output pipe is unavailable"))?;
        let stderr = child
            .child
            .stderr
            .take()
            .ok_or_else(|| parser("Poppler diagnostic pipe is unavailable"))?;
        let exceeded = Arc::new(AtomicBool::new(false));
        let (out_rx, out_thread) = reader_thread(stdout, stdout_cap, exceeded.clone());
        let (err_rx, err_thread) = reader_thread(stderr, 64 * 1024, exceeded.clone());
        let mut out = None;
        let mut err = None;
        let mut status = None;
        let mut failure = None;
        loop {
            if out.is_none() {
                match out_rx.try_recv() {
                    Ok(value) => out = Some(value),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        failure = Some("PDF output reader failed");
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => (),
                }
            }
            if err.is_none() {
                match err_rx.try_recv() {
                    Ok(value) => err = Some(value),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        failure = Some("PDF diagnostic reader failed");
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => (),
                }
            }
            if exceeded.load(Ordering::Acquire) {
                failure = Some("PDF process output exceeds the configured limit");
                break;
            }
            match child.child.try_wait() {
                Ok(Some(value)) => status = Some(value),
                Ok(None) => (),
                Err(_) => {
                    failure = Some("PDF process status could not be read");
                    break;
                }
            }
            #[cfg(target_os = "macos")]
            if status.is_none() {
                match sample_darwin_child_memory(&mut child.child, self.config.memory_bytes) {
                    Ok(DarwinMemorySample::Exceeded) => {
                        failure = Some("PDF process exceeded the configured memory limit");
                        break;
                    }
                    Ok(DarwinMemorySample::WithinLimit) => (),
                    Ok(DarwinMemorySample::Exited(value)) => status = Some(value),
                    Err(_) => {
                        failure = Some("PDF process memory use could not be sampled");
                        break;
                    }
                }
            }
            if status.is_some() && out.is_some() && err.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                failure = Some("PDF operation timed out");
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if failure.is_some() {
            child.terminate();
        } else {
            child.finished = true;
            let _ = child.child.wait();
        }
        let _ = out_thread.join();
        let _ = err_thread.join();
        if let Some(reason) = failure {
            return Err(parser(reason));
        }
        if !status.is_some_and(|s| s.success()) {
            return Err(parser(
                "Poppler rejected an unsupported, encrypted or malformed PDF",
            ));
        }
        let out = out.ok_or_else(|| parser("Poppler output is unavailable"))??;
        let err = err.ok_or_else(|| parser("Poppler diagnostics are unavailable"))??;
        // Warnings can indicate xref reconstruction, corrupt streams or fonts.
        // This adapter deliberately rejects them rather than claiming strict
        // pypdf fidelity after Poppler recovered a malformed document.
        if !err.iter().all(u8::is_ascii_whitespace) {
            return Err(parser(
                "Poppler reported PDF syntax or decoding diagnostics",
            ));
        }
        Ok(out)
    }
}

pub struct PopplerPdfDocument {
    backend: PopplerPdfBackend,
    scratch: Scratch,
    input: PathBuf,
    pages: usize,
    deadline: Instant,
    text_bytes: usize,
}
impl PdfTextDocument for PopplerPdfDocument {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn page_text(&mut self, index: usize) -> Result<String> {
        if index >= self.pages {
            return Err(parser("PDF page is out of range"));
        }
        let text = self
            .backend
            .page_text(&self.input, &self.scratch.path, index, self.deadline)?;
        self.text_bytes = self
            .text_bytes
            .checked_add(text.len())
            .ok_or_else(|| parser("PDF text size overflow"))?;
        if self.text_bytes > self.backend.config.max_text_bytes {
            return Err(IngestError::Validation(
                "PDF text exceeds the total output limit".into(),
            ));
        }
        Ok(text)
    }
}
impl PdfTextBackend for PopplerPdfBackend {
    fn open_pypdf(&self, _bytes: &[u8], _stream_limit: usize) -> Result<Box<dyn PdfTextDocument>> {
        Err(parser(
            "Poppler does not implement pypdf strict parsing or its decompressed-stream limit; select the explicit text alternative",
        ))
    }
    fn open_pymupdf(&self, bytes: &[u8]) -> Result<Box<dyn PdfTextDocument>> {
        if !self.config.allow_ingestion_text_alternative {
            return Err(parser(
                "Poppler ingestion text alternative was not explicitly selected",
            ));
        }
        let source = self.bytes_source(bytes, 16 * 1024 * 1024)?;
        Ok(Box::new(self.open_bounded(
            &source,
            16 * 1024 * 1024,
            500,
        )?))
    }
}
impl PdfAttachmentParser for PopplerPdfBackend {
    fn extract_strict(
        &self,
        raw: &[u8],
        max_pages: usize,
    ) -> std::result::Result<(String, usize), ContentError> {
        let parse = || -> Result<(String, usize)> {
            if !self.config.allow_attachment_alternative {
                return Err(parser(
                    "Poppler attachment alternative was not explicitly selected",
                ));
            }
            let source = self.bytes_source(raw, 32 * 1024 * 1024)?;
            let mut document = self.open_bounded(&source, 32 * 1024 * 1024, max_pages.min(100))?;
            let count = document.page_count();
            let mut text = String::new();
            for page in 0..count {
                let part = document.page_text(page)?;
                if page > 0 {
                    text.push('\n')
                }
                if part.len() > self.config.max_text_bytes.saturating_sub(text.len()) {
                    return Err(parser("PDF attachment text exceeds the output limit"));
                }
                text.push_str(&part);
            }
            Ok((text, count))
        };
        parse().map_err(|_| ContentError::new(422, "PDF cannot be safely parsed"))
    }
}
impl PdfRasterBackend for PopplerPdfBackend {
    fn page_count(&self, pdf: &[u8]) -> Result<usize> {
        Ok(self
            .open_bounded(
                &self.bytes_source(pdf, 16 * 1024 * 1024)?,
                16 * 1024 * 1024,
                500,
            )?
            .pages)
    }
    fn render(&self, pdf: &[u8], page: usize, dpi: Option<u32>) -> Result<RenderedPage> {
        if pdf.len() > 16 * 1024 * 1024 {
            return Err(parser("PDF source exceeds the 16777216 byte parser limit"));
        }
        self.render_source(&self.bytes_source(pdf, 16 * 1024 * 1024)?, page, dpi)
    }
}
impl StreamingPdfBackend for PopplerPdfBackend {
    fn page_count(&self, source: &PdfSource) -> Result<usize> {
        Ok(self.open_document(source)?.pages)
    }
    fn extract_page(
        &self,
        source: &PdfSource,
        page_index: usize,
        extract_images: bool,
        extract_images_if_no_text: bool,
        max_image_dimension: usize,
    ) -> Result<PageContent> {
        if !self.config.allow_ingestion_text_alternative {
            return Err(parser(
                "Poppler streaming text alternative was not explicitly selected",
            ));
        }
        let mut document = self.open_document(source)?;
        let text = document.page_text(page_index)?;
        let images = if extract_images || (extract_images_if_no_text && text.trim().is_empty()) {
            self.extract_images_document(&document, page_index)?
                .into_iter()
                .map(|image| {
                    crate::migration_knowledge_ingest::streaming::resize_image(
                        &image.bytes,
                        max_image_dimension,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(PageContent::new(page_index + 1, text, images))
    }
}
impl PdfImageBackend for PopplerPdfBackend {
    fn page_texts(&self, bytes: &[u8]) -> Result<Vec<String>> {
        if !self.config.allow_ingestion_text_alternative {
            return Err(parser(
                "Poppler text alternative was not explicitly selected",
            ));
        }
        let mut document =
            self.open_document(&self.bytes_source(bytes, self.config.max_source_bytes)?)?;
        (0..document.pages)
            .map(|page| document.page_text(page))
            .collect()
    }
    fn page_images(&self, bytes: &[u8], zero_based_page: usize) -> Result<Vec<PdfImage>> {
        let source = self.bytes_source(bytes, self.config.max_source_bytes)?;
        let document = self.open_document(&source)?;
        self.extract_images_document(&document, zero_based_page)
    }
}
impl PdfSplitBackend for PopplerPdfBackend {
    fn page_count(&self, source: &Path) -> Result<usize> {
        Ok(self
            .open_document(&PdfSource::Path(source.to_path_buf()))?
            .pages)
    }
    fn write_pages(
        &self,
        source: &Path,
        start: usize,
        end: usize,
        destination: &Path,
    ) -> Result<()> {
        let separate = self
            .config
            .pdfseparate
            .as_ref()
            .ok_or_else(|| parser("pdfseparate is not configured"))?;
        let unite = self
            .config
            .pdfunite
            .as_ref()
            .ok_or_else(|| parser("pdfunite is not configured"))?;
        let document = self.open_document(&PdfSource::Path(source.to_path_buf()))?;
        if start >= end || end > document.pages {
            return Err(parser("PDF split page range is invalid"));
        }
        let pattern = document.scratch.path.join("split-%d.pdf");
        self.run(
            separate,
            &[
                "-f".into(),
                (start + 1).to_string().into(),
                "-l".into(),
                end.to_string().into(),
                document.input.as_os_str().to_owned(),
                pattern.as_os_str().to_owned(),
            ],
            &document.scratch.path,
            document.deadline,
            1024 * 1024,
            self.config.max_source_bytes as usize,
        )?;
        let output = document.scratch.path.join("joined.pdf");
        let mut args = Vec::new();
        for page in start + 1..=end {
            let part = document.scratch.path.join(format!("split-{page}.pdf"));
            let _ = read_regular_bounded(&part, self.config.max_source_bytes)?;
            args.push(part.into_os_string());
        }
        args.push(output.as_os_str().to_owned());
        self.run(
            unite,
            &args,
            &document.scratch.path,
            document.deadline,
            1024 * 1024,
            self.config.max_source_bytes as usize,
        )?;
        let bytes = read_regular_bounded(&output, self.config.max_source_bytes)?;
        if !bytes.starts_with(b"%PDF-") {
            return Err(parser("PDF page serialization output is invalid"));
        }
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|_| parser("PDF split destination is unavailable"))?;
        target
            .write_all(&bytes)
            .map_err(|_| parser("PDF split destination write failed"))?;
        Ok(())
    }
}
impl PopplerPdfBackend {
    fn extract_images_document(
        &self,
        document: &PopplerPdfDocument,
        page: usize,
    ) -> Result<Vec<PdfImage>> {
        if page >= document.pages {
            return Err(parser("PDF page is out of range"));
        }
        let executable = self
            .config
            .pdfimages
            .as_ref()
            .ok_or_else(embedded_images_unsupported)?;
        let page_args = [
            "-f".into(),
            (page + 1).to_string().into(),
            "-l".into(),
            (page + 1).to_string().into(),
        ];
        let mut list = page_args.to_vec();
        list.push("-list".into());
        list.push(document.input.as_os_str().to_owned());
        let listing = self.run(
            executable,
            &list,
            &document.scratch.path,
            document.deadline,
            2 * 1024 * 1024,
            self.config.max_render_bytes,
        )?;
        let listing =
            String::from_utf8(listing).map_err(|_| parser("PDF image inventory is invalid"))?;
        let entries = listing
            .lines()
            .filter_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                if fields.len() < 12 || fields[0].parse::<usize>().ok() != Some(page + 1) {
                    return None;
                }
                Some((
                    fields[1].parse::<usize>().ok()?,
                    fields[2].to_owned(),
                    fields[10].parse::<i64>().ok()?,
                ))
            })
            .collect::<Vec<_>>();
        if entries.len() > 1000 {
            return Err(parser("PDF page image count exceeds limit"));
        }
        let prefix = document.scratch.path.join(format!("image-page-{page}"));
        let mut args = page_args.to_vec();
        args.push("-png".into());
        args.push(document.input.as_os_str().to_owned());
        args.push(prefix.as_os_str().to_owned());
        self.run(
            executable,
            &args,
            &document.scratch.path,
            document.deadline,
            1024 * 1024,
            self.config.max_render_bytes,
        )?;
        let mut out = Vec::new();
        let mut total = 0usize;
        for (number, kind, xref) in entries {
            if kind != "image" {
                continue;
            }
            let path = PathBuf::from(format!("{}-{number:03}.png", prefix.display()));
            let bytes = read_regular_bounded(&path, self.config.max_render_bytes as u64)?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| parser("PDF image output overflow"))?;
            if total > self.config.max_render_bytes {
                return Err(parser("PDF page image bytes exceed limit"));
            }
            let (width, height) = image::ImageReader::with_format(
                std::io::Cursor::new(&bytes),
                image::ImageFormat::Png,
            )
            .into_dimensions()
            .map_err(|_| parser("PDF embedded image dimensions are invalid"))?;
            if u64::from(width) * u64::from(height) > self.config.max_render_pixels {
                return Err(parser("PDF embedded image pixels exceed limit"));
            }
            out.push(PdfImage {
                bytes,
                extension: "png".into(),
                width,
                height,
                xref,
            });
        }
        Ok(out)
    }
}

fn embedded_images_unsupported() -> IngestError {
    parser("PDF embedded image extraction requires configured pdfimages")
}
fn parser(message: &str) -> IngestError {
    IngestError::Parser(message.into())
}

struct PdfInfo {
    pages: usize,
    page_size: Option<(f64, f64)>,
}
fn parse_info(bytes: &[u8]) -> Result<PdfInfo> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| parser("PDF information is not valid UTF-8"))?;
    let mut pages = None;
    let mut encrypted = None;
    let mut page_size = None;
    let size = regex::Regex::new(
        r"^Page(?:\s+\d+)?\s+size:\s+([0-9]+(?:\.[0-9]+)?)\s+x\s+([0-9]+(?:\.[0-9]+)?)\s+pts",
    )
    .expect("static pdfinfo size pattern");
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Pages:") {
            if pages.is_some() {
                return Err(parser("PDF information has duplicate page counts"));
            }
            pages = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| parser("PDF page count is invalid"))?,
            );
        }
        if let Some(value) = line.strip_prefix("Encrypted:") {
            if encrypted.is_some() {
                return Err(parser("PDF information has duplicate encryption fields"));
            }
            encrypted = Some(
                value
                    .trim()
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
            );
        }
        if let Some(value) = size.captures(line) {
            if page_size.is_some() {
                return Err(parser("PDF page information is ambiguous"));
            }
            let w = value[1]
                .parse::<f64>()
                .map_err(|_| parser("PDF page size is invalid"))?;
            let h = value[2]
                .parse::<f64>()
                .map_err(|_| parser("PDF page size is invalid"))?;
            if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
                return Err(parser("PDF page size is invalid"));
            }
            page_size = Some((w, h));
        }
    }
    if encrypted.as_deref() != Some("no") {
        return Err(IngestError::Validation(
            "Encrypted PDF documents are unsupported".into(),
        ));
    }
    let pages = pages
        .filter(|n| *n > 0)
        .ok_or_else(|| parser("PDF page count is unavailable"))?;
    Ok(PdfInfo { pages, page_size })
}
fn executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(parser("PDF executable path must be absolute"));
    }
    let path = path
        .canonicalize()
        .map_err(|_| parser("Configured PDF executable is unavailable"))?;
    executable_path_again(&path)
}
fn executable_path_again(path: &Path) -> Result<PathBuf> {
    let info = fs::symlink_metadata(path)
        .map_err(|_| parser("Configured PDF executable is unavailable"))?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(parser("PDF executable is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = info.permissions().mode();
        if mode & 0o111 == 0 || mode & 0o022 != 0 {
            return Err(parser("PDF executable permissions are not trusted"));
        }
    }
    Ok(path.to_path_buf())
}
struct Scratch {
    path: PathBuf,
}
impl Scratch {
    fn new(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).map_err(|_| parser("PDF scratch root could not be created"))?;
        let info =
            fs::symlink_metadata(root).map_err(|_| parser("PDF scratch root unavailable"))?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(parser("PDF scratch root is not a directory"));
        }
        let path = root.join(format!("pdf-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| parser("PDF scratch directory could not be created"))?;
        Ok(Self { path })
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn regular_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|_| parser("PDF source file cannot be opened"))?;
    if !file
        .metadata()
        .map_err(|_| parser("PDF source metadata unavailable"))?
        .is_file()
    {
        return Err(parser("PDF source is not a regular file"));
    }
    Ok(file)
}
fn copy_source(source: &PdfSource, target: &Path, max: u64) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut target = options
        .open(target)
        .map_err(|_| parser("PDF scratch input cannot be created"))?;
    let mut reader: Box<dyn Read + '_> = match source {
        PdfSource::Bytes(bytes) => {
            if bytes.len() as u64 > max {
                return Err(IngestError::Validation(
                    "PDF source exceeds the input byte limit".into(),
                ));
            }
            Box::new(std::io::Cursor::new(bytes.as_slice()))
        }
        PdfSource::Path(path) => {
            let file = regular_file(path)?;
            if file
                .metadata()
                .map_err(|_| parser("PDF input size unavailable"))?
                .len()
                > max
            {
                return Err(IngestError::Validation(
                    "PDF source exceeds the input byte limit".into(),
                ));
            }
            Box::new(file)
        }
    };
    let mut prefix = [0u8; 5];
    reader
        .read_exact(&mut prefix)
        .map_err(|_| parser("PDF source is truncated"))?;
    if &prefix != b"%PDF-" {
        return Err(parser("PDF header is invalid"));
    }
    target
        .write_all(&prefix)
        .map_err(|_| parser("PDF scratch input write failed"))?;
    let mut count = 5u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|_| parser("PDF source read failed"))?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| parser("PDF source size overflow"))?;
        if count > max {
            return Err(IngestError::Validation(
                "PDF source exceeds the input byte limit".into(),
            ));
        }
        target
            .write_all(&buffer[..n])
            .map_err(|_| parser("PDF scratch input write failed"))?;
    }
    target
        .flush()
        .map_err(|_| parser("PDF scratch input flush failed"))
}
fn read_regular_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let file = regular_file(path)?;
    if file
        .metadata()
        .map_err(|_| parser("PDF output size unavailable"))?
        .len()
        > max
    {
        return Err(parser("PDF output exceeds the byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| parser("PDF output read failed"))?;
    if bytes.len() as u64 > max {
        return Err(parser("PDF output exceeds the byte limit"));
    }
    Ok(bytes)
}
struct ProcessPermit(Arc<AtomicUsize>);
impl ProcessPermit {
    fn acquire(active: &Arc<AtomicUsize>, max: usize) -> Result<Self> {
        active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .map_err(|_| parser("PDF process concurrency limit reached"))?;
        Ok(Self(active.clone()))
    }
}
impl Drop for ProcessPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct ChildGuard {
    child: Child,
    finished: bool,
}
impl ChildGuard {
    fn terminate(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finished = true;
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.terminate()
        }
    }
}
fn reader_thread<R: Read + Send + 'static>(
    mut pipe: R,
    cap: usize,
    exceeded: Arc<AtomicBool>,
) -> (mpsc::Receiver<Result<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 16384];
            loop {
                let n = pipe
                    .read(&mut buffer)
                    .map_err(|_| parser("PDF process pipe read failed"))?;
                if n == 0 {
                    return Ok(bytes);
                }
                if n > cap.saturating_sub(bytes.len()) {
                    exceeded.store(true, Ordering::Release);
                    return Err(parser("PDF process output exceeds the configured limit"));
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
        })();
        let _ = tx.send(result);
    });
    (rx, thread)
}
#[cfg(unix)]
fn configure_child(command: &mut Command, timeout: Duration, memory: u64, file: u64) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let cpu = timeout.as_secs().saturating_add(2);
    #[cfg(not(target_os = "macos"))]
    let memory = memory as libc::rlim_t;
    #[cfg(target_os = "macos")]
    let _ = memory;
    let file = file as libc::rlim_t;
    // Only async-signal-safe libc calls and stack values are used after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let cpu_limit = libc::rlimit {
                rlim_cur: cpu as libc::rlim_t,
                rlim_max: cpu as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu_limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Darwin defines RLIMIT_AS but its kernel rejects setrlimit for
            // that resource (EINVAL). The parent samples proc_pid_rusage while
            // the child runs and kills its process group if the ceiling is
            // crossed or sampling fails. Linux and other Unix hosts retain the
            // kernel-enforced address-space ceiling.
            #[cfg(not(target_os = "macos"))]
            {
                let memory_limit = libc::rlimit {
                    rlim_cur: memory,
                    rlim_max: memory,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &memory_limit) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            let file_limit = libc::rlimit {
                rlim_cur: file,
                rlim_max: file,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &file_limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let open_file_limit = libc::rlimit {
                rlim_cur: 64,
                rlim_max: 64,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &open_file_limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let core_limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &core_limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct DarwinRusageInfoV2 {
    uuid: [u8; 16],
    _before_footprint: [u64; 7],
    phys_footprint: u64,
    _after_footprint: [u64; 10],
}

#[cfg(target_os = "macos")]
fn darwin_memory_limit_exceeded(pid: u32, limit: u64) -> std::io::Result<bool> {
    let mut usage = std::mem::MaybeUninit::<DarwinRusageInfoV2>::zeroed();
    let result = unsafe {
        libc::proc_pid_rusage(
            pid as libc::pid_t,
            libc::RUSAGE_INFO_V2,
            usage.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let footprint = unsafe { usage.assume_init() }.phys_footprint;
    Ok(footprint > limit)
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
enum DarwinMemorySample {
    WithinLimit,
    Exceeded,
    Exited(std::process::ExitStatus),
}

#[cfg(target_os = "macos")]
fn sample_darwin_child_memory(
    child: &mut Child,
    limit: u64,
) -> std::io::Result<DarwinMemorySample> {
    let sample = darwin_memory_limit_exceeded(child.id(), limit);
    reconcile_darwin_memory_sample(child, sample)
}

#[cfg(target_os = "macos")]
fn reconcile_darwin_memory_sample(
    child: &mut Child,
    sample: std::io::Result<bool>,
) -> std::io::Result<DarwinMemorySample> {
    match sample {
        Ok(false) => Ok(DarwinMemorySample::WithinLimit),
        Ok(true) => Ok(DarwinMemorySample::Exceeded),
        Err(sample_error) => match child.try_wait()? {
            // The process may exit after the caller's first try_wait. Recheck
            // before treating an unavailable footprint as a live-child error.
            Some(status) => Ok(DarwinMemorySample::Exited(status)),
            None => Err(sample_error),
        },
    }
}

#[cfg(not(unix))]
fn configure_child(
    _command: &mut Command,
    _timeout: Duration,
    _memory: u64,
    _file: u64,
) -> Result<()> {
    Err(parser("PDF process resource limits require Unix"))
}

#[cfg(test)]
#[path = "pdf_backend_tests.rs"]
mod native_tests;
