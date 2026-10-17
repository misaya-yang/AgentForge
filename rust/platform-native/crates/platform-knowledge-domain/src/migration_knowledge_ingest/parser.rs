//! Bounded native source decoding and real PDF/OCR executable adapters.
//! A missing parser produces an error, never a fabricated extraction receipt.
use super::{IngestError, Result, sha256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TEXT_CHARS: usize = 16_000_000;
pub const MAX_TEXT_BYTES: usize = 48 * 1024 * 1024;
pub const MAX_PARTS: usize = 65_536;
pub const MAX_PAGES: usize = 500;

pub fn require_text_budget(text: &str) -> Result<()> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(IngestError::Validation(format!(
            "extracted text exceeds the {MAX_TEXT_BYTES} byte limit"
        )));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(IngestError::Validation(format!(
            "extracted text exceeds the {MAX_TEXT_CHARS} character limit"
        )));
    }
    Ok(())
}
pub fn sanitize_text(text: &str) -> Result<String> {
    require_text_budget(text)?;
    Ok(text
        .chars()
        .filter(|c| *c > '\u{1f}' || matches!(*c, '\t' | '\n' | '\r'))
        .collect())
}
pub fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}
fn bad(s: &str) -> IngestError {
    IngestError::Parser(s.into())
}
fn bounded_append(out: &mut String, value: &str, sep: &str) -> Result<()> {
    if !out.is_empty() {
        out.push_str(sep)
    }
    out.push_str(value);
    require_text_budget(out)
}
pub fn decode_text_bytes(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(bad("text source exceeds the 16777216 byte parser limit"));
    }
    // An explicit encoding marker is authoritative. Never reinterpret corrupt
    // Unicode as GBK/Latin-1: that can publish apparently successful gibberish.
    if bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff])
    {
        return Err(bad("UTF-32 text is unsupported; export as UTF-8 or UTF-16"));
    }
    if let Some(body) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        let text = std::str::from_utf8(body)
            .map_err(|_| bad("UTF-8 text contains invalid bytes; export the source again"))?;
        return sanitize_text(text);
    }
    let utf16 = bytes
        .strip_prefix(&[0xff, 0xfe])
        .map(|body| (body, true))
        .or_else(|| bytes.strip_prefix(&[0xfe, 0xff]).map(|body| (body, false)));
    if let Some((body, little_endian)) = utf16 {
        if body.len() % 2 != 0 {
            return Err(bad(
                "UTF-16 text has an incomplete code unit; export the source again",
            ));
        }
        let units = body.chunks_exact(2).map(|pair| {
            let pair = [pair[0], pair[1]];
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        let text: std::result::Result<String, _> = char::decode_utf16(units).collect();
        return sanitize_text(&text.map_err(|_| {
            bad("UTF-16 text contains an unpaired surrogate; export the source again")
        })?);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return sanitize_text(s);
    }
    if let Some(s) = encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(bytes) {
        return sanitize_text(&s);
    }
    sanitize_text(&bytes.iter().map(|b| char::from(*b)).collect::<String>())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub block_id: String,
    #[serde(
        rename = "type",
        default = "text_kind",
        deserialize_with = "block_kind"
    )]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub order: usize,
    pub bbox: Option<Value>,
    #[serde(default)]
    pub parser: String,
    #[serde(default)]
    pub parser_version: String,
    pub confidence: Option<f64>,
    pub table: Option<Value>,
    pub formula: Option<Value>,
    pub figure: Option<Value>,
    #[serde(default = "empty_metadata")]
    pub metadata: Value,
}
fn empty_metadata() -> Value {
    json!({})
}
fn text_kind() -> String {
    "text".into()
}
fn schema_version() -> String {
    "1".into()
}
fn block_kind<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(match value.as_str() {
        Some(
            kind @ ("text" | "heading" | "table" | "formula" | "figure" | "list" | "caption"
            | "code" | "unknown"),
        ) => kind,
        _ => "unknown",
    }
    .into())
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}
impl BBox {
    pub fn from_list(value: &Value) -> Option<Self> {
        let values = value.as_array().filter(|v| v.len() >= 4)?;
        let number = |i: usize| {
            values[i]
                .as_f64()
                .or_else(|| values[i].as_str()?.parse().ok())
        };
        Some(Self {
            x0: number(0)?,
            y0: number(1)?,
            x1: number(2)?,
            y1: number(3)?,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageIR {
    pub page_number: usize,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub parser: String,
    #[serde(default)]
    pub parser_version: String,
    pub page_width: Option<f64>,
    pub page_height: Option<f64>,
    pub confidence: Option<f64>,
    #[serde(default)]
    pub hard_page: bool,
    #[serde(default = "empty_metadata")]
    pub metadata: Value,
}
impl PageIR {
    pub fn sorted_blocks(&self) -> Vec<&Block> {
        let mut blocks: Vec<_> = self.blocks.iter().collect();
        blocks.sort_by_key(|b| b.order);
        blocks
    }
    pub fn page_text(&self) -> String {
        self.sorted_blocks()
            .into_iter()
            .filter(|b| !b.text.is_empty())
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocIR {
    pub doc_id: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub filename: String,
    pub mime: Option<String>,
    #[serde(default = "schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub pages: Vec<PageIR>,
    #[serde(default = "empty_metadata")]
    pub metadata: Value,
}
impl DocIR {
    pub fn page(&self, number: usize) -> Option<&PageIR> {
        self.pages.iter().find(|p| p.page_number == number)
    }
    pub fn iter_blocks(&self) -> Vec<&Block> {
        let mut pages: Vec<_> = self.pages.iter().collect();
        pages.sort_by_key(|p| p.page_number);
        pages.into_iter().flat_map(PageIR::sorted_blocks).collect()
    }
    pub fn stats(&self) -> Value {
        let blocks = self.iter_blocks();
        let mut by_type = std::collections::BTreeMap::<&str, usize>::new();
        let mut parsers = std::collections::BTreeSet::new();
        for block in &blocks {
            *by_type.entry(&block.kind).or_default() += 1;
            if !block.parser.is_empty() {
                parsers.insert(block.parser.as_str());
            }
        }
        for page in &self.pages {
            if !page.parser.is_empty() {
                parsers.insert(page.parser.as_str());
            }
        }
        json!({"pages":self.pages.len(),"blocks":blocks.len(),"by_type":by_type,"text_chars":blocks.iter().map(|b|b.text.chars().count()).sum::<usize>(),"tables":by_type.get("table").copied().unwrap_or(0),"formulas":by_type.get("formula").copied().unwrap_or(0),"figures":by_type.get("figure").copied().unwrap_or(0),"hard_pages":self.pages.iter().filter(|p|p.hard_page).count(),"parsers":parsers})
    }
    pub fn text(&self) -> Result<String> {
        let mut out = String::new();
        for p in &self.pages {
            for b in &p.blocks {
                if !b.text.is_empty() {
                    bounded_append(&mut out, &b.text, "\n\n")?
                }
            }
        }
        Ok(out)
    }
}
#[derive(Clone, Debug)]
pub struct ParserConfig {
    /// Explicit native executable paths supplied by the host; no PATH discovery.
    pub pdftotext: Option<PathBuf>,
    pub pdftoppm: Option<PathBuf>,
    pub tesseract: Option<PathBuf>,
    /// Optional alternative, never the canonical pypdf/PyMuPDF path.
    pub allow_pdftotext_alternative: bool,
    pub scratch_root: PathBuf,
    pub ocr_languages: String,
    pub ocr_dpi: u32,
    pub timeout: Duration,
}
impl ParserConfig {
    pub fn new(scratch_root: PathBuf) -> Self {
        Self {
            pdftotext: None,
            pdftoppm: None,
            tesseract: None,
            allow_pdftotext_alternative: false,
            scratch_root,
            ocr_languages: "eng+ara".into(),
            ocr_dpi: 200,
            timeout: Duration::from_secs(60),
        }
    }
    fn languages(&self) -> String {
        let allowed = [
            "eng", "ara", "chi_sim", "chi_tra", "fra", "deu", "spa", "rus", "jpn", "kor", "por",
            "ita", "nld", "tur", "vie", "tha",
        ];
        if !self.ocr_languages.is_empty()
            && self
                .ocr_languages
                .split('+')
                .all(|s| allowed.contains(&s.trim()))
        {
            self.ocr_languages.clone()
        } else {
            "eng+ara".into()
        }
    }
}
/// Native decoder boundary preserving the canonical pypdf-first/PyMuPDF-fallback
/// decision. Implementations must enforce the supplied decompressed-stream cap.
/// A decoder handle is local to one bounded worker, never shared across threads.
pub use ai_platform_service_primitives::pdf_ports::parser::{PdfTextDocument,PdfTextBackend};
fn collect_pdf_text(mut document: Box<dyn PdfTextDocument>) -> Result<String> {
    let count = document.page_count();
    if count > MAX_PAGES {
        return Err(IngestError::Validation(
            "PDF exceeds the 500 page limit".into(),
        ));
    }
    let mut out = String::new();
    for page in 0..count {
        let text = document.page_text(page)?;
        if text.chars().count() > 256_000 {
            return Err(IngestError::Validation(
                "PDF page text exceeds the parser output limit".into(),
            ));
        }
        bounded_append(&mut out, &text, "\n")?;
    }
    Ok(out)
}
#[derive(Clone)]
pub struct DocumentParser {
    pub config: ParserConfig,
    pub pdf_backend: Option<Arc<dyn PdfTextBackend>>,
}
impl DocumentParser {
    pub fn new(config: ParserConfig) -> Self {
        Self {
            config,
            pdf_backend: None,
        }
    }
    /// Configuration availability only; the host probes configured engines at
    /// startup before advertising PDF ingestion as ready.
    pub fn supports_pdf(&self) -> bool {
        self.pdf_backend.is_some()
            || (self.config.allow_pdftotext_alternative && self.config.pdftotext.is_some())
    }
    /// Reject an unavailable decoder before creating a document or storing its
    /// source. Detection matches extraction, including PDF bytes with a text name.
    pub fn require_supported_source(&self, bytes: &[u8], filename: &str, mime: &str) -> Result<()> {
        if (bytes.starts_with(b"%PDF")
            || filename.trim().to_lowercase().ends_with(".pdf")
            || mime.trim().to_lowercase().contains("application/pdf"))
            && !self.supports_pdf()
        {
            return Err(IngestError::Validation(
                "PDF ingestion is unavailable: an operator must explicitly configure a supported PDF backend; text, Markdown, HTML and DOCX remain available".into(),
            ));
        }
        Ok(())
    }
    pub fn extract_text_from_bytes(
        &self,
        bytes: &[u8],
        filename: &str,
        mime: &str,
    ) -> Result<(String, String)> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(bad(
                "document source exceeds the 16777216 byte parser limit",
            ));
        }
        let name = filename.trim().to_lowercase();
        let mime = mime.trim().to_lowercase();
        if bytes.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1")
            || name.ends_with(".doc")
            || mime.contains("application/msword")
        {
            return Err(bad(
                "Legacy DOC extraction is disabled; convert the document to DOCX or text",
            ));
        }
        if bytes.starts_with(b"%PDF") || name.ends_with(".pdf") || mime.contains("application/pdf")
        {
            return Ok((self.extract_pdf(bytes)?, "application/pdf".into()));
        }
        if bytes.starts_with(b"PK\x03\x04")
            || name.ends_with(".docx")
            || mime.contains("wordprocessingml.document")
        {
            return Ok((
                extract_docx(bytes)?,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
            ));
        }
        if mime.contains("text/html") || name.ends_with(".html") || name.ends_with(".htm") {
            if bytes.len() > MAX_HTML_BYTES {
                return Err(bad("HTML source exceeds the 4194304 byte parser limit"));
            }
            return Ok((
                extract_html(&decode_text_bytes(bytes)?)?,
                "text/html".into(),
            ));
        }
        let kind = if name.ends_with(".md")
            || ["text/markdown", "text/x-markdown"].contains(&mime.as_str())
        {
            "text/markdown"
        } else if mime.is_empty() {
            "text/plain"
        } else {
            &mime
        };
        Ok((decode_text_bytes(bytes)?, kind.into()))
    }
    pub fn parse(&self, doc_id: &str, bytes: &[u8], filename: &str, mime: &str) -> Result<DocIR> {
        let (text, detected) = self.extract_text_from_bytes(bytes, filename, mime)?;
        let page = PageIR {
            page_number: 1,
            blocks: vec![Block {
                block_id: "b1".into(),
                kind: "text".into(),
                text,
                order: 0,
                bbox: None,
                parser: "text_layer".into(),
                parser_version: "native-1".into(),
                confidence: None,
                table: None,
                formula: None,
                figure: None,
                metadata: json!({}),
            }],
            parser: "text_layer".into(),
            parser_version: "native-1".into(),
            page_width: None,
            page_height: None,
            confidence: None,
            hard_page: false,
            metadata: json!({}),
        };
        Ok(DocIR {
            doc_id: doc_id.into(),
            content_hash: sha256(bytes),
            filename: filename.into(),
            mime: Some(detected),
            schema_version: "1".into(),
            pages: vec![page],
            metadata: json!({}),
        })
    }
    fn extract_pdf(&self, bytes: &[u8]) -> Result<String> {
        preflight_pdf(bytes)?;
        if let Some(backend) = &self.pdf_backend {
            let first = backend
                .open_pypdf(bytes, 2 * 1024 * 1024)
                .and_then(collect_pdf_text);
            let text = match first {
                Ok(text) => text,
                Err(IngestError::Validation(message)) => {
                    return Err(IngestError::Validation(message));
                }
                Err(error) => backend
                    .open_pymupdf(bytes)
                    .and_then(collect_pdf_text)
                    .map_err(|fallback| {
                        bad(&format!(
                            "Failed to parse PDF: {error}; PyMuPDF fallback: {fallback}"
                        ))
                    })?,
            };
            if text.trim().is_empty() {
                return Err(bad("Failed to extract any text from PDF"));
            }
            return clean_pdf(&sanitize_text(&normalize_text(&text))?);
        }
        if !self.config.allow_pdftotext_alternative {
            return Err(bad(
                "canonical pypdf/PyMuPDF backend migration is pending; no substitute parser was selected",
            ));
        }
        let exe = self.config.pdftotext.as_ref().ok_or_else(|| {
            bad("native PDF extraction requires an explicitly configured pdftotext executable")
        })?;
        let scratch = Scratch::new(&self.config.scratch_root)?;
        let input = scratch.0.join("source.pdf");
        fs::write(&input, bytes)?;
        let args = vec![
            "-enc".into(),
            "UTF-8".into(),
            input.to_string_lossy().into_owned(),
            "-".into(),
        ];
        let text = run_bounded(exe, &args, self.config.timeout, MAX_TEXT_BYTES)?;
        let decoded =
            std::str::from_utf8(&text).map_err(|_| bad("PDF parser returned invalid UTF-8"))?;
        let pages: Vec<_> = decoded.split('\u{c}').collect();
        if pages.len().saturating_sub(1) > MAX_PAGES {
            return Err(bad("PDF exceeds the 500 page limit"));
        }
        if pages.iter().any(|p| p.chars().count() > 256_000) {
            return Err(bad("PDF page text exceeds the parser output limit"));
        }
        if decoded.trim().is_empty() {
            return Err(bad("Failed to extract any text from PDF"));
        }
        clean_pdf(&sanitize_text(&normalize_text(decoded))?)
    }
    /// Canonical bounded text-only API intentionally disables legacy whole-PDF OCR.
    pub fn ocr_pdf_bytes(&self, _bytes: &[u8]) -> Result<String> {
        Err(IngestError::Validation(
            "OCR text extraction is disabled in the bounded text-only pipeline".into(),
        ))
    }
    pub fn extract_pdf_with_pdfplumber(&self, _bytes: &[u8]) -> Result<String> {
        Err(IngestError::Validation(
            "pdfplumber table extraction is disabled in the bounded text-only pipeline".into(),
        ))
    }
    pub fn ocr_image_bytes(&self, bytes: &[u8], fallback_to_eng: bool) -> Result<String> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(bad("OCR image source exceeds the parser byte limit"));
        }
        let exe = self.config.tesseract.as_ref().ok_or_else(|| {
            bad("native OCR requires an explicitly configured tesseract executable")
        })?;
        let scratch = Scratch::new(&self.config.scratch_root)?;
        let input = scratch.0.join("image.png");
        fs::write(&input, bytes)?;
        let langs = self.config.languages();
        let mut attempts = vec![langs.clone()];
        if fallback_to_eng && langs != "eng" {
            attempts.push("eng".into())
        }
        let mut last = bad("OCR did not extract text");
        for lang in attempts {
            let args = vec![
                input.to_string_lossy().into_owned(),
                "stdout".into(),
                "-l".into(),
                lang,
                "--dpi".into(),
                self.config.ocr_dpi.clamp(72, 1200).to_string(),
            ];
            match run_bounded(
                exe,
                &args,
                self.config.timeout.min(Duration::from_secs(300)),
                MAX_TEXT_BYTES,
            ) {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out);
                    if !text.trim().is_empty() {
                        return sanitize_text(text.trim());
                    }
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let p = root.join(format!("knowledge-parser-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p)?;
        Ok(Self(p))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// Drain stdout concurrently with the process deadline; a limit kills and waits
/// for the child before returning. Stderr is deliberately not retained.
fn run_bounded(exe: &Path, args: &[String], timeout: Duration, cap: usize) -> Result<Vec<u8>> {
    if !exe.is_absolute() {
        return Err(bad(
            "parser executable must be an absolute host-configured path",
        ));
    }
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| bad("native parser could not start"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| bad("native parser stdout unavailable"))?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let mark = exceeded.clone();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out.len().saturating_add(n) > cap {
                        mark.store(true, Ordering::SeqCst);
                        break;
                    }
                    out.extend_from_slice(&buf[..n]);
                }
                Err(_) => return None,
            }
        }
        Some(out)
    });
    let started = Instant::now();
    let mut failure = None;
    let mut success = false;
    loop {
        if exceeded.load(Ordering::SeqCst) {
            failure = Some("native parser output exceeds the byte limit");
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                success = status.success();
                break;
            }
            Ok(None) => {}
            Err(_) => {
                failure = Some("native parser process failed");
                break;
            }
        }
        if started.elapsed() > timeout {
            failure = Some("native parser timed out");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if failure.is_some() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let out = reader
        .join()
        .ok()
        .flatten()
        .ok_or_else(|| bad("native parser output could not be read"))?;
    if let Some(m) = failure {
        return Err(bad(m));
    }
    if exceeded.load(Ordering::SeqCst) {
        return Err(bad("native parser output exceeds the byte limit"));
    }
    if !success {
        return Err(bad("native parser rejected the document"));
    }
    Ok(out)
}
fn preflight_pdf(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(bad("PDF source exceeds the 16777216 byte parser limit"));
    }
    let source = String::from_utf8_lossy(bytes);
    if regex::Regex::new(r"/Type\s*/Page\b")
        .unwrap()
        .find_iter(&source)
        .count()
        > MAX_PAGES
    {
        return Err(bad("PDF exceeds the 500 page limit"));
    }
    if regex::Regex::new(r"\b\d+\s+\d+\s+obj\b")
        .unwrap()
        .find_iter(&source)
        .count()
        > 100_000
    {
        return Err(bad("PDF exceeds the object marker limit"));
    }
    Ok(())
}
pub fn clean_pdf(text: &str) -> Result<String> {
    let page_number = regex::Regex::new(r"^[\d.\s]+$").unwrap();
    let duplicated = regex::Regex::new(r"^\d+(\.\d+)*\s+\d+(\.\d+)*").unwrap();
    let clean = regex::Regex::new(r"\.{3,}|(?:\.\s+){2,}|·{2,}|…+|\s{2,}").unwrap();
    let mut out = String::new();
    for raw in text.lines() {
        if raw.chars().count() > 32_000 {
            return Err(bad("PDF line exceeds the 32000 character limit"));
        }
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let prefix = line.trim_end_matches(char::is_numeric).trim_end();
        let suffix = prefix.len() != line.len();
        let dots = line
            .chars()
            .filter(|c| matches!(c, '.' | '·' | '…'))
            .count();
        if suffix
            && (prefix.contains("..")
                || prefix.contains("··")
                || prefix.contains('…')
                || prefix.matches('.').count() >= 2)
        {
            continue;
        }
        let lead = line.trim_start_matches(['.', '·', '…', ' ']);
        if lead != line && lead.chars().next().is_some_and(char::is_numeric) {
            continue;
        }
        if line.chars().count() > 5
            && dots > 3
            && (dots as f64 / line.chars().count() as f64) > 0.15
        {
            continue;
        }
        if (line.chars().count() < 10 && page_number.is_match(line)) || duplicated.is_match(line) {
            continue;
        }
        let clean = clean.replace_all(line, " ");
        let clean = clean.trim();
        if clean.chars().count() > 2 {
            bounded_append(&mut out, clean, "\n")?
        }
    }
    Ok(out)
}

fn le16(bytes: &[u8], offset: usize) -> Result<u16> {
    let s = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| bad("DOCX archive is truncated"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn le32(bytes: &[u8], offset: usize) -> Result<u32> {
    let s = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| bad("DOCX archive is truncated"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
pub(super) fn preflight_docx(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(bad("DOCX source exceeds the 16777216 byte parser limit"));
    }
    let start = bytes.len().saturating_sub(22 + 65_535);
    let eocd = (start..bytes.len().saturating_sub(3))
        .rev()
        .find(|i| bytes.get(*i..*i + 4) == Some(b"PK\x05\x06"))
        .ok_or_else(|| bad("DOCX archive is missing a valid central directory"))?;
    let entries = le16(bytes, eocd + 10)? as usize;
    let size = le32(bytes, eocd + 12)? as usize;
    let offset = le32(bytes, eocd + 16)? as usize;
    if eocd + 22 + le16(bytes, eocd + 20)? as usize != bytes.len() {
        return Err(bad("DOCX archive has an invalid central directory trailer"));
    }
    if le16(bytes, eocd + 4)? != 0
        || le16(bytes, eocd + 6)? != 0
        || le16(bytes, eocd + 8)? as usize != entries
    {
        return Err(bad("Multi-disk DOCX archives are not supported"));
    }
    if entries == 65535 || size == u32::MAX as usize || offset == u32::MAX as usize {
        return Err(bad("ZIP64 DOCX archives are not supported"));
    }
    if entries > 4096 || offset.checked_add(size) != Some(eocd) {
        return Err(bad(
            "DOCX archive central directory is invalid or oversized",
        ));
    }
    let mut at = offset;
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut count = 0;
    while at < eocd {
        if bytes.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(bad("DOCX central directory record signature is invalid"));
        }
        if le16(bytes, at + 8)? & 1 != 0 {
            return Err(bad("Encrypted DOCX archive entries are not supported"));
        }
        let compressed = le32(bytes, at + 20)?;
        let uncompressed = le32(bytes, at + 24)?;
        let name_len = le16(bytes, at + 28)? as usize;
        let extra_len = le16(bytes, at + 30)? as usize;
        let comment_len = le16(bytes, at + 32)? as usize;
        if le16(bytes, at + 34)? != 0 {
            return Err(bad("Multi-disk DOCX entries are not supported"));
        }
        let local = le32(bytes, at + 42)?;
        if compressed == u32::MAX || uncompressed == u32::MAX || local == u32::MAX {
            return Err(bad("ZIP64 DOCX entries are not supported"));
        }
        let end = at + 46 + name_len + extra_len + comment_len;
        if end > eocd || name_len == 0 || local as usize >= offset {
            return Err(bad("DOCX central directory record exceeds its bounds"));
        }
        let name = &bytes[at + 46..at + 46 + name_len];
        if !seen.insert(name.to_vec()) {
            return Err(bad("DOCX archive contains duplicate entries"));
        }
        if uncompressed as usize > MAX_SOURCE_BYTES {
            return Err(bad(
                "DOCX archive entry exceeds the uncompressed byte limit",
            ));
        }
        total += uncompressed as u64;
        if total > MAX_TEXT_BYTES as u64 {
            return Err(bad(
                "DOCX archive exceeds the total uncompressed byte limit",
            ));
        }
        if uncompressed > 0 && (compressed == 0 || uncompressed as f64 / compressed as f64 > 100.0)
        {
            return Err(bad(
                "DOCX archive entry exceeds the compression ratio limit",
            ));
        }
        let mut extra = at + 46 + name_len;
        let extra_end = extra + extra_len;
        while extra < extra_end {
            if extra + 4 > extra_end {
                return Err(bad("DOCX central directory extra field is malformed"));
            }
            let id = le16(bytes, extra)?;
            let len = le16(bytes, extra + 2)? as usize;
            extra += 4 + len;
            if id == 1 || extra > extra_end {
                return Err(bad("DOCX ZIP64 or malformed extra field is unsupported"));
            }
        }
        count += 1;
        if count > 4096 {
            return Err(bad("DOCX archive exceeds the 4096 entry limit"));
        }
        at = end;
    }
    if count != entries {
        return Err(bad("DOCX central directory entry count is inconsistent"));
    }
    Ok(())
}
#[derive(Default)]
struct XmlBudget {
    elements: usize,
    attributes: usize,
    paragraphs: usize,
    tables: usize,
    rows: usize,
    cells: usize,
}
fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}
fn check_xml(bytes: &[u8], budget: &mut XmlBudget) -> Result<()> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut depth = 0usize;
    loop {
        let event = reader
            .read_event()
            .map_err(|_| bad("DOCX XML is malformed"))?;
        let is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::DocType(_) => {
                return Err(bad("DOCX XML document type declarations are not supported"));
            }
            Event::Start(e) | Event::Empty(e) => {
                budget.elements += 1;
                budget.attributes += e.attributes().count();
                let local = local_name(e.name().as_ref()).to_vec();
                match local.as_slice() {
                    b"p" => budget.paragraphs += 1,
                    b"tbl" => budget.tables += 1,
                    b"tr" => budget.rows += 1,
                    b"tc" => budget.cells += 1,
                    _ => {}
                }
                // Empty elements do not remain on the stack. Their own depth is still charged.
                if !is_empty {
                    depth += 1;
                }
                if depth > 128
                    || budget.elements > 100_000
                    || budget.attributes > 200_000
                    || budget.paragraphs > 25_000
                    || budget.tables > 2000
                    || budget.rows > 25_000
                    || budget.cells > 50_000
                {
                    return Err(bad("DOCX XML exceeds the parser structure budget"));
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}
pub fn extract_docx(bytes: &[u8]) -> Result<String> {
    preflight_docx(bytes)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| bad("DOCX is not a valid OOXML ZIP archive"))?;
    let mut xml_budget = XmlBudget::default();
    let mut document = None;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|_| bad("DOCX ZIP entry could not be opened"))?;
        let name = file.name().to_owned();
        if !name.ends_with(".xml") && !name.ends_with(".rels") {
            continue;
        }
        let mut data = Vec::new();
        file.by_ref()
            .take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut data)?;
        if data.len() > MAX_SOURCE_BYTES {
            return Err(bad(
                "DOCX archive entry exceeds the uncompressed byte limit",
            ));
        }
        check_xml(&data, &mut xml_budget)?;
        if name == "word/document.xml" {
            document = Some(data)
        }
    }
    let data = document.ok_or_else(|| bad("DOCX archive has no word/document.xml"))?;
    super::docx::extract_document_xml(&data)
}

pub fn extract_html(html: &str) -> Result<String> {
    if html.len() > MAX_HTML_BYTES {
        return Err(bad("HTML source exceeds the 4194304 byte parser limit"));
    }
    // Preflight before the HTML DOM is materialized, including malformed markup.
    let tags = regex::Regex::new(r"(?s)<\s*([/]?)([A-Za-z][A-Za-z0-9:-]*)([^>]*?)>").unwrap();
    let attrs = regex::Regex::new(r#"[^\s=/'">]+\s*="#).unwrap();
    let mut count = 0;
    let mut depth = 0usize;
    let mut attribute_count = 0;
    let mut attribute_chars = 0;
    let void = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    for cap in tags.captures_iter(html) {
        count += 1;
        let name = cap[2].to_ascii_lowercase();
        if &cap[1] == "/" {
            depth = depth.saturating_sub(1)
        } else {
            attribute_count += attrs.find_iter(&cap[3]).count();
            attribute_chars += cap[3].len();
            if !void.contains(&name.as_str()) && !cap[3].trim_end().ends_with('/') {
                depth += 1
            }
        }
        if count > 50_000 || depth > 256 || attribute_count > 200_000 || attribute_chars > 4_000_000
        {
            return Err(bad("HTML exceeds the parser structure budget"));
        }
    }
    let doc = scraper::Html::parse_document(html);
    let mut selected = None;
    for selector in [
        "main",
        "article",
        "[role='main']",
        ".content",
        "#content",
        ".post",
        ".article",
        "body",
    ] {
        let selector = scraper::Selector::parse(selector)
            .map_err(|_| bad("invalid built-in HTML selector"))?;
        if let Some(el) = doc.select(&selector).next() {
            selected = Some(el);
            break;
        }
    }
    let root = selected.unwrap_or_else(|| doc.root_element());
    let mut out = String::new();
    if let Some(title) = doc
        .select(&scraper::Selector::parse("title").unwrap())
        .next()
    {
        let text = title.text().collect::<String>();
        if !text.trim().is_empty() {
            bounded_append(&mut out, &format!("# {}", text.trim()), "\n\n")?
        }
    }
    let ignored = [
        "script", "style", "noscript", "header", "footer", "nav", "aside", "iframe", "form",
    ];
    for node in root.descendants() {
        let Some(el) = scraper::ElementRef::wrap(node) else {
            continue;
        };
        let name = el.value().name();
        if el
            .ancestors()
            .take_while(|n| n.id() != root.id())
            .filter_map(scraper::ElementRef::wrap)
            .any(|p| ignored.contains(&p.value().name()))
            || ignored.contains(&name)
        {
            continue;
        }
        if name == "tr" {
            let cells: Vec<String> = el
                .children()
                .filter_map(scraper::ElementRef::wrap)
                .filter(|e| ["td", "th"].contains(&e.value().name()))
                .map(|e| {
                    e.text()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            if cells.iter().any(|s| !s.is_empty()) {
                bounded_append(
                    &mut out,
                    &cells
                        .iter()
                        .map(|s| if s.is_empty() { "-" } else { s.as_str() })
                        .collect::<Vec<_>>()
                        .join(" | "),
                    "\n",
                )?
            }
            continue;
        }
        if ![
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "p",
            "li",
            "blockquote",
            "pre",
            "code",
        ]
        .contains(&name)
        {
            continue;
        }
        let text = el
            .text()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            continue;
        }
        let value = if name.starts_with('h') && name.len() == 2 {
            format!(
                "{} {}",
                "#".repeat(name.as_bytes()[1].saturating_sub(b'0') as usize),
                text
            )
        } else if name == "li" {
            format!("• {text}")
        } else {
            text
        };
        bounded_append(&mut out, &value, "\n\n")?;
    }
    if out.is_empty() {
        for text in root.text().map(str::trim).filter(|s| !s.is_empty()) {
            bounded_append(&mut out, text, "\n")?
        }
    }
    sanitize_text(&normalize_text(&out))
}
