//! Source image extraction, scoped durable receipts and real batched image vectors.
use super::store::DocumentLease;
use super::{
    Chunk, Dataset, IngestError, IngestionService, KnowledgeWriteStore, Result,
    assets::AssetStorage, integer, object, sha256, string,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    sync::Arc,
    time::Duration,
};
pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone)]
pub struct ExtractedImage {
    pub image_id: String,
    pub content: Vec<u8>,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub source_location: String,
    pub context_text: String,
    pub alt_text: String,
    pub filename: String,
    pub char_offset: i64,
    pub metadata: Value,
}
impl ExtractedImage {
    pub fn embeddable(&self) -> bool {
        [
            "image/png",
            "image/jpeg",
            "image/jpg",
            "image/bmp",
            "image/webp",
            "image/gif",
            "image/svg+xml",
            "image/avif",
        ]
        .contains(&self.mime_type.as_str())
            && self.content.len() <= MAX_IMAGE_BYTES
            && self.width >= 50
            && self.height >= 50
    }
    pub fn to_value(&self, context: usize) -> Value {
        json!({"image_id":self.image_id,"mime_type":self.mime_type,"width":self.width,"height":self.height,"source_location":self.source_location,"size_bytes":self.content.len(),"context_text":self.context_text.chars().take(context).collect::<String>(),"alt_text":self.alt_text,"filename":self.filename,"char_offset":self.char_offset,"is_embeddable":self.embeddable(),"aspect_ratio":if self.height==0{0.0}else{((self.width as f64/self.height as f64)*100.0).round()/100.0},"metadata":self.metadata})
    }
}
pub struct Extraction {
    pub text: String,
    pub images: Vec<ExtractedImage>,
    pub document_type: String,
    pub metadata: Value,
}
impl Extraction {
    pub fn associations(&self, chunks: &[Value], distance: i64) -> BTreeMap<String, Vec<Value>> {
        let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for image in self
            .images
            .iter()
            .filter(|i| i.embeddable() && i.char_offset >= 0)
        {
            for chunk in chunks {
                let start = chunk["start"].as_i64().unwrap_or(0);
                let end = chunk["end"]
                    .as_i64()
                    .unwrap_or(self.text.chars().count() as i64);
                if start - distance <= image.char_offset && image.char_offset <= end + distance {
                    out.entry(
                        chunk["chunk_id"]
                            .as_str()
                            .or(chunk["id"].as_str())
                            .unwrap_or("")
                            .into(),
                    )
                    .or_default()
                    .push(image.to_value(200));
                }
            }
        }
        out
    }
}
pub struct PdfImage {
    pub bytes: Vec<u8>,
    pub extension: String,
    pub width: u32,
    pub height: u32,
    pub xref: i64,
}
/// Matching PyMuPDF engine boundary: each page call opens its own document handle.
pub trait PdfImageBackend: Send + Sync {
    fn page_texts(&self, bytes: &[u8]) -> Result<Vec<String>>;
    fn page_images(&self, bytes: &[u8], zero_based_page: usize) -> Result<Vec<PdfImage>>;
}
#[derive(Clone)]
pub struct DocumentImageExtractor {
    pub pdf: Option<Arc<dyn PdfImageBackend>>,
    pub safe_fetch: Option<Arc<dyn crate::migration_content::images::ImageReferenceFetch>>,
    pub download_images: bool,
    pub min_width: u32,
    pub min_height: u32,
    pub context_chars: usize,
}
impl Default for DocumentImageExtractor {
    fn default() -> Self {
        Self {
            pdf: None,
            safe_fetch: None,
            download_images: false,
            min_width: 50,
            min_height: 50,
            context_chars: 500,
        }
    }
}
pub fn detect_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        "image/tiff"
    } else if String::from_utf8_lossy(&bytes[..bytes.len().min(1024)])
        .to_lowercase()
        .contains("<svg")
        || bytes[..bytes.len().min(100)]
            .windows(5)
            .any(|s| s == b"<?xml")
    {
        "image/svg+xml"
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && bytes[8..bytes.len().min(16)]
            .windows(4)
            .any(|s| s == b"avif" || s == b"avis")
    {
        "image/avif"
    } else if bytes.starts_with(b"\0\0\x01\0") {
        "image/x-icon"
    } else {
        "application/octet-stream"
    }
}
pub fn dimensions(bytes: &[u8], mime: &str) -> (u32, u32) {
    if let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
        if let Ok(size) = reader.into_dimensions() {
            return size;
        }
    }
    match mime {
        "image/png" if bytes.len() >= 24 => (
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
        ),
        "image/gif" if bytes.len() >= 10 => (
            u16::from_le_bytes(bytes[6..8].try_into().unwrap()) as u32,
            u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as u32,
        ),
        "image/bmp" if bytes.len() >= 26 => (
            u32::from_le_bytes(bytes[18..22].try_into().unwrap()),
            i32::from_le_bytes(bytes[22..26].try_into().unwrap()).unsigned_abs(),
        ),
        _ => (0, 0),
    }
}
fn image(bytes: Vec<u8>, mime: String, width: u32, height: u32) -> ExtractedImage {
    ExtractedImage {
        image_id: sha256(&bytes)[..16].into(),
        content: bytes,
        mime_type: mime,
        width,
        height,
        source_location: String::new(),
        context_text: String::new(),
        alt_text: String::new(),
        filename: String::new(),
        char_offset: -1,
        metadata: json!({}),
    }
}
fn read_entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str, max: usize) -> Result<Vec<u8>> {
    let mut entry = zip
        .by_name(name)
        .map_err(|_| IngestError::Parser("DOCX image relationship target missing".into()))?;
    if entry.size() > max as u64 {
        return Err(IngestError::Validation(
            "DOCX image part exceeds budget".into(),
        ));
    }
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(IngestError::Validation(
            "DOCX image part exceeds budget".into(),
        ));
    }
    Ok(bytes)
}
impl DocumentImageExtractor {
    pub async fn extract(
        &self,
        filename: &str,
        bytes: &[u8],
        base_url: Option<&str>,
    ) -> Result<Extraction> {
        if bytes.is_empty() || bytes.len() > 100 * 1024 * 1024 {
            return Err(IngestError::Validation(
                "document image extraction byte budget exceeded".into(),
            ));
        }
        let ext = filename
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = if ext == "pdf" || bytes.starts_with(b"%PDF") {
            "pdf"
        } else if ext == "docx"
            || (bytes.starts_with(b"PK\x03\x04")
                && bytes[..bytes.len().min(2000)]
                    .windows(5)
                    .any(|s| s == b"word/"))
        {
            "docx"
        } else if ["html", "htm"].contains(&ext.as_str())
            || String::from_utf8_lossy(&bytes[..bytes.len().min(1000)])
                .to_lowercase()
                .contains("<html")
        {
            "html"
        } else {
            "unknown"
        };
        match kind {
            "pdf" => self.pdf_extract(bytes).await,
            "docx" => self.docx_extract(bytes),
            "html" => self.html_extract(bytes, base_url).await,
            _ => Err(IngestError::Parser(format!(
                "Unsupported document type: {kind}"
            ))),
        }
    }
    pub async fn sample_pdf(
        &self,
        bytes: &[u8],
    ) -> Result<(usize, Vec<super::detection::PageSample>)> {
        let backend = self.pdf.clone().ok_or_else(|| {
            IngestError::Parser("PDF detection requires a PyMuPDF backend".into())
        })?;
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let pages = backend.page_texts(&bytes)?;
            let total = pages.len();
            let mut samples = vec![];
            for i in super::detection::DocumentTypeDetector::sample_indices(total, 5) {
                samples.push(super::detection::PageSample {
                    page_index: i,
                    text: pages[i].clone(),
                    image_count: backend.page_images(&bytes, i)?.len(),
                });
            }
            Ok((total, samples))
        })
        .await
        .map_err(|_| IngestError::Parser("PDF detection task failed".into()))?
    }
    async fn pdf_extract(&self, bytes: &[u8]) -> Result<Extraction> {
        let backend = self.pdf.clone().ok_or_else(|| {
            IngestError::Parser(
                "canonical PyMuPDF PDF image extraction backend is unavailable".into(),
            )
        })?;
        let source = Arc::new(bytes.to_vec());
        let data = source.clone();
        let reader = backend.clone();
        let pages = tokio::task::spawn_blocking(move || reader.page_texts(&data))
            .await
            .map_err(|_| IngestError::Parser("PDF image text task failed".into()))??;
        let mut running = 0;
        let offsets: Vec<_> = pages
            .iter()
            .map(|s| {
                let start = running;
                running += s.chars().count() + 2;
                start
            })
            .collect();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(if pages.len() < 3 {
            1
        } else {
            4
        }));
        let mut tasks = tokio::task::JoinSet::new();
        for page in 0..pages.len() {
            let sem = semaphore.clone();
            let data = source.clone();
            let reader = backend.clone();
            tasks.spawn(async move {
                let permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|_| IngestError::Cancelled)?;
                let result = tokio::time::timeout(
                    Duration::from_secs(60),
                    tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        reader.page_images(&data, page)
                    }),
                )
                .await
                .map_err(|_| IngestError::Parser("PDF page image extraction timed out".into()))?
                .map_err(|_| IngestError::Parser("PDF page image task failed".into()))?;
                Ok::<_, IngestError>((page, result?))
            });
        }
        let mut images = vec![];
        let mut seen = BTreeSet::new();
        let mut errors = 0;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((page, items))) => {
                    for (index, item) in items.into_iter().enumerate() {
                        if !seen.insert(sha256(&item.bytes))
                            || item.width < self.min_width
                            || item.height < self.min_height
                        {
                            continue;
                        }
                        let mime = match item.extension.as_str() {
                            "jpg" | "jpeg" | "jpe" => "image/jpeg",
                            "bmp" => "image/bmp",
                            "webp" => "image/webp",
                            "gif" => "image/gif",
                            "tif" | "tiff" => "image/tiff",
                            "svg" => "image/svg+xml",
                            "avif" => "image/avif",
                            _ => "image/png",
                        };
                        let mut img = image(item.bytes, mime.into(), item.width, item.height);
                        img.source_location = format!("page_{}", page + 1);
                        img.context_text = pages[page].chars().take(self.context_chars).collect();
                        img.char_offset = offsets[page] as i64;
                        img.metadata =
                            json!({"page_number":page+1,"image_index":index,"xref":item.xref});
                        images.push(img)
                    }
                }
                _ => errors += 1,
            }
        }
        Ok(Extraction {
            text: pages.join("\n\n"),
            metadata: json!({"page_count":pages.len(),"total_images_found":images.len(),"extraction_errors":errors}),
            images,
            document_type: "pdf".into(),
        })
    }
    fn docx_extract(&self, bytes: &[u8]) -> Result<Extraction> {
        super::parser::preflight_docx(bytes)?;
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| IngestError::Parser("invalid DOCX archive".into()))?;
        let data = read_entry(
            &mut zip,
            "word/document.xml",
            super::parser::MAX_SOURCE_BYTES,
        )?;
        let mut reader = quick_xml::Reader::from_reader(data.as_slice());
        let mut paragraphs = vec![];
        let mut current = String::new();
        let mut in_text = false;
        let mut tables = 0usize;
        use quick_xml::events::Event;
        loop {
            match reader
                .read_event()
                .map_err(|_| IngestError::Parser("invalid DOCX XML".into()))?
            {
                Event::Start(e) => {
                    let n = e.local_name();
                    match n.as_ref() {
                        b"tbl" => tables += 1,
                        b"t" => in_text = true,
                        b"p" if tables == 0 => current.clear(),
                        _ => {}
                    }
                }
                Event::End(e) => {
                    let n = e.local_name();
                    match n.as_ref() {
                        b"tbl" => tables = tables.saturating_sub(1),
                        b"t" => in_text = false,
                        b"p" if tables == 0 => paragraphs.push(current.clone()),
                        _ => {}
                    }
                }
                Event::Text(t) if in_text && tables == 0 => current.push_str(
                    &t.decode()
                        .map_err(|_| IngestError::Parser("invalid DOCX text".into()))?,
                ),
                Event::Empty(e) if tables == 0 => match e.local_name().as_ref() {
                    b"tab" => current.push('\t'),
                    b"br" | b"cr" => current.push('\n'),
                    _ => {}
                },
                Event::GeneralRef(t) if in_text && tables == 0 => {
                    if let Ok(Some(c)) = t.resolve_char_ref() {
                        current.push(c)
                    } else {
                        let name = t.decode().unwrap_or_default();
                        current.push_str(match name.as_ref() {
                            "amp" => "&",
                            "lt" => "<",
                            "gt" => ">",
                            "quot" => "\"",
                            "apos" => "'",
                            _ => "",
                        })
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        let rels = read_entry(
            &mut zip,
            "word/_rels/document.xml.rels",
            super::parser::MAX_SOURCE_BYTES,
        )?;
        let mut reader = quick_xml::Reader::from_reader(rels.as_slice());
        let mut specs = vec![];
        loop {
            match reader
                .read_event()
                .map_err(|_| IngestError::Parser("invalid DOCX relationships".into()))?
            {
                Event::Start(e) | Event::Empty(e) => {
                    let mut attrs = BTreeMap::new();
                    for a in e.attributes().flatten() {
                        attrs.insert(
                            String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                            a.decode_and_unescape_value(reader.decoder())
                                .map_err(|_| {
                                    IngestError::Parser("invalid DOCX relationship".into())
                                })?
                                .into_owned(),
                        );
                    }
                    if attrs.get("Type").is_some_and(|s| s.contains("image")) {
                        specs.push(attrs)
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        let mut seen = BTreeSet::new();
        let mut images = vec![];
        let mut errors = 0;
        for rel in specs {
            let target = rel.get("Target").map(String::as_str).unwrap_or("");
            let path = if target.starts_with('/') {
                target.trim_start_matches('/').to_owned()
            } else {
                format!("word/{target}")
            };
            if path.split('/').any(|p| p == "..")
                || rel.get("TargetMode").is_some_and(|v| v == "External")
            {
                errors += 1;
                continue;
            }
            match read_entry(&mut zip, &path, super::parser::MAX_SOURCE_BYTES) {
                Ok(data) => {
                    if !seen.insert(sha256(&data)) {
                        continue;
                    }
                    let mime = detect_mime(&data);
                    let (w, h) = dimensions(&data, mime);
                    if w < self.min_width || h < self.min_height {
                        continue;
                    }
                    let mut img = image(data, mime.into(), w, h);
                    img.filename = path.rsplit('/').next().unwrap_or("").into();
                    img.source_location = "document_body".into();
                    img.metadata = json!({"relationship_id":rel.get("Id")});
                    images.push(img)
                }
                Err(_) => errors += 1,
            }
        }
        Ok(Extraction {
            text: paragraphs.join("\n"),
            metadata: json!({"paragraph_count":paragraphs.len(),"total_images_found":images.len(),"extraction_errors":errors}),
            images,
            document_type: "docx".into(),
        })
    }
    async fn html_extract(&self, bytes: &[u8], base_url: Option<&str>) -> Result<Extraction> {
        // Collect owned image specifications before awaiting: scraper nodes are not Send.
        let (text, specs) = {
            let decoded = String::from_utf8_lossy(bytes);
            let html = scraper::Html::parse_document(&decoded);
            let text = html
                .tree
                .nodes()
                .filter_map(|n| {
                    if n.ancestors()
                        .filter_map(scraper::ElementRef::wrap)
                        .any(|p| ["script", "style", "noscript"].contains(&p.value().name()))
                    {
                        None
                    } else {
                        n.value()
                            .as_text()
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let selector = scraper::Selector::parse("img").unwrap();
            let nodes: Vec<_> = html.tree.root().descendants().collect();
            let specs = html
                .select(&selector)
                .map(|e| {
                    let pos = nodes.iter().position(|n| n.id() == e.id()).unwrap_or(0);
                    let previous = nodes[..pos]
                        .iter()
                        .rev()
                        .find_map(|n| n.value().as_text())
                        .map(|t| {
                            t.trim()
                                .chars()
                                .take(self.context_chars / 2)
                                .collect::<String>()
                        });
                    let following = nodes[pos + 1..]
                        .iter()
                        .find_map(|n| n.value().as_text())
                        .map(|t| {
                            t.trim()
                                .chars()
                                .take(self.context_chars / 2)
                                .collect::<String>()
                        });
                    let context = previous
                        .into_iter()
                        .chain(following)
                        .collect::<Vec<_>>()
                        .join(" ");
                    (
                        e.value().attr("src").unwrap_or("").to_owned(),
                        e.value().attr("alt").unwrap_or("").to_owned(),
                        context,
                    )
                })
                .collect::<Vec<_>>();
            (text, specs)
        };
        let tags = specs.len();
        let mut images = vec![];
        let mut seen = BTreeSet::new();
        let mut errors = 0;
        for (src, alt, context) in specs {
            if src.is_empty() {
                continue;
            }
            let loaded = if src.starts_with("data:") {
                let pattern = regex::Regex::new(r"^data:([^;,]+)?(?:;base64)?,(.+)").unwrap();
                pattern.captures(&src).and_then(|c| {
                    STANDARD.decode(c.get(2)?.as_str()).ok().map(|b| {
                        (
                            b,
                            c.get(1)
                                .map(|m| m.as_str())
                                .unwrap_or("application/octet-stream")
                                .to_owned(),
                        )
                    })
                })
            } else if self.download_images {
                let url = if src.starts_with("http://") || src.starts_with("https://") {
                    Some(src.clone())
                } else {
                    base_url
                        .and_then(|b| reqwest::Url::parse(b).ok())
                        .and_then(|u| u.join(&src).ok())
                        .map(|u| u.to_string())
                };
                match (url, self.safe_fetch.as_ref()) {
                    (Some(url), Some(fetcher)) => fetcher
                        .fetch(&url, MAX_IMAGE_BYTES, Duration::from_secs(10))
                        .await
                        .ok()
                        .map(|f| {
                            let mime = detect_mime(&f.bytes).into();
                            (f.bytes, mime)
                        }),
                    _ => None,
                }
            } else {
                None
            };
            if let Some((data, mime)) = loaded {
                if !seen.insert(sha256(&data)) {
                    continue;
                }
                let (w, h) = dimensions(&data, &mime);
                if w < self.min_width || h < self.min_height {
                    continue;
                }
                let mut img = image(data, mime, w, h);
                img.alt_text = alt;
                img.context_text = context;
                img.source_location = if src.starts_with("data:") {
                    "inline_data_uri".into()
                } else {
                    src.clone()
                };
                if !src.starts_with("data:") {
                    img.filename = src
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .split('?')
                        .next()
                        .unwrap_or("")
                        .into()
                }
                images.push(img)
            } else if src.starts_with("data:") {
                errors += 1
            }
        }
        Ok(Extraction {
            text,
            metadata: json!({"total_img_tags":tags,"extracted_images":images.len(),"extraction_errors":errors}),
            images,
            document_type: "html".into(),
        })
    }
}
impl IngestionService {
    pub(crate) async fn persist_image_receipt(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        mode: &str,
        images: &[ExtractedImage],
    ) -> Result<Value> {
        let id = string(doc, "document_id");
        let metadata = object(&doc["metadata"]);
        let original = string(&metadata, "original_file_key");
        if original.is_empty() || dataset.tenant_id.trim().is_empty() {
            return Err(IngestError::Validation(
                "durable image receipt source identity missing".into(),
            ));
        }
        let sem = Arc::new(tokio::sync::Semaphore::new(5));
        let mut tasks = tokio::task::JoinSet::new();
        let mut keys = vec![];
        for (idx, img) in images.iter().enumerate() {
            let attachment = format!("reindex_{}", img.image_id);
            let filename = if img.filename.is_empty() {
                format!(
                    "image_{idx}.{}",
                    img.mime_type.split('/').next_back().unwrap_or("png")
                )
            } else {
                img.filename.clone()
            };
            let key = AssetStorage::image_key(&dataset.tenant_id, id, &attachment, &filename)?;
            keys.push(key.clone());
            let img = img.clone();
            let assets = self.assets.clone();
            let sem = sem.clone();
            tasks.spawn(async move{let _permit=sem.acquire_owned().await.map_err(|_|IngestError::Cancelled)?;let page=img.metadata.get("page_number").cloned().unwrap_or(Value::Null);let url=assets.backend.upload(&key,img.content.clone(),&img.mime_type,BTreeMap::from([("width".into(),img.width.to_string()),("height".into(),img.height.to_string()),("source_location".into(),img.source_location.clone()),("page_number".into(),page.as_i64().map(|n|n.to_string()).unwrap_or_else(||idx.to_string()))])).await.map_err(|_|IngestError::Storage)?;if url.trim().is_empty(){return Err(IngestError::Storage)}Ok::<_,IngestError>((idx,json!({"image_id":img.image_id,"storage_url":assets.sign_url_if_local(&url)?,"storage_key":key,"filename":filename,"mime_type":img.mime_type,"width":img.width,"height":img.height,"page_number":page,"size_bytes":img.content.len(),"context_text":img.context_text.chars().take(200).collect::<String>(),"source_location":img.source_location})))});
        }
        let mut receipts = BTreeMap::new();
        let mut failed = false;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((idx, receipt))) => {
                    receipts.insert(idx, receipt);
                }
                _ => failed = true,
            }
        }
        let ordered: Vec<_> = receipts.into_values().collect();
        let result:Result<()>=async{if failed||ordered.len()!=images.len(){return Err(IngestError::Storage)}let changed=sqlx::query("UPDATE knowledge.documents SET metadata=jsonb_set(jsonb_set(COALESCE(metadata,'{}'::jsonb),'{extracted_images}',$5::jsonb,TRUE),'{image_count}',to_jsonb(jsonb_array_length($5::jsonb)),TRUE),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2 AND status='parsing' AND metadata->>'original_file_key'=$3 AND LOWER(COALESCE(metadata->>'processing_mode','text_only'))=$4 AND ((COALESCE(enabled,TRUE)=TRUE AND COALESCE(archived,FALSE)=FALSE AND NOT(COALESCE(metadata,'{}'::jsonb)?'_document_lifecycle_reindex')) OR(metadata->'_document_lifecycle_reindex'->>'status'='pending' AND metadata->'_document_lifecycle_reindex'->>'desired_enabled'='true' AND metadata->'_document_lifecycle_reindex'->>'desired_archived'='false')) AND NOT(COALESCE(metadata,'{}'::jsonb)?'_document_upload_generation') AND NOT(COALESCE(metadata,'{}'::jsonb)?'_document_upload_failed') AND NOT(COALESCE(metadata,'{}'::jsonb)?'_confluence_sync_generation') RETURNING document_id").bind(id).bind(&dataset.dataset_id).bind(original).bind(mode).bind(json!(ordered)).fetch_optional(lease.connection()).await?;if changed.is_none(){return Err(IngestError::GenerationChanged)}Ok(())}.await;
        if let Err(error) = result {
            let mut cleanup_failed = false;
            for key in keys {
                if !matches!(self.assets.backend.delete(&key).await, Ok(true)) {
                    cleanup_failed = true
                }
            }
            return Err(if cleanup_failed {
                IngestError::Validation(
                    "image receipt publication failed and storage compensation was incomplete"
                        .into(),
                )
            } else {
                error
            });
        }
        let mut metadata = metadata;
        metadata["extracted_images"] = json!(ordered);
        metadata["image_count"] = json!(images.len());
        Ok(metadata)
    }
    /// Prepare every required image or reject the generation before vector mutation.
    pub(crate) async fn prepare_receipt_images(
        &self,
        dataset: &Dataset,
        document: &str,
        receipts: &[Value],
        old: &[Chunk],
        base_position: i32,
    ) -> Result<(Vec<Chunk>, BTreeMap<String, Vec<Value>>)> {
        if receipts.is_empty() {
            return Ok((vec![], BTreeMap::new()));
        }
        let resolved = self.indexer.embeddings.resolve(dataset).await?;
        if !["unified_multimodal", "dashscope_multimodal"].contains(&resolved.provider.as_str()) {
            return Err(IngestError::Validation(
                "image indexing requires the dataset's unified multimodal embedding space".into(),
            ));
        }
        let prefix = format!(
            "knowledge/confluence/{}/{document}/images/",
            dataset.tenant_id
        );
        let sem = Arc::new(tokio::sync::Semaphore::new(5));
        let mut tasks = tokio::task::JoinSet::new();
        for (idx, receipt) in receipts.iter().enumerate() {
            let key = string(receipt, "storage_key").to_owned();
            if string(receipt, "storage_url").is_empty()
                || !key.starts_with(&prefix)
                || key.split('/').any(|p| p == ".." || p == ".")
            {
                return Err(IngestError::Validation(
                    "durable image receipt violates document storage scope".into(),
                ));
            }
            let assets = self.assets.clone();
            let sem = sem.clone();
            let receipt = receipt.clone();
            tasks.spawn(async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|_| IngestError::Cancelled)?;
                for attempt in 0..3 {
                    match assets
                        .download_bounded(&key, super::parser::MAX_SOURCE_BYTES)
                        .await
                    {
                        Ok(bytes) if !bytes.is_empty() => {
                            return Ok::<_, IngestError>((idx, receipt, bytes));
                        }
                        Err(IngestError::Validation(e)) => return Err(IngestError::Validation(e)),
                        _ => {
                            if attempt < 2 {
                                tokio::time::sleep(Duration::from_secs(1 << attempt)).await
                            }
                        }
                    }
                }
                Err(IngestError::Storage)
            });
        }
        let mut loaded = BTreeMap::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(Ok((idx, meta, bytes))) = result {
                loaded.insert(idx, (meta, bytes));
            }
        }
        if loaded.len() != receipts.len() {
            return Err(IngestError::Validation("durable image receipt could not be fully loaded; refusing a partial image generation".into()));
        }
        let loaded: Vec<_> = loaded.into_iter().collect();
        let mut rows = vec![];
        let mut points = vec![];
        for batch in loaded.chunks(8) {
            let bytes: Vec<_> = batch.iter().map(|(_, (_, bytes))| bytes.clone()).collect();
            let mut vectors = None;
            for retry in 0..3 {
                match self.indexer.embeddings.embed_images(dataset, &bytes).await {
                    Ok(v) if v.len() == batch.len() => {
                        vectors = Some(v);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        if retry < 2 {
                            tokio::time::sleep(Duration::from_secs(1 << retry)).await
                        }
                    }
                }
            }
            let vectors = vectors.ok_or_else(|| {
                IngestError::Index("image embedding did not produce every required vector".into())
            })?;
            for ((idx, (meta, _)), vector) in batch.iter().zip(vectors) {
                if vector.is_empty()
                    || vector.len() != resolved.dimension
                    || vector.iter().any(|x| !x.is_finite())
                {
                    return Err(IngestError::Index(
                        "image vector dimensions or finite values are invalid".into(),
                    ));
                }
                let position = base_position + *idx as i32;
                let id = old
                    .iter()
                    .find(|s| s.content_type == "image" && s.position == position)
                    .map(|s| s.segment_id.clone())
                    .unwrap_or_else(|| super::stable_segment_id(document, "image", position));
                let text = meta["vlm_description"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| meta["context_text"].as_str().filter(|s| !s.is_empty()))
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        format!(
                            "[Image: page {}]",
                            meta.get("page_number")
                                .map(|v| if v.is_null() {
                                    "None".into()
                                } else {
                                    v.to_string()
                                })
                                .unwrap_or("unknown".into())
                        )
                    });
                let attachment = meta["confluence_attachment_id"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .or(meta["image_id"].as_str())
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        IngestError::Validation(
                            "durable image receipt has no attachment identity".into(),
                        )
                    })?;
                let filename = meta["filename"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        string(meta, "storage_key")
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .to_owned()
                    });
                let mut metadata = json!({"source_position":idx});
                for k in [
                    "width",
                    "height",
                    "page_number",
                    "source_location",
                    "storage_key",
                    "image_id",
                    "filename",
                    "context_text",
                    "vlm_description",
                    "confluence_attachment_id",
                    "attachment_updated_at",
                ] {
                    metadata[k] = meta[k].clone()
                }
                let disabled = old
                    .iter()
                    .find(|s| s.segment_id == id)
                    .is_some_and(|s| s.extra.get("disabled_by").is_some_and(|v| !v.is_null()));
                let row: Chunk = serde_json::from_value(
                    json!({"segment_id":id,"dataset_id":dataset.dataset_id,"document_id":document,"position":position,"text":text,"level":3,"token_count":0,"vector_id":id,"content_type":"image","image_url":meta["storage_url"],"image_attachment_id":attachment,"image_filename":filename,"image_media_type":meta["mime_type"],"image_file_size":meta["size_bytes"],"metadata":metadata,"enabled":!disabled,"status":"indexing"}),
                )?;
                let payload = json!({"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":id,"position":position,"text":text,"content_type":"image","image_id":meta["image_id"],"image_mime_type":meta["mime_type"],"image_width":meta["width"],"image_height":meta["height"],"image_page":meta["page_number"],"source_location":meta["source_location"],"confluence_attachment_id":meta["confluence_attachment_id"],"attachment_updated_at":meta["attachment_updated_at"],"enabled":!disabled});
                points.push(json!({"id":id,"vector":{"":vector},"payload":payload}));
                rows.push(row);
            }
        }
        let collection = dataset
            .collection_name
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or(IngestError::GenerationChanged)?;
        Ok((rows, BTreeMap::from([(collection, points)])))
    }
}
