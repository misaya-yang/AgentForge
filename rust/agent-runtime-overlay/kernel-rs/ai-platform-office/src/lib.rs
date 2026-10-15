//! Deterministic, bounded document generation for the platform Office tool.
//!
//! The crate intentionally has no model, network, process, or worker
//! integration.  A caller supplies the complete Markdown body and receives
//! validated bytes plus the semantic IR used to render it.

use std::io::Write;
use std::io::{Cursor, Read};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use docx_rs::{
    AbstractNumbering, Docx, IndentLevel, Level, LevelJc, LevelText, NumberFormat, Numbering,
    NumberingId, Paragraph, Pic, Run, RunFonts, SpecialIndentType, Start, Table as DocxTable,
    TableCell as DocxTableCell, TableRow as DocxTableRow,
};
use drawingml::{
    BulletKind, Geometry, PresetShape, ShapeProperties, TextBody, TextParagraph,
    TextParagraphProperties, TextRun, TextRunProperties, Transform2D,
};
use powerpoint_ooxml::{
    AutoShape, DocumentProperties, Picture, PictureFormat, Presentation, Shape, Slide, SlideTable,
    TableCell as PptxTableCell, TableRow as PptxTableRow, Theme,
};
use rust_xlsxwriter::{Format, Workbook};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use typst_as_lib::TypstEngine;
use zip::ZipArchive;

pub const MAX_TITLE_CHARS: usize = 300;
pub const MAX_GOAL_CHARS: usize = 2_000;
pub const MAX_BODY_CHARS: usize = 20_000;
pub const MAX_LOCALE_CHARS: usize = 16;
pub const MAX_BLOCKS: usize = 512;
pub const MAX_TABLE_COLUMNS: usize = 32;
pub const MAX_TABLE_ROWS: usize = 512;
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ARCHIVE_PARTS: usize = 2_048;
pub const MAX_ARCHIVE_UNCOMPRESSED_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RENDERED_BYTES: usize = 32 * 1024 * 1024;
const DOCX_BULLET_NUMBERING_ID: usize = 2;
const PPTX_FONT: &str = "Noto Sans CJK SC";
const PPTX_CODE_FONT: &str = "Noto Sans Mono CJK SC";
const MAX_PPTX_TABLE_COLUMNS: usize = 10;
const MAX_PPTX_TABLE_ROWS: usize = 12;
const MAX_PPTX_TEXT_BLOCK_CHARS: usize = 1_800;

/// The public tool's output format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    Docx,
    Pptx,
    Xlsx,
    Pdf,
}

impl DocumentFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Pptx => "pptx",
            Self::Xlsx => "xlsx",
            Self::Pdf => "pdf",
        }
    }

    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Pdf => "application/pdf",
        }
    }
}

/// The six built-in design systems exposed by the MCP resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesignSystem {
    Claude,
    Stripe,
    Carbon,
    Keynote,
    Editorial,
    Enterprise,
}

impl Default for DesignSystem {
    fn default() -> Self {
        Self::Enterprise
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesignPalette {
    pub primary: &'static str,
    pub accent: &'static str,
    pub background: &'static str,
    pub foreground: &'static str,
}

impl DesignSystem {
    pub const fn palette(self) -> DesignPalette {
        match self {
            Self::Claude => DesignPalette {
                primary: "#D97757",
                accent: "#F5E9E3",
                background: "#FFFCF8",
                foreground: "#1F2933",
            },
            Self::Stripe => DesignPalette {
                primary: "#635BFF",
                accent: "#E7E5FF",
                background: "#FFFFFF",
                foreground: "#1D1D1F",
            },
            Self::Carbon => DesignPalette {
                primary: "#0F62FE",
                accent: "#D0E2FF",
                background: "#F4F4F4",
                foreground: "#161616",
            },
            Self::Keynote => DesignPalette {
                primary: "#007AFF",
                accent: "#D9EEFF",
                background: "#FFFFFF",
                foreground: "#111111",
            },
            Self::Editorial => DesignPalette {
                primary: "#8B1E3F",
                accent: "#F2D9E1",
                background: "#FFFDF8",
                foreground: "#241F20",
            },
            Self::Enterprise => DesignPalette {
                primary: "#1F4B99",
                accent: "#DCE8FA",
                background: "#FFFFFF",
                foreground: "#1F2937",
            },
        }
    }
}

/// Strict wire input matching `GenerateDocumentInput`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateDocumentRequest {
    pub format: DocumentFormat,
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub body_markdown: Option<String>,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default)]
    pub design_system: Option<DesignSystem>,
    #[serde(default)]
    pub template_name: Option<String>,
}

fn default_locale() -> String {
    "en-US".to_owned()
}

/// A small semantic document IR.  Rendering never reparses the model body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIr {
    pub title: String,
    pub goal: String,
    pub locale: String,
    pub design_system: DesignSystem,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    BulletList {
        items: Vec<Vec<Inline>>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Quote {
        text: String,
    },
    Code {
        language: Option<String>,
        text: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Inline {
    Text { text: String },
    Strong { text: String },
    Emphasis { text: String },
    Code { text: String },
    Formula { expression: String },
    Image { alt: String, source: String },
}

/// A bounded, package-level preview. It is intentionally semantic rather than
/// a claim that an Office viewer rendered pixels correctly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentPreview {
    pub format: DocumentFormat,
    pub semantic_ir: DocumentIr,
    pub part_names: Vec<String>,
    pub unknown_parts: Vec<String>,
    pub visual_input: VisualInput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualInput {
    pub source_format: DocumentFormat,
    pub text_fragments: Vec<String>,
    pub image_part_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentGenerationResult {
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: usize,
    pub sha256: String,
    pub plan_outline: String,
    /// Rendering is structurally checked by this crate, but product-level
    /// visual/semantic verification belongs to the Worker and is not claimed
    /// by the core renderer.
    pub verifier_status: VerifierStatus,
    pub semantic_ir: DocumentIr,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierStatus {
    NotRun,
}

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("invalid document request: {0}")]
    InvalidRequest(String),
    #[error("unsupported markdown element: {0}")]
    UnsupportedMarkdown(String),
    #[error("document renderer error: {0}")]
    Renderer(String),
    #[error("artifact archive is invalid: {0}")]
    InvalidArchive(String),
}

/// An immutable, concurrency-safe core renderer. It returns bytes and semantic
/// metadata only; artifact storage and public URLs belong to the Worker.
#[derive(Clone, Copy, Debug, Default)]
pub struct DocumentGenerator;

impl DocumentGenerator {
    pub const fn new() -> Self {
        Self
    }

    pub fn generate(
        &self,
        request: &GenerateDocumentRequest,
    ) -> Result<DocumentGenerationResult, DocumentError> {
        self.generate_with_fonts(request, &[])
    }

    /// Render a document with caller-provided, already-authorized font bytes.
    /// No filesystem font discovery or network/package resolution is enabled.
    pub fn generate_with_fonts(
        &self,
        request: &GenerateDocumentRequest,
        fonts: &[&[u8]],
    ) -> Result<DocumentGenerationResult, DocumentError> {
        let ir = build_ir(request)?;
        let bytes = render(&ir, request.format, fonts)?;
        if bytes.len() > MAX_RENDERED_BYTES {
            return Err(DocumentError::InvalidRequest(
                "rendered document exceeds the bounded artifact size".to_owned(),
            ));
        }
        let digest = sha256_hex(&bytes);
        let filename = format!(
            "{}.{}",
            safe_filename(&request.title),
            request.format.extension()
        );
        Ok(DocumentGenerationResult {
            filename,
            mime_type: request.format.mime_type().to_owned(),
            size_bytes: bytes.len(),
            sha256: digest,
            plan_outline: plan_outline(&ir),
            verifier_status: VerifierStatus::NotRun,
            semantic_ir: ir,
            bytes,
        })
    }

    /// Safely edits the textual payload of an existing OOXML package. Parts
    /// outside the edited, well-known part are copied byte-for-byte. A PDF or
    /// package with an unknown target part is rejected instead of rebuilt.
    pub fn modify_existing(
        &self,
        format: DocumentFormat,
        source: &[u8],
        request: &GenerateDocumentRequest,
    ) -> Result<DocumentGenerationResult, DocumentError> {
        if request.format != format {
            return Err(DocumentError::InvalidRequest(
                "edit format does not match request format".to_owned(),
            ));
        }
        let ir = build_ir(request)?;
        let parts = package_parts(source)?;
        let mut replacements = std::collections::BTreeMap::new();
        match format {
            DocumentFormat::Docx => {
                let target = parts
                    .iter()
                    .find(|(name, _)| name == "word/document.xml")
                    .map(|(_, content)| content.as_slice())
                    .ok_or_else(|| {
                        DocumentError::InvalidArchive("DOCX document part missing".to_owned())
                    })?;
                let addition = docx_edit_payload(&ir);
                replacements.insert(
                    "word/document.xml".to_owned(),
                    append_before(target, "</w:body>", &addition)?,
                );
            }
            DocumentFormat::Xlsx => {
                let (name, target) = parts
                    .iter()
                    .find(|(name, _)| name.starts_with("xl/worksheets/") && name.ends_with(".xml"))
                    .ok_or_else(|| {
                        DocumentError::InvalidArchive("XLSX worksheet part missing".to_owned())
                    })?;
                let addition = xlsx_edit_payload(&ir, target);
                replacements.insert(
                    name.clone(),
                    append_before(target, "</sheetData>", &addition)?,
                );
            }
            DocumentFormat::Pptx => {
                let (name, target) = parts
                    .iter()
                    .rev()
                    .find(|(name, _)| {
                        name.starts_with("ppt/slides/slide") && name.ends_with(".xml")
                    })
                    .ok_or_else(|| {
                        DocumentError::InvalidArchive("PPTX slide part missing".to_owned())
                    })?;
                let addition = pptx_edit_payload(&ir, target);
                replacements.insert(
                    name.clone(),
                    append_before(target, "</p:spTree>", &addition)?,
                );
            }
            DocumentFormat::Pdf => {
                return Err(DocumentError::InvalidRequest(
                    "PDF editing is unavailable without a lossless PDF editor".to_owned(),
                ));
            }
        }
        let bytes = rewrite_package(source, &replacements)?;
        if bytes.len() > MAX_RENDERED_BYTES {
            return Err(DocumentError::InvalidRequest(
                "edited document exceeds the bounded artifact size".to_owned(),
            ));
        }
        Ok(document_result(ir, format, bytes))
    }

    /// Unpack known semantic text and package topology for a visual verifier.
    /// This does not invoke an Office application and makes no pixel-level
    /// acceptance claim.
    pub fn preview(
        &self,
        format: DocumentFormat,
        source: &[u8],
    ) -> Result<DocumentPreview, DocumentError> {
        unpack_preview(format, source)
    }
}

pub fn generate_document(
    request: &GenerateDocumentRequest,
) -> Result<DocumentGenerationResult, DocumentError> {
    DocumentGenerator::new().generate(request)
}

pub fn modify_document(
    format: DocumentFormat,
    source: &[u8],
    request: &GenerateDocumentRequest,
) -> Result<DocumentGenerationResult, DocumentError> {
    DocumentGenerator::new().modify_existing(format, source, request)
}

pub fn preview_document(
    format: DocumentFormat,
    source: &[u8],
) -> Result<DocumentPreview, DocumentError> {
    DocumentGenerator::new().preview(format, source)
}

fn document_result(
    ir: DocumentIr,
    format: DocumentFormat,
    bytes: Vec<u8>,
) -> DocumentGenerationResult {
    DocumentGenerationResult {
        filename: format!("{}.{}", safe_filename(&ir.title), format.extension()),
        mime_type: format.mime_type().to_owned(),
        size_bytes: bytes.len(),
        sha256: sha256_hex(&bytes),
        plan_outline: plan_outline(&ir),
        verifier_status: VerifierStatus::NotRun,
        semantic_ir: ir,
        bytes,
    }
}

fn build_ir(request: &GenerateDocumentRequest) -> Result<DocumentIr, DocumentError> {
    validate_request(request)?;
    let body = match request.body_markdown.as_deref() {
        Some(body) => body,
        None => "",
    };
    let blocks = parse_markdown(body)?;
    Ok(DocumentIr {
        title: request.title.clone(),
        goal: request.goal.clone(),
        locale: request.locale.clone(),
        design_system: match request.design_system {
            Some(design_system) => design_system,
            None => DesignSystem::default(),
        },
        blocks,
    })
}

fn validate_request(request: &GenerateDocumentRequest) -> Result<(), DocumentError> {
    validate_nonempty_bounded("title", &request.title, MAX_TITLE_CHARS)?;
    validate_nonempty_bounded("goal", &request.goal, MAX_GOAL_CHARS)?;
    if let Some(body) = &request.body_markdown {
        if body.chars().count() > MAX_BODY_CHARS {
            return Err(DocumentError::InvalidRequest(format!(
                "body_markdown exceeds {MAX_BODY_CHARS} characters"
            )));
        }
        if body.chars().any(disallowed_control) {
            return Err(DocumentError::InvalidRequest(
                "body_markdown contains a disallowed control character".to_owned(),
            ));
        }
    }
    if request.locale.chars().count() > MAX_LOCALE_CHARS || !valid_locale(&request.locale) {
        return Err(DocumentError::InvalidRequest(
            "locale must be a bounded BCP-47-like value".to_owned(),
        ));
    }
    if let Some(template) = &request.template_name {
        if template.is_empty() || template.chars().count() > 128 || has_control(template) {
            return Err(DocumentError::InvalidRequest(
                "template_name is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_nonempty_bounded(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), DocumentError> {
    if value.trim().is_empty() || value.chars().count() > max_chars || has_control(value) {
        return Err(DocumentError::InvalidRequest(format!(
            "{field} must be non-empty and at most {max_chars} characters"
        )));
    }
    Ok(())
}

fn valid_locale(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts.all(|part| {
        (2..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn disallowed_control(value: char) -> bool {
    value.is_control() && !matches!(value, '\n' | '\r' | '\t')
}

fn parse_markdown(markdown: &str) -> Result<Vec<Block>, DocumentError> {
    if markdown.trim().is_empty() {
        return Ok(Vec::new());
    }
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_end();
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if line.starts_with("```") {
            let language = line.trim_start_matches('`').trim();
            let language = (!language.is_empty()).then(|| language.to_owned());
            index += 1;
            let start = index;
            while index < lines.len() && !lines[index].trim_start().starts_with("```") {
                index += 1;
            }
            if index == lines.len() {
                return Err(DocumentError::UnsupportedMarkdown(
                    "unterminated code block".to_owned(),
                ));
            }
            blocks.push(Block::Code {
                language,
                text: lines[start..index].join("\n"),
            });
            index += 1;
        } else if let Some((level, text)) = heading(line) {
            blocks.push(Block::Heading {
                level,
                text: plain_text(text)?,
            });
            index += 1;
        } else if line.starts_with('>') {
            let mut quote = String::new();
            while index < lines.len() && lines[index].trim_start().starts_with('>') {
                if !quote.is_empty() {
                    quote.push('\n');
                }
                quote.push_str(lines[index].trim_start().trim_start_matches('>').trim());
                index += 1;
            }
            blocks.push(Block::Quote {
                text: plain_text(&quote)?,
            });
        } else if is_bullet(line) {
            let mut items = Vec::new();
            while index < lines.len() && is_bullet(lines[index].trim_end()) {
                let item = lines[index].trim_start()[2..].trim();
                items.push(parse_inlines(item)?);
                index += 1;
            }
            blocks.push(Block::BulletList { items });
        } else if line.starts_with('|') {
            let (headers, rows, consumed) = parse_table(&lines, index)?;
            blocks.push(Block::Table { headers, rows });
            index = consumed;
        } else if let Some((expression, consumed)) = take_display_math(&lines, index) {
            if !expression.is_empty() {
                blocks.push(Block::Paragraph {
                    inlines: vec![Inline::Formula { expression }],
                });
            }
            index = consumed;
        } else if line.starts_with('<') || line.contains("![") || line.contains("](") {
            return Err(DocumentError::UnsupportedMarkdown(line.to_owned()));
        } else {
            let mut paragraph = line.to_owned();
            index += 1;
            while index < lines.len() {
                let next = lines[index].trim_end();
                if next.trim().is_empty()
                    || heading(next).is_some()
                    || is_bullet(next)
                    || next.starts_with('>')
                    || next.starts_with('|')
                    || next.starts_with("```")
                {
                    break;
                }
                paragraph.push('\n');
                paragraph.push_str(next);
                index += 1;
            }
            blocks.push(Block::Paragraph {
                inlines: parse_inlines(&paragraph)?,
            });
        }
        if blocks.len() > MAX_BLOCKS {
            return Err(DocumentError::InvalidRequest(
                "too many document blocks".to_owned(),
            ));
        }
    }
    Ok(blocks)
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let text = match line[hashes..].strip_prefix(' ') {
        Some(text) => text,
        None => &line[hashes..],
    };
    Some((hashes as u8, text))
}

fn is_bullet(line: &str) -> bool {
    let value = line.trim_start();
    value.starts_with("- ") || value.starts_with("* ")
}

fn looks_like_html(value: &str) -> bool {
    value.as_bytes().windows(2).any(|pair| {
        pair[0] == b'<' && (pair[1].is_ascii_alphabetic() || pair[1] == b'/' || pair[1] == b'!')
    })
}

fn take_display_math(lines: &[&str], start: usize) -> Option<(String, usize)> {
    let first = lines.get(start)?.trim();
    if !first.starts_with("$$") {
        return None;
    }
    if first.len() > 4 && first.ends_with("$$") {
        return Some((first[2..first.len() - 2].trim().to_owned(), start + 1));
    }
    let mut math = if first == "$$" {
        String::new()
    } else {
        first[2..].to_owned()
    };
    let mut index = start + 1;
    while index < lines.len() {
        let next = lines[index].trim_end();
        let trimmed = next.trim();
        if trimmed == "$$" {
            return Some((math.trim().to_owned(), index + 1));
        }
        if let Some(stripped) = trimmed.strip_suffix("$$") {
            if !math.is_empty() {
                math.push('\n');
            }
            math.push_str(stripped.trim());
            return Some((math.trim().to_owned(), index + 1));
        }
        if !math.is_empty() {
            math.push('\n');
        }
        math.push_str(next);
        index += 1;
    }
    Some((math.trim().to_owned(), index))
}

fn parse_table(
    lines: &[&str],
    start: usize,
) -> Result<(Vec<String>, Vec<Vec<String>>, usize), DocumentError> {
    let mut rows = Vec::new();
    let mut index = start;
    while index < lines.len() && lines[index].trim_start().starts_with('|') {
        let cells = split_table_row(lines[index])?;
        if cells.is_empty() || cells.len() > MAX_TABLE_COLUMNS {
            return Err(DocumentError::InvalidRequest(
                "invalid table width".to_owned(),
            ));
        }
        rows.push(cells);
        index += 1;
        if rows.len() > MAX_TABLE_ROWS + 2 {
            return Err(DocumentError::InvalidRequest(
                "table exceeds row limit".to_owned(),
            ));
        }
    }
    if rows.len() < 2
        || !rows[1].iter().all(|cell| {
            cell.chars()
                .all(|c| c == '-' || c == ':' || c.is_whitespace())
        })
    {
        return Err(DocumentError::UnsupportedMarkdown(
            "table separator row is required".to_owned(),
        ));
    }
    let headers = rows.remove(0);
    let _separator = rows.remove(0);
    if rows.iter().any(|row| row.len() != headers.len()) {
        return Err(DocumentError::InvalidRequest(
            "table rows have inconsistent widths".to_owned(),
        ));
    }
    Ok((headers, rows, index))
}

fn split_table_row(line: &str) -> Result<Vec<String>, DocumentError> {
    let trimmed = line.trim();
    if !trimmed.ends_with('|') {
        return Err(DocumentError::UnsupportedMarkdown(
            "table row must end with |".to_owned(),
        ));
    }
    let cells: Vec<String> = trimmed[1..trimmed.len() - 1]
        .split('|')
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    if cells
        .iter()
        .any(|cell| cell.contains('\n') || has_control(cell))
    {
        return Err(DocumentError::UnsupportedMarkdown(
            "invalid table cell".to_owned(),
        ));
    }
    Ok(cells)
}

fn parse_inlines(value: &str) -> Result<Vec<Inline>, DocumentError> {
    let mut output = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if let Some(end) = rest.strip_prefix("![").and_then(|tail| tail.find("](")) {
            let alt = &rest[2..2 + end];
            let target_start = 2 + end + 2;
            let Some(target_end) = rest[target_start..].find(')') else {
                return Err(DocumentError::UnsupportedMarkdown(value.to_owned()));
            };
            let source = &rest[target_start..target_start + target_end];
            validate_image_source(source)?;
            output.push(Inline::Image {
                alt: alt.to_owned(),
                source: source.to_owned(),
            });
            rest = &rest[target_start + target_end + 1..];
        } else if let Some(end) = rest.strip_prefix("**").and_then(|tail| tail.find("**")) {
            let text = &rest[2..2 + end];
            if text.is_empty() {
                return Err(DocumentError::UnsupportedMarkdown(value.to_owned()));
            }
            output.push(Inline::Strong {
                text: text.to_owned(),
            });
            rest = &rest[4 + end..];
        } else if let Some(end) = rest.strip_prefix('`').and_then(|tail| tail.find('`')) {
            let text = &rest[1..1 + end];
            output.push(Inline::Code {
                text: text.to_owned(),
            });
            rest = &rest[2 + end..];
        } else if rest.starts_with("$$") {
            if let Some(end) = rest[2..].find("$$") {
                let expression = rest[2..2 + end].trim();
                if !expression.is_empty() {
                    output.push(Inline::Formula {
                        expression: expression.to_owned(),
                    });
                }
                rest = &rest[4 + end..];
            } else {
                output.push(Inline::Text {
                    text: rest.to_owned(),
                });
                rest = "";
            }
        } else if let Some(end) = rest.strip_prefix('$').and_then(|tail| tail.find('$')) {
            let expression = &rest[1..1 + end];
            if expression.is_empty() {
                output.push(Inline::Text {
                    text: "$".to_owned(),
                });
                rest = &rest[1..];
            } else {
                output.push(Inline::Formula {
                    expression: expression.to_owned(),
                });
                rest = &rest[2 + end..];
            }
        } else if let Some(end) = rest.strip_prefix('*').and_then(|tail| tail.find('*')) {
            let text = &rest[1..1 + end];
            if text.is_empty() {
                return Err(DocumentError::UnsupportedMarkdown(value.to_owned()));
            }
            output.push(Inline::Emphasis {
                text: text.to_owned(),
            });
            rest = &rest[2 + end..];
        } else {
            let next = match [
                rest.find("!["),
                rest.find("**"),
                rest.find('`'),
                rest.find('*'),
                rest.find('$'),
            ]
            .into_iter()
            .flatten()
            .min()
            {
                Some(next) => next,
                None => rest.len(),
            };
            let text = &rest[..next];
            if text.is_empty() {
                output.push(Inline::Text {
                    text: rest[..1].to_owned(),
                });
                rest = &rest[1..];
                continue;
            }
            if looks_like_html(text) {
                return Err(DocumentError::UnsupportedMarkdown(value.to_owned()));
            }
            output.push(Inline::Text {
                text: text.to_owned(),
            });
            rest = &rest[next..];
        }
    }
    Ok(output)
}

fn validate_image_source(source: &str) -> Result<(), DocumentError> {
    let Some((mime, encoded)) = source.split_once(",") else {
        return Err(DocumentError::UnsupportedMarkdown(
            "images must use a data URI".to_owned(),
        ));
    };
    if !mime.starts_with("data:image/") || !mime.ends_with(";base64") || encoded.is_empty() {
        return Err(DocumentError::UnsupportedMarkdown(
            "images must use a base64 data URI".to_owned(),
        ));
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| DocumentError::UnsupportedMarkdown("invalid image data".to_owned()))?;
    if decoded.is_empty() || decoded.len() > MAX_IMAGE_BYTES {
        return Err(DocumentError::InvalidRequest(
            "image exceeds the bounded image size".to_owned(),
        ));
    }
    Ok(())
}

fn inline_image(source: &str) -> Result<(Vec<u8>, PictureFormat), DocumentError> {
    validate_image_source(source)?;
    let (mime, encoded) = source
        .split_once(',')
        .ok_or_else(|| DocumentError::UnsupportedMarkdown("invalid image data".to_owned()))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| DocumentError::UnsupportedMarkdown("invalid image data".to_owned()))?;
    let format = match mime
        .trim_start_matches("data:image/")
        .trim_end_matches(";base64")
    {
        "png" => PictureFormat::Png,
        "jpeg" | "jpg" => PictureFormat::Jpeg,
        "gif" => PictureFormat::Gif,
        "bmp" => PictureFormat::Bmp,
        _ => {
            return Err(DocumentError::UnsupportedMarkdown(
                "unsupported image format".to_owned(),
            ));
        }
    };
    Ok((bytes, format))
}

fn plain_text(value: &str) -> Result<String, DocumentError> {
    parse_inlines(value).map(|inlines| {
        inlines
            .into_iter()
            .map(|item| match item {
                Inline::Text { text }
                | Inline::Strong { text }
                | Inline::Emphasis { text }
                | Inline::Code { text } => text,
                Inline::Formula { expression } => expression,
                Inline::Image { alt, .. } => alt,
            })
            .collect()
    })
}

fn render(
    ir: &DocumentIr,
    format: DocumentFormat,
    fonts: &[&[u8]],
) -> Result<Vec<u8>, DocumentError> {
    let bytes = match format {
        DocumentFormat::Docx => render_docx(ir)?,
        DocumentFormat::Xlsx => render_xlsx(ir)?,
        DocumentFormat::Pptx => render_pptx(ir)?,
        DocumentFormat::Pdf => render_pdf(ir, fonts)?,
    };
    if matches!(
        format,
        DocumentFormat::Docx | DocumentFormat::Pptx | DocumentFormat::Xlsx
    ) {
        validate_archive(&bytes)?;
    }
    Ok(bytes)
}

fn render_docx(ir: &DocumentIr) -> Result<Vec<u8>, DocumentError> {
    let bullet_level = Level::new(
        0,
        Start::new(1),
        NumberFormat::new("bullet"),
        LevelText::new("•"),
        LevelJc::new("left"),
    )
    .indent(Some(720), Some(SpecialIndentType::Hanging(360)), None, None)
    .fonts(docx_body_fonts());
    let mut document = Docx::new()
        .add_abstract_numbering(
            AbstractNumbering::new(DOCX_BULLET_NUMBERING_ID).add_level(bullet_level),
        )
        .add_numbering(Numbering::new(
            DOCX_BULLET_NUMBERING_ID,
            DOCX_BULLET_NUMBERING_ID,
        ))
        .add_paragraph(
            Paragraph::new()
                .style("Title")
                .add_run(docx_text_run(&ir.title).bold()),
        )
        .add_paragraph(
            Paragraph::new()
                .style("Subtitle")
                .add_run(docx_text_run(&ir.goal)),
        );
    for block in &ir.blocks {
        document = match block {
            Block::Heading { level, text } => document.add_paragraph(
                Paragraph::new()
                    .style(&format!("Heading{}", (*level).clamp(1, 6)))
                    .add_run(docx_text_run(text).bold()),
            ),
            Block::Paragraph { inlines } => document.add_paragraph(docx_inline_paragraph(inlines)?),
            Block::BulletList { items } => {
                for item in items {
                    document = document.add_paragraph(docx_inline_paragraph(item)?.numbering(
                        NumberingId::new(DOCX_BULLET_NUMBERING_ID),
                        IndentLevel::new(0),
                    ));
                }
                document
            }
            Block::Table { headers, rows } => document.add_table(docx_table(headers, rows)),
            Block::Quote { text } => document.add_paragraph(
                Paragraph::new()
                    .style("Quote")
                    .add_run(docx_text_run(text).italic()),
            ),
            Block::Code { text, .. } => document.add_paragraph(
                Paragraph::new()
                    .style("NoSpacing")
                    .add_run(docx_code_run(text)),
            ),
        };
    }
    let mut output = Cursor::new(Vec::new());
    document
        .build()
        .pack(&mut output)
        .map_err(|error| DocumentError::Renderer(error.to_string()))?;
    Ok(output.into_inner())
}

fn docx_body_fonts() -> RunFonts {
    RunFonts::new()
        .ascii("Aptos")
        .hi_ansi("Aptos")
        .east_asia("Noto Sans CJK SC")
}

fn docx_code_fonts() -> RunFonts {
    RunFonts::new()
        .ascii("Consolas")
        .hi_ansi("Consolas")
        .east_asia("Noto Sans Mono CJK SC")
}

fn docx_text_run(text: &str) -> Run {
    Run::new().add_text(text).fonts(docx_body_fonts())
}

fn docx_code_run(text: &str) -> Run {
    Run::new().add_text(text).fonts(docx_code_fonts())
}

fn docx_inline_paragraph(inlines: &[Inline]) -> Result<Paragraph, DocumentError> {
    let mut paragraph = Paragraph::new();
    for inline in inlines {
        let run = match inline {
            Inline::Text { text } => docx_text_run(text),
            Inline::Strong { text } => docx_text_run(text).bold(),
            Inline::Emphasis { text } => docx_text_run(text).italic(),
            Inline::Code { text } => docx_code_run(text),
            Inline::Formula { expression } => docx_code_run(expression),
            Inline::Image { source, .. } => {
                let (bytes, _) = inline_image(source)?;
                Run::new().add_image(Pic::new_with_dimensions(bytes, 320, 240))
            }
        };
        paragraph = paragraph.add_run(run);
    }
    Ok(paragraph)
}

fn docx_table(headers: &[String], rows: &[Vec<String>]) -> DocxTable {
    let mut table_rows = Vec::with_capacity(rows.len() + 1);
    table_rows.push(DocxTableRow::new(
        headers
            .iter()
            .map(|header| {
                DocxTableCell::new()
                    .add_paragraph(Paragraph::new().add_run(docx_text_run(header).bold()))
            })
            .collect(),
    ));
    table_rows.extend(rows.iter().map(|row| {
        DocxTableRow::new(
            row.iter()
                .map(|cell| {
                    DocxTableCell::new()
                        .add_paragraph(Paragraph::new().add_run(docx_text_run(cell)))
                })
                .collect(),
        )
    }));
    DocxTable::new(table_rows).style("TableGrid")
}

fn render_xlsx(ir: &DocumentIr) -> Result<Vec<u8>, DocumentError> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    let body_format = Format::new().set_font_name(PPTX_FONT).set_font_size(11);
    let heading_format = Format::new()
        .set_font_name(PPTX_FONT)
        .set_font_size(14)
        .set_bold();
    worksheet
        .write_string_with_format(0, 0, &ir.title, &heading_format)
        .map_err(|error| DocumentError::Renderer(error.to_string()))?;
    let mut row: u32 = 1;
    for block in &ir.blocks {
        match block {
            Block::Table { headers, rows } => {
                for (column, value) in headers.iter().enumerate() {
                    worksheet
                        .write_string_with_format(row, column as u16, value, &heading_format)
                        .map_err(|error| DocumentError::Renderer(error.to_string()))?;
                }
                row += 1;
                for values in rows {
                    for (column, value) in values.iter().enumerate() {
                        if value.starts_with('=') {
                            worksheet
                                .write_formula_with_format(
                                    row,
                                    column as u16,
                                    value.as_str(),
                                    &body_format,
                                )
                                .map_err(|error| DocumentError::Renderer(error.to_string()))?;
                        } else {
                            worksheet
                                .write_string_with_format(row, column as u16, value, &body_format)
                                .map_err(|error| DocumentError::Renderer(error.to_string()))?;
                        }
                    }
                    row += 1;
                }
            }
            _ => {
                worksheet
                    .write_string_with_format(row, 0, block_text(block), &body_format)
                    .map_err(|error| DocumentError::Renderer(error.to_string()))?;
                row += 1;
            }
        }
    }
    workbook
        .save_to_buffer()
        .map_err(|error| DocumentError::Renderer(error.to_string()))
}

fn render_pptx(ir: &DocumentIr) -> Result<Vec<u8>, DocumentError> {
    let mut theme = Theme::office_default();
    theme.name = "AI Platform".to_owned();
    theme.fonts.major_latin = PPTX_FONT.to_owned();
    theme.fonts.minor_latin = PPTX_FONT.to_owned();
    let mut presentation = Presentation::new()
        .with_theme(theme)
        .with_properties(
            DocumentProperties::new()
                .with_title(&ir.title)
                .with_subject(&ir.goal),
        )
        .with_slide(pptx_title_slide(ir));
    for slide in pptx_content_slides(ir)? {
        presentation = presentation.with_slide(slide);
    }
    presentation
        .write_to(Cursor::new(Vec::new()))
        .map(|cursor| cursor.into_inner())
        .map_err(|error| DocumentError::Renderer(error.to_string()))
}

fn pptx_title_slide(ir: &DocumentIr) -> Slide {
    Slide::new()
        .with_name("Title")
        .with_shape(pptx_text_shape(
            2,
            "Title",
            pptx_plain_text_body(&ir.title, 30.0, true, false),
            (700_000, 1_300_000, 10_792_000, 1_400_000),
        ))
        .with_shape(pptx_text_shape(
            3,
            "Goal",
            pptx_plain_text_body(&ir.goal, 18.0, false, false),
            (1_000_000, 3_000_000, 10_192_000, 1_200_000),
        ))
}

fn pptx_content_slides(ir: &DocumentIr) -> Result<Vec<Slide>, DocumentError> {
    let sections = pptx_sections(ir);
    let mut output = Vec::new();
    for (section_title, blocks) in sections {
        let mut slide = pptx_content_slide(&section_title, false);
        let mut next_shape_id = 3_u32;
        let mut y = 1_150_000_i64;
        for block in blocks {
            validate_pptx_block(block)?;
            let height = pptx_block_height(block);
            if y + height > 6_500_000 && y > 1_150_000 {
                output.push(slide);
                slide = pptx_content_slide(&section_title, true);
                next_shape_id = 3;
                y = 1_150_000;
            }
            if y + height > 6_500_000 {
                return Err(DocumentError::InvalidRequest(
                    "a single PPTX block exceeds the usable slide height".to_owned(),
                ));
            }
            let shape = pptx_block_shape(next_shape_id, block, y, height)?;
            slide = slide.with_shape(shape);
            next_shape_id += 1;
            for (image_index, (alt, source)) in pptx_block_images(block).into_iter().enumerate() {
                let (data, format) = inline_image(&source)?;
                slide = slide.with_shape(Shape::Picture(
                    Picture::new(
                        next_shape_id,
                        format!("Image {}", image_index + 1),
                        data,
                        format,
                        2_200_000,
                        1_500_000,
                    )
                    .with_offset(8_900_000, y + 100_000)
                    .with_description(alt),
                ));
                next_shape_id += 1;
            }
            y += height + 120_000;
        }
        output.push(slide);
    }
    Ok(output)
}

fn pptx_block_images(block: &Block) -> Vec<(String, String)> {
    let mut images = Vec::new();
    let mut collect = |inlines: &[Inline]| {
        for inline in inlines {
            if let Inline::Image { alt, source } = inline {
                images.push((alt.clone(), source.clone()));
            }
        }
    };
    match block {
        Block::Paragraph { inlines } => collect(inlines),
        Block::BulletList { items } => items.iter().for_each(|item| collect(item)),
        _ => {}
    }
    images
}

fn pptx_sections(ir: &DocumentIr) -> Vec<(String, Vec<&Block>)> {
    let mut sections = Vec::new();
    let mut title = "Overview".to_owned();
    let mut blocks = Vec::new();
    let mut saw_top_level_heading = false;
    for block in &ir.blocks {
        if let Block::Heading { level: 1, text } = block {
            if saw_top_level_heading || !blocks.is_empty() {
                sections.push((title, std::mem::take(&mut blocks)));
            }
            title = text.clone();
            saw_top_level_heading = true;
        } else {
            blocks.push(block);
        }
    }
    if saw_top_level_heading || !blocks.is_empty() {
        sections.push((title, blocks));
    }
    sections
}

fn pptx_content_slide(title: &str, continued: bool) -> Slide {
    let display_title = if continued {
        format!("{title} (continued)")
    } else {
        title.to_owned()
    };
    Slide::new()
        .with_name(display_title.clone())
        .with_shape(pptx_text_shape(
            2,
            "Section title",
            pptx_plain_text_body(&display_title, 24.0, true, false),
            (600_000, 250_000, 10_992_000, 700_000),
        ))
}

fn validate_pptx_block(block: &Block) -> Result<(), DocumentError> {
    if block_text(block).chars().count() > MAX_PPTX_TEXT_BLOCK_CHARS {
        return Err(DocumentError::InvalidRequest(format!(
            "PPTX text block exceeds {MAX_PPTX_TEXT_BLOCK_CHARS} characters"
        )));
    }
    match block {
        Block::BulletList { items } if items.len() > 12 => Err(DocumentError::InvalidRequest(
            "PPTX bullet list exceeds 12 items".to_owned(),
        )),
        Block::Table { headers, rows }
            if headers.len() > MAX_PPTX_TABLE_COLUMNS || rows.len() > MAX_PPTX_TABLE_ROWS =>
        {
            Err(DocumentError::InvalidRequest(format!(
                "PPTX tables are limited to {MAX_PPTX_TABLE_COLUMNS} columns and {MAX_PPTX_TABLE_ROWS} data rows"
            )))
        }
        _ => Ok(()),
    }
}

fn pptx_block_height(block: &Block) -> i64 {
    match block {
        Block::Heading { .. } => 550_000,
        Block::Paragraph { inlines } => {
            500_000 + (inline_text(inlines).chars().count() as i64 / 80) * 260_000
        }
        Block::BulletList { items } => 180_000 + items.len() as i64 * 360_000,
        Block::Table { rows, .. } => 220_000 + (rows.len() as i64 + 1) * 360_000,
        Block::Quote { text } => 520_000 + (text.chars().count() as i64 / 80) * 260_000,
        Block::Code { text, .. } => {
            520_000 + (text.lines().count().saturating_sub(1) as i64) * 240_000
        }
    }
}

fn pptx_block_shape(id: u32, block: &Block, y: i64, height: i64) -> Result<Shape, DocumentError> {
    let bounds = (700_000, y, 10_792_000, height);
    let shape = match block {
        Block::Heading { level, text } => pptx_text_shape(
            id,
            "Heading",
            pptx_plain_text_body(text, if *level <= 2 { 20.0 } else { 17.0 }, true, false),
            bounds,
        ),
        Block::Paragraph { inlines } => pptx_text_shape(
            id,
            "Paragraph",
            pptx_inline_text_body(inlines, 15.0, None),
            bounds,
        ),
        Block::BulletList { items } => {
            let mut body = TextBody::new();
            for item in items {
                body = body.with_paragraph(pptx_inline_paragraph(item, 15.0, Some("•")));
            }
            pptx_text_shape(id, "Bullet list", body, bounds)
        }
        Block::Table { headers, rows } => pptx_table_shape(id, headers, rows, y, height),
        Block::Quote { text } => pptx_text_shape(
            id,
            "Quote",
            pptx_plain_text_body(text, 15.0, false, true),
            bounds,
        ),
        Block::Code { text, .. } => {
            pptx_text_shape(id, "Code", pptx_code_text_body(text, 12.0), bounds)
        }
    };
    Ok(shape)
}

fn pptx_text_shape(
    id: u32,
    name: &str,
    text_body: TextBody,
    bounds: (i64, i64, i64, i64),
) -> Shape {
    let (x, y, width, height) = bounds;
    let properties = ShapeProperties::new()
        .with_transform(
            Transform2D::new()
                .with_offset(x, y)
                .with_extent(width, height),
        )
        .with_geometry(Geometry::Preset(PresetShape::Rectangle));
    Shape::AutoShape(
        AutoShape::new(id, name)
            .with_properties(properties)
            .with_text_body(text_body)
            .with_text_box(true),
    )
}

fn pptx_run(text: &str, size: f64, bold: bool, italic: bool, code: bool) -> TextRun {
    let mut properties = TextRunProperties::new()
        .with_font_family(if code { PPTX_CODE_FONT } else { PPTX_FONT })
        .with_font_size_points(size)
        .with_bold(bold)
        .with_italic(italic);
    if code {
        properties = properties.with_character_spacing_points(0.2);
    }
    TextRun::text_with_properties(text, properties)
}

fn pptx_plain_text_body(text: &str, size: f64, bold: bool, italic: bool) -> TextBody {
    TextBody::new()
        .with_paragraph(TextParagraph::new().with_run(pptx_run(text, size, bold, italic, false)))
}

fn pptx_code_text_body(text: &str, size: f64) -> TextBody {
    TextBody::new()
        .with_paragraph(TextParagraph::new().with_run(pptx_run(text, size, false, false, true)))
}

fn pptx_inline_text_body(inlines: &[Inline], size: f64, bullet: Option<&str>) -> TextBody {
    TextBody::new().with_paragraph(pptx_inline_paragraph(inlines, size, bullet))
}

fn pptx_inline_paragraph(inlines: &[Inline], size: f64, bullet: Option<&str>) -> TextParagraph {
    let mut paragraph = TextParagraph::new();
    if let Some(marker) = bullet {
        paragraph = paragraph.with_properties(
            TextParagraphProperties::new()
                .with_margins_emu(360_000, 0)
                .with_indent_emu(-180_000)
                .with_bullet(BulletKind::Character(marker.to_owned())),
        );
    }
    for inline in inlines {
        let run = match inline {
            Inline::Text { text } => pptx_run(text, size, false, false, false),
            Inline::Strong { text } => pptx_run(text, size, true, false, false),
            Inline::Emphasis { text } => pptx_run(text, size, false, true, false),
            Inline::Code { text } => pptx_run(text, size, false, false, true),
            Inline::Formula { expression } => pptx_run(expression, size, false, false, true),
            Inline::Image { alt, .. } => pptx_run(alt, size, false, true, false),
        };
        paragraph = paragraph.with_run(run);
    }
    paragraph
}

fn pptx_table_shape(
    id: u32,
    headers: &[String],
    rows: &[Vec<String>],
    y: i64,
    height: i64,
) -> Shape {
    let table_width = 10_792_000_i64;
    let column_width = table_width / headers.len() as i64;
    let mut table = SlideTable::new(id, "Table", table_width, height)
        .with_offset(700_000, y)
        .with_column_widths(vec![column_width; headers.len()])
        .with_style_first_row(true)
        .with_style_band_rows(true)
        .with_row(pptx_table_row(headers, true));
    for row in rows {
        table = table.with_row(pptx_table_row(row, false));
    }
    Shape::Table(table)
}

fn pptx_table_row(values: &[String], header: bool) -> PptxTableRow {
    let mut row = PptxTableRow::new(360_000);
    for value in values {
        row = row.with_cell(
            PptxTableCell::new().with_text_body(pptx_plain_text_body(value, 12.0, header, false)),
        );
    }
    row
}

fn render_pdf(ir: &DocumentIr, fonts: &[&[u8]]) -> Result<Vec<u8>, DocumentError> {
    if fonts.is_empty() || fonts.iter().any(|font| font.is_empty()) {
        return Err(DocumentError::InvalidRequest(
            "PDF rendering requires caller-provided controlled font bytes".to_owned(),
        ));
    }
    let engine = TypstEngine::builder()
        .main_file(typst_source(ir))
        .fonts(fonts.iter().copied())
        .build();
    let compiled = engine.compile();
    if !compiled.warnings.is_empty() {
        return Err(DocumentError::Renderer(
            "Typst emitted a rendering warning; verify the controlled font pack".to_owned(),
        ));
    }
    let document = compiled
        .output
        .map_err(|error| DocumentError::Renderer(format!("{error:?}")))?;
    typst_pdf::pdf(&document, &Default::default())
        .map_err(|error| DocumentError::Renderer(format!("{error:?}")))
}

fn typst_source(ir: &DocumentIr) -> String {
    let language = ir.locale.split('-').next().unwrap_or("en");
    let font = if language.eq_ignore_ascii_case("zh") {
        "Noto Sans CJK SC"
    } else {
        "Libertinus Serif"
    };
    let mut source = format!("#set text(lang: \"{language}\", font: \"{font}\")\n");
    source.push_str("#set page(margin: 1.5cm)\n");
    source.push_str("#text(size: 20pt, weight: \"bold\", \"");
    source.push_str(&typst_string_escape(&ir.title));
    source.push_str("\")\n\n");
    for block in &ir.blocks {
        source.push_str("#text(\"");
        source.push_str(&typst_string_escape(&block_text(block)));
        source.push_str("\")\n\n");
    }
    source
}

fn typst_string_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn block_text(block: &Block) -> String {
    match block {
        Block::Heading { text, .. } | Block::Quote { text } | Block::Code { text, .. } => {
            text.clone()
        }
        Block::Paragraph { inlines } => inline_text(inlines),
        Block::BulletList { items } => items
            .iter()
            .map(|item| format!("• {}", inline_text(item)))
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Table { headers, rows } => std::iter::once(headers.join(" | "))
            .chain(rows.iter().map(|row| row.join(" | ")))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn inline_text(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text { text }
            | Inline::Strong { text }
            | Inline::Emphasis { text }
            | Inline::Code { text } => text.as_str(),
            Inline::Formula { expression } => expression.as_str(),
            Inline::Image { alt, .. } => alt.as_str(),
        })
        .collect()
}

fn plan_outline(ir: &DocumentIr) -> String {
    ir.blocks
        .iter()
        .filter_map(|block| match block {
            Block::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn safe_filename(title: &str) -> String {
    let mut output = String::new();
    for character in title.chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_') {
            output.push(character);
        } else if character.is_whitespace() {
            if !output.ends_with('-') {
                output.push('-');
            }
        }
        if output.chars().count() >= 96 {
            break;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "document".to_owned()
    } else {
        output
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn validate_archive(bytes: &[u8]) -> Result<(), DocumentError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(DocumentError::InvalidArchive(
            "archive exceeds the bounded package size".to_owned(),
        ));
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| DocumentError::InvalidArchive(error.to_string()))?;
    if archive.len() == 0 || archive.len() > MAX_ARCHIVE_PARTS {
        return Err(DocumentError::InvalidArchive(
            "archive contains no parts or too many parts".to_owned(),
        ));
    }
    let mut total_uncompressed = 0usize;
    for index in 0..archive.len() {
        let mut part = archive
            .by_index(index)
            .map_err(|error| DocumentError::InvalidArchive(error.to_string()))?;
        let name = part.name();
        let checked_name = name.strip_suffix('/').unwrap_or(name);
        if checked_name.is_empty()
            || checked_name.starts_with('/')
            || checked_name.contains('\\')
            || checked_name
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(DocumentError::InvalidArchive(
                "unsafe archive part path".to_owned(),
            ));
        }
        total_uncompressed = total_uncompressed
            .checked_add(part.size() as usize)
            .ok_or_else(|| DocumentError::InvalidArchive("archive size overflow".to_owned()))?;
        if total_uncompressed > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
            return Err(DocumentError::InvalidArchive(
                "archive exceeds the bounded uncompressed size".to_owned(),
            ));
        }
        let mut sink = Vec::new();
        part.read_to_end(&mut sink)
            .map_err(|error| DocumentError::InvalidArchive(error.to_string()))?;
    }
    Ok(())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn package_parts(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, DocumentError> {
    validate_archive(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| DocumentError::InvalidArchive(error.to_string()))?;
    let mut parts = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut part = archive
            .by_index(index)
            .map_err(|error| DocumentError::InvalidArchive(error.to_string()))?;
        if part.is_dir() {
            continue;
        }
        let name = part.name().to_owned();
        let mut content = Vec::with_capacity(part.size() as usize);
        part.read_to_end(&mut content)
            .map_err(|error| DocumentError::InvalidArchive(error.to_string()))?;
        parts.push((name, content));
    }
    Ok(parts)
}

fn rewrite_package(
    bytes: &[u8],
    replacements: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, DocumentError> {
    use zip::write::{SimpleFileOptions, ZipWriter};
    let parts = package_parts(bytes)?;
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut output);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in parts {
            writer
                .start_file(&name, options)
                .map_err(|error| DocumentError::Renderer(error.to_string()))?;
            let payload = replacements
                .get(&name)
                .map_or(content.as_slice(), |replacement| replacement.as_slice());
            writer
                .write_all(payload)
                .map_err(|error| DocumentError::Renderer(error.to_string()))?;
        }
        writer
            .finish()
            .map_err(|error| DocumentError::Renderer(error.to_string()))?;
    }
    let output = output.into_inner();
    validate_archive(&output)?;
    Ok(output)
}

fn append_before(xml: &[u8], closing: &str, addition: &str) -> Result<Vec<u8>, DocumentError> {
    let source = std::str::from_utf8(xml)
        .map_err(|_| DocumentError::InvalidArchive("OOXML part is not UTF-8".to_owned()))?;
    let position = source
        .rfind(closing)
        .ok_or_else(|| DocumentError::InvalidArchive(format!("OOXML part lacks {closing}")))?;
    let mut output = String::with_capacity(source.len() + addition.len());
    output.push_str(&source[..position]);
    output.push_str(addition);
    output.push_str(&source[position..]);
    Ok(output.into_bytes())
}

fn docx_edit_payload(ir: &DocumentIr) -> String {
    let mut paragraphs = Vec::new();
    let mut texts = vec![ir.title.clone()];
    texts.extend(ir.blocks.iter().map(block_text));
    for text in texts {
        paragraphs.push(format!(
            "<w:p><w:r><w:rPr><w:rFonts w:ascii=\"Aptos\" w:eastAsia=\"Noto Sans CJK SC\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
            xml_escape(&text)
        ));
    }
    paragraphs.join("")
}

fn xlsx_edit_payload(ir: &DocumentIr, target: &[u8]) -> String {
    let source = std::str::from_utf8(target).unwrap_or_default();
    let next_row = source
        .rsplit("<row r=\"")
        .next()
        .and_then(|tail| tail.split('"').next())
        .and_then(|number| number.parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    let mut cells = Vec::new();
    for (column, text) in std::iter::once(ir.title.clone())
        .chain(ir.blocks.iter().map(block_text))
        .enumerate()
    {
        let column_name = excel_column(column as u32);
        if text.starts_with('=') {
            cells.push(format!(
                "<c r=\"{column_name}{next_row}\"><f>{}</f><v></v></c>",
                xml_escape(text.trim_start_matches('='))
            ));
        } else {
            cells.push(format!(
                "<c r=\"{column_name}{next_row}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                xml_escape(&text)
            ));
        }
    }
    format!("<row r=\"{next_row}\">{}</row>", cells.join(""))
}

fn excel_column(mut index: u32) -> String {
    let mut output = String::new();
    loop {
        output.insert(0, char::from(b'A' + (index % 26) as u8));
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    output
}

fn pptx_edit_payload(ir: &DocumentIr, target: &[u8]) -> String {
    let source = std::str::from_utf8(target).unwrap_or_default();
    let id = source
        .split("id=\"")
        .skip(1)
        .filter_map(|value| value.split('"').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(1)
        .saturating_add(1);
    let text = xml_escape(&format!(
        "{}\n{}",
        ir.title,
        ir.blocks
            .iter()
            .map(block_text)
            .collect::<Vec<_>>()
            .join("\n")
    ));
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Edited content\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\" sz=\"1200\"/><a:t>{text}</a:t></a:r><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>"
    )
}

fn extract_xml_text(xml: &[u8], tag: &str) -> Vec<String> {
    let Ok(source) = std::str::from_utf8(xml) else {
        return Vec::new();
    };
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    let mut rest = source;
    let mut output = Vec::new();
    while let Some(start) = rest.find(&opening) {
        let after_name = rest.as_bytes().get(start + opening.len()).copied();
        if !matches!(
            after_name,
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            rest = &rest[start + opening.len()..];
            continue;
        }
        let Some(content_start) = rest[start..].find('>').map(|offset| start + offset + 1) else {
            break;
        };
        let Some(end) = rest[content_start..]
            .find(&closing)
            .map(|offset| content_start + offset)
        else {
            break;
        };
        let text = rest[content_start..end]
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'");
        if !text.is_empty() {
            output.push(text);
        }
        rest = &rest[end + closing.len()..];
    }
    output
}

fn unpack_preview(format: DocumentFormat, source: &[u8]) -> Result<DocumentPreview, DocumentError> {
    if format == DocumentFormat::Pdf {
        return Err(DocumentError::InvalidRequest(
            "PDF preview requires a controlled PDF text/image extractor".to_owned(),
        ));
    }
    let parts = package_parts(source)?;
    let part_names = parts
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let known = |name: &str| match format {
        DocumentFormat::Docx => {
            name.starts_with("word/")
                || name.starts_with("docProps/")
                || name.starts_with("_rels/")
                || name == "[Content_Types].xml"
        }
        DocumentFormat::Xlsx => {
            name.starts_with("xl/")
                || name.starts_with("docProps/")
                || name.starts_with("_rels/")
                || name == "[Content_Types].xml"
        }
        DocumentFormat::Pptx => {
            name.starts_with("ppt/")
                || name.starts_with("docProps/")
                || name.starts_with("_rels/")
                || name == "[Content_Types].xml"
        }
        DocumentFormat::Pdf => false,
    };
    let unknown_parts = part_names
        .iter()
        .filter(|name| !known(name))
        .cloned()
        .collect::<Vec<_>>();
    let mut fragments = Vec::new();
    let mut image_part_count = 0;
    for (name, content) in &parts {
        if name.contains("/media/") || name.starts_with("xl/media/") {
            image_part_count += 1;
        }
        let tag = match format {
            DocumentFormat::Docx => "w:t",
            DocumentFormat::Xlsx => "t",
            DocumentFormat::Pptx => "a:t",
            DocumentFormat::Pdf => unreachable!(),
        };
        fragments.extend(extract_xml_text(content, tag));
    }
    let title = fragments
        .first()
        .cloned()
        .unwrap_or_else(|| "Preview".to_owned());
    let blocks = if fragments.len() > 1 {
        fragments[1..]
            .iter()
            .cloned()
            .map(|text| Block::Paragraph {
                inlines: vec![Inline::Text { text }],
            })
            .collect()
    } else {
        Vec::new()
    };
    let semantic_ir = DocumentIr {
        title,
        goal: "Unpacked semantic preview".to_owned(),
        locale: "en-US".to_owned(),
        design_system: DesignSystem::Enterprise,
        blocks,
    };
    Ok(DocumentPreview {
        format,
        semantic_ir,
        part_names,
        unknown_parts,
        visual_input: VisualInput {
            source_format: format,
            text_fragments: fragments,
            image_part_count,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn request(format: DocumentFormat, body: &str) -> GenerateDocumentRequest {
        GenerateDocumentRequest {
            format,
            title: "Deterministic Report".to_owned(),
            goal: "A bounded semantic fixture".to_owned(),
            body_markdown: Some(body.to_owned()),
            locale: "en-US".to_owned(),
            design_system: Some(DesignSystem::Enterprise),
            template_name: None,
        }
    }

    #[test]
    fn parses_supported_blocks_and_rejects_unknown_markup() {
        let ir = build_ir(&request(DocumentFormat::Docx, "# Heading\n\n- one\n- two\n\n| A | B |\n| --- | --- |\n| 1 | =SUM(1,2) |\n\n> quote\n\n```rust\nlet x = 1;\n```")).expect("fixture parses");
        assert_eq!(ir.blocks.len(), 5);
        assert!(build_ir(&request(DocumentFormat::Docx, "![image](x)")).is_err());
    }

    #[test]
    fn bounds_and_semantic_determinism_are_stable() {
        let too_long = "x".repeat(MAX_BODY_CHARS + 1);
        assert!(build_ir(&request(DocumentFormat::Pdf, &too_long)).is_err());
        let first = build_ir(&request(DocumentFormat::Pdf, "## A\n\ntext")).expect("first");
        let second = build_ir(&request(DocumentFormat::Pdf, "## A\n\ntext")).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn pdf_requires_controlled_font_bytes_and_does_not_persist() {
        let generator = DocumentGenerator::new();
        let req = request(DocumentFormat::Pdf, "中文输入");
        let error = generator.generate(&req).expect_err("font gate");
        assert!(error.to_string().contains("controlled font bytes"));
    }

    fn archive_entry(bytes: &[u8], name: &str) -> String {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("valid archive");
        let mut entry = archive.by_name(name).expect("archive entry");
        let mut output = String::new();
        entry.read_to_string(&mut output).expect("UTF-8 XML");
        output
    }

    #[test]
    fn display_math_is_kept_as_formula_instead_of_failing_generation() {
        let generator = DocumentGenerator::new();
        let result = generator
            .generate(&GenerateDocumentRequest {
                format: DocumentFormat::Docx,
                title: "Flow Matching".to_owned(),
                goal: "Keep LaTeX formulas in generated Word documents".to_owned(),
                body_markdown: Some(
                    "The ODE is\n\n$$\\frac{dx_t}{dt} = v_t(x_t)$$\n\nwith inline $x_t$."
                        .to_owned(),
                ),
                locale: "en-US".to_owned(),
                design_system: Some(DesignSystem::Enterprise),
                template_name: None,
            })
            .expect("display math must not fail document generation");
        let document = archive_entry(&result.bytes, "word/document.xml");
        assert!(document.contains("dx_t"));
        assert!(document.contains("x_t"));
    }

    #[test]
    fn unterminated_display_math_is_kept_as_text() {
        let generator = DocumentGenerator::new();
        generator
            .generate(&GenerateDocumentRequest {
                format: DocumentFormat::Docx,
                title: "Partial formula".to_owned(),
                goal: "Do not fail the whole document on an unclosed formula".to_owned(),
                body_markdown: Some("$$\\frac{dx_t}{dt} = v_t(x".to_owned()),
                locale: "en-US".to_owned(),
                design_system: Some(DesignSystem::Enterprise),
                template_name: None,
            })
            .expect("unterminated display math must not fail generation");
    }

    #[test]
    fn docx_package_preserves_rich_semantic_structure() {
        let generator = DocumentGenerator::new();
        let result = generator
            .generate(&GenerateDocumentRequest {
                format: DocumentFormat::Docx,
                title: "中文结构测试".to_owned(),
                goal: "验证真实 WordprocessingML 语义".to_owned(),
                body_markdown: Some(
                    "# 一级标题\n\n正文 **粗体**、*斜体* 与 `代码`。\n\n- 条目一\n- 条目二\n\n| 列一 | 列二 |\n| --- | --- |\n| 值一 | 值二 |\n\n> 引用内容\n\n```rust\nlet answer = 42;\n```"
                        .to_owned(),
                ),
                locale: "zh-CN".to_owned(),
                design_system: Some(DesignSystem::Enterprise),
                template_name: None,
            })
            .expect("docx");
        let document = archive_entry(&result.bytes, "word/document.xml");
        let numbering = archive_entry(&result.bytes, "word/numbering.xml");

        assert!(document.contains("一级标题"));
        assert!(document.contains("引用内容"));
        assert!(document.contains("let answer = 42;"));
        assert!(document.contains("w:pStyle w:val=\"Heading1\""));
        assert!(document.contains("<w:b"));
        assert!(document.contains("<w:i"));
        assert!(document.contains("<w:numPr>"));
        assert!(document.contains("<w:tbl>"));
        assert!(document.contains("<w:tr>"));
        assert!(document.contains("<w:tc>"));
        assert!(document.contains("w:pStyle w:val=\"Quote\""));
        assert!(document.contains("w:ascii=\"Consolas\""));
        assert!(document.contains("w:eastAsia=\"Noto Sans Mono CJK SC\""));
        assert!(numbering.contains("w:numFmt w:val=\"bullet\""));
        assert!(numbering.contains("w:lvlText w:val=\"•\""));
    }

    #[test]
    fn pptx_package_preserves_slides_shapes_runs_bullets_and_table() {
        let generator = DocumentGenerator::new();
        let result = generator
            .generate(&GenerateDocumentRequest {
                format: DocumentFormat::Pptx,
                title: "中文演示标题".to_owned(),
                goal: "验证真实 PresentationML 语义".to_owned(),
                body_markdown: Some(
                    "# 第一章\n\n正文 **粗体** 与 *斜体*。\n\n- 条目一\n- 条目二\n\n| 指标 | 数值 |\n| --- | --- |\n| 延迟 | 3 秒 |\n\n# 第二章\n\n继续说明。"
                        .to_owned(),
                ),
                locale: "zh-CN".to_owned(),
                design_system: Some(DesignSystem::Editorial),
                template_name: None,
            })
            .expect("pptx");

        let read_back = Presentation::read_from(Cursor::new(result.bytes.as_slice()))
            .expect("generated package should round-trip");
        assert_eq!(read_back.slides.len(), 3);
        assert!(
            read_back.slides[1]
                .shapes
                .iter()
                .any(|shape| matches!(shape, Shape::Table(_)))
        );

        let title_slide = archive_entry(&result.bytes, "ppt/slides/slide1.xml");
        let first_content = archive_entry(&result.bytes, "ppt/slides/slide2.xml");
        let second_content = archive_entry(&result.bytes, "ppt/slides/slide3.xml");
        assert!(title_slide.contains("中文演示标题"));
        assert!(first_content.contains("第一章"));
        assert!(second_content.contains("第二章"));
        assert!(first_content.matches("<p:sp>").count() >= 3);
        assert!(first_content.contains(" b=\"1\""));
        assert!(first_content.contains(" i=\"1\""));
        assert!(first_content.contains("<a:buChar"));
        assert!(first_content.contains("<a:tbl>"));
        assert!(first_content.contains("<a:tr"));
        assert!(first_content.contains("Noto Sans CJK SC"));

        let theme = archive_entry(&result.bytes, "ppt/theme/theme1.xml");
        assert!(theme.contains("Noto Sans CJK SC"));
        let _ = archive_entry(&result.bytes, "ppt/slideMasters/slideMaster1.xml");
        let _ = archive_entry(&result.bytes, "ppt/slideLayouts/slideLayout1.xml");
    }
}
