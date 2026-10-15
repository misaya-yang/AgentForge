//! Native bounded chunking, preserving public configuration aliases and hierarchy.
use super::parser::{require_text_budget, sanitize_text};
use super::{Chunk, IngestError, Result, object, sha256, stable_index_node_id, stable_segment_id};
use serde_json::{Value, json};
use std::sync::Arc;
pub const MAX_CHUNK_OUTPUTS: usize = 10_000;
fn invalid(message: impl Into<String>) -> IngestError {
    IngestError::Validation(message.into())
}
#[derive(Clone, Debug)]
pub struct ChunkingConfig {
    pub mode: String,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub max_chunk_size: usize,
    pub min_chunk_size: usize,
    pub use_token_count: bool,
    pub token_limit: usize,
    pub min_chunk_tokens: Option<usize>,
    pub max_chunk_tokens: Option<usize>,
    pub separators: Vec<String>,
    pub primary_separator: String,
    pub parent_chunk_size: usize,
    pub parent_overlap: usize,
    pub child_chunk_size: usize,
    pub child_overlap: usize,
    pub parent_mode: String,
    pub parent_token_limit: usize,
    pub child_token_limit: usize,
    pub remove_extra_spaces: bool,
    pub remove_urls_emails: bool,
    pub normalize_whitespace: bool,
    pub strip_html: bool,
    pub extract_metadata: bool,
    pub strict_section_traceability: bool,
    pub raw: Value,
}
fn n(v: &Value, key: &str, default: usize) -> usize {
    v[key].as_u64().map(|x| x as usize).unwrap_or(default)
}
fn b(v: &Value, key: &str, default: bool) -> bool {
    v[key].as_bool().unwrap_or(default)
}
impl ChunkingConfig {
    pub fn to_value(&self) -> Value {
        json!({"mode":self.mode,"chunk_size":self.chunk_size,"chunk_overlap":self.chunk_overlap,"max_chunk_size":self.max_chunk_size,"min_chunk_size":self.min_chunk_size,"use_token_count":self.use_token_count,"token_limit":self.token_limit,"min_chunk_tokens":self.min_chunk_tokens,"max_chunk_tokens":self.max_chunk_tokens,"separators":self.separators,"primary_separator":self.primary_separator,"regex_pattern":"","heading_patterns":self.raw.get("heading_patterns").cloned().unwrap_or(json!([])),"parent_chunk_size":self.parent_chunk_size,"parent_overlap":self.parent_overlap,"child_chunk_size":self.child_chunk_size,"child_overlap":self.child_overlap,"parent_mode":self.parent_mode,"parent_token_limit":self.parent_token_limit,"child_token_limit":self.child_token_limit,"preserve_images":b(&self.raw,"preserve_images",true),"image_context_chars":n(&self.raw,"image_context_chars",1000),"remove_extra_spaces":self.remove_extra_spaces,"remove_urls_emails":self.remove_urls_emails,"normalize_whitespace":self.normalize_whitespace,"strip_html":self.strip_html,"extract_metadata":self.extract_metadata,"metadata_fields":self.raw.get("metadata_fields").filter(|v|v.as_array().is_some_and(|v|!v.is_empty())).cloned().unwrap_or(json!(["title","author","date"])),"page_marker":self.raw["page_marker"].as_str().filter(|s|!s.is_empty()).unwrap_or(r"\f"),"strict_section_traceability":self.strict_section_traceability})
    }
    pub fn from_value(value: &Value) -> Result<Self> {
        validate_config(value)?;
        let mut v = object(value);
        // Python from_dict({}) returns the dataclass constructor directly.
        // Its overlap, token and hierarchy defaults differ from a nonempty
        // automatic-mode dictionary and must also be retained in previews.
        if v.as_object().is_some_and(|value| value.is_empty()) {
            v = json!({
                "mode":"automatic", "chunk_size":2000, "chunk_overlap":300,
                "token_limit":500, "parent_chunk_size":8000, "parent_overlap":400,
                "child_chunk_size":2000, "child_overlap":300, "parent_mode":"recursive",
                "parent_token_limit":1500, "child_token_limit":500,
                "metadata_fields":["title","author","date","keywords"],
                "separators":["\n\n\n","\n\n","\n","。",".","！","!","？","?","；",";","،",","," "],
                "heading_patterns":[r"^#{1,6}\s+.+$",r"^第[一二三四五六七八九十\d]+[章节条款]",r"^[A-Z][A-Z \t]{4,}:?$"]
            });
        }
        if !v["metadata_fields"]
            .as_array()
            .is_some_and(|fields| !fields.is_empty())
        {
            v["metadata_fields"] = json!(["title", "author", "date"]);
        }
        let raw_mode = v["mode"].as_str().unwrap_or("automatic");
        let mode = match raw_mode.to_lowercase().as_str() {
            "auto" => "automatic",
            "fixed" | "custom" => "fixed_size",
            "section" => "heading",
            "parent_child" => "hierarchical",
            _ => raw_mode,
        }
        .to_owned();
        let use_tokens = b(&v, "use_token_count", true);
        let explicit = v["token_limit"]
            .as_u64()
            .or_else(|| v["max_tokens"].as_u64())
            .or_else(|| v["segmentation"]["max_tokens"].as_u64())
            .or_else(|| {
                if mode == "fixed_size" && use_tokens {
                    v["chunk_size"].as_u64()
                } else {
                    None
                }
            });
        let auto_defaults = mode == "automatic"
            && explicit.is_none()
            && [
                "chunk_size",
                "chunk_overlap",
                "parent_chunk_size",
                "child_chunk_size",
                "parent_overlap",
                "child_overlap",
                "token_limit",
                "parent_token_limit",
                "child_token_limit",
            ]
            .iter()
            .all(|key| v.get(*key).is_none());
        let token_limit = if auto_defaults {
            400
        } else {
            explicit.unwrap_or(500) as usize
        };
        let overlap = v["chunk_overlap"]
            .as_u64()
            .or_else(|| v["overlap"].as_u64())
            .map(|x| x as usize)
            .unwrap_or_else(|| {
                if use_tokens {
                    if ["automatic", "hierarchical"].contains(&mode.as_str()) {
                        50
                    } else {
                        (token_limit / 10).max(20)
                    }
                } else {
                    300
                }
            });
        let child_limit = n(&v, "child_token_limit", token_limit);
        let parent_limit = n(
            &v,
            "parent_token_limit",
            if auto_defaults {
                1500
            } else {
                (child_limit * 4).max(900)
            },
        );
        let child_overlap = v["child_overlap"]
            .as_u64()
            .or_else(|| v["child_chunk_overlap"].as_u64())
            .map(|x| x as usize)
            .unwrap_or(overlap);
        let parent_overlap = v["parent_overlap"]
            .as_u64()
            .or_else(|| v["parent_chunk_overlap"].as_u64())
            .map(|x| x as usize)
            .unwrap_or(child_overlap);
        let config = Self {
            mode: mode.clone(),
            chunk_size: n(
                &v,
                "chunk_size",
                if use_tokens {
                    if mode == "fixed_size" {
                        token_limit
                    } else {
                        (token_limit * 4).max(1000)
                    }
                } else {
                    2000
                },
            ),
            chunk_overlap: overlap,
            max_chunk_size: n(&v, "max_chunk_size", 3000),
            min_chunk_size: n(&v, "min_chunk_size", 400),
            use_token_count: use_tokens,
            token_limit,
            min_chunk_tokens: v["min_chunk_tokens"].as_u64().map(|x| x as usize),
            max_chunk_tokens: v["max_chunk_tokens"].as_u64().map(|x| x as usize),
            separators: v["separators"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_else(|| {
                    ["\n\n\n", "\n\n", "\n", "。", ".", "！", "!", "？", "?", " "]
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                }),
            primary_separator: v["primary_separator"]
                .as_str()
                .or_else(|| v["separator"].as_str())
                .unwrap_or("\n\n")
                .into(),
            parent_chunk_size: n(&v, "parent_chunk_size", 8000),
            parent_overlap,
            child_chunk_size: n(&v, "child_chunk_size", 2000),
            child_overlap,
            parent_mode: v["parent_mode"]
                .as_str()
                .unwrap_or(if ["automatic", "hierarchical"].contains(&mode.as_str()) {
                    "fixed"
                } else {
                    "recursive"
                })
                .into(),
            parent_token_limit: parent_limit,
            child_token_limit: child_limit,
            remove_extra_spaces: b(&v, "remove_extra_spaces", true),
            remove_urls_emails: b(&v, "remove_urls_emails", false),
            normalize_whitespace: b(&v, "normalize_whitespace", true),
            strip_html: b(&v, "strip_html", false),
            extract_metadata: b(&v, "extract_metadata", false),
            strict_section_traceability: b(&v, "strict_section_traceability", false),
            raw: v,
        };
        Ok(config)
    }
    pub fn preprocess(&self, text: &str) -> Result<String> {
        require_text_budget(text)?;
        let mut out = text.to_owned();
        for (enabled, pattern, replacement) in [
            (self.strip_html, r"<[^>]+>", " "),
            (self.remove_urls_emails, r"https?://\S+|www\.\S+", " "),
            (
                self.remove_urls_emails,
                r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
                " ",
            ),
        ] {
            if enabled {
                out = regex::Regex::new(pattern)
                    .unwrap()
                    .replace_all(&out, replacement)
                    .into_owned()
            }
        }
        if self.normalize_whitespace {
            out = out.replace("\r\n", "\n").replace('\r', "\n");
            out = regex::Regex::new(r"\n{3,}")
                .unwrap()
                .replace_all(&out, "\n\n")
                .into_owned()
        }
        if self.remove_extra_spaces {
            out = regex::Regex::new(r"[ \t]+")
                .unwrap()
                .replace_all(&out, " ")
                .into_owned()
        }
        Ok(out.trim().into())
    }
}
pub fn validate_config(value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let Some(map) = value.as_object() else {
        return Err(invalid("chunking config must be an object"));
    };
    let allowed = [
        "mode",
        "chunk_size",
        "chunk_overlap",
        "overlap",
        "max_chunk_size",
        "min_chunk_size",
        "use_token_count",
        "token_limit",
        "max_tokens",
        "min_chunk_tokens",
        "max_chunk_tokens",
        "separators",
        "separator",
        "primary_separator",
        "keep_separator",
        "regex_pattern",
        "regex",
        "heading_level",
        "heading_patterns",
        "min_paragraph_length",
        "merge_short_paragraphs",
        "parent_chunk_size",
        "parent_overlap",
        "parent_chunk_overlap",
        "child_chunk_size",
        "child_overlap",
        "child_chunk_overlap",
        "parent_mode",
        "parent_token_limit",
        "child_token_limit",
        "question_prefix",
        "answer_prefix",
        "preserve_images",
        "image_context_chars",
        "remove_extra_spaces",
        "remove_urls_emails",
        "normalize_whitespace",
        "strip_html",
        "extract_metadata",
        "metadata_fields",
        "page_marker",
        "strict_section_traceability",
        "segmentation",
    ];
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(format!("unsupported chunking config field: {key}")));
        }
    }
    let mode = value["mode"]
        .as_str()
        .unwrap_or("automatic")
        .trim()
        .to_lowercase();
    if ![
        "automatic",
        "auto",
        "fixed_size",
        "fixed",
        "custom",
        "paragraph",
        "page",
        "heading",
        "section",
        "separator",
        "recursive",
        "hierarchical",
        "parent_child",
        "qa",
    ]
    .contains(&mode.as_str())
    {
        return Err(invalid("unsupported chunking mode"));
    }
    for key in ["regex", "regex_pattern"] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        {
            return Err(invalid("custom chunking regex patterns are disabled"));
        }
    }
    if value["heading_patterns"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        return Err(invalid("custom heading patterns are disabled"));
    }
    if let Some(marker) = value["page_marker"].as_str() {
        if ![r"\f", "\u{c}"].contains(&marker) {
            return Err(invalid("custom page markers are disabled"));
        }
    }
    for key in [
        "chunk_size",
        "chunk_overlap",
        "overlap",
        "max_chunk_size",
        "min_chunk_size",
        "token_limit",
        "max_tokens",
        "min_chunk_tokens",
        "max_chunk_tokens",
        "min_paragraph_length",
        "parent_chunk_size",
        "parent_overlap",
        "parent_chunk_overlap",
        "child_chunk_size",
        "child_overlap",
        "child_chunk_overlap",
        "parent_token_limit",
        "child_token_limit",
        "image_context_chars",
    ] {
        if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
            let (min, max) = match key {
                "chunk_overlap"
                | "overlap"
                | "parent_overlap"
                | "parent_chunk_overlap"
                | "child_overlap"
                | "child_chunk_overlap" => (0, 50_000),
                "image_context_chars" => (0, 1_000_000),
                "chunk_size" | "max_chunk_size" | "min_chunk_size" | "parent_chunk_size" => {
                    (50, 100_000)
                }
                "child_chunk_size" => (10, 100_000),
                _ => (1, 100_000),
            };
            if !v.as_u64().is_some_and(|v| v >= min && v <= max) {
                return Err(invalid(format!(
                    "chunking {key} must be an integer between {min} and {max}"
                )));
            }
        }
    }
    for key in [
        "use_token_count",
        "keep_separator",
        "merge_short_paragraphs",
        "preserve_images",
        "remove_extra_spaces",
        "remove_urls_emails",
        "normalize_whitespace",
        "strip_html",
        "extract_metadata",
        "strict_section_traceability",
    ] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err(invalid(format!("chunking {key} must be a boolean")));
        }
    }
    for (key, max) in [
        ("separator", 1024),
        ("primary_separator", 1024),
        ("heading_level", 32),
        ("parent_mode", 32),
        ("question_prefix", 128),
        ("answer_prefix", 128),
    ] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.as_str().is_some_and(|s| s.chars().count() <= max))
        {
            return Err(invalid(format!("chunking {key} is invalid")));
        }
    }
    for (key, max_len, item_max) in [("separators", 32, 1024), ("metadata_fields", 32, 64)] {
        if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
            let Some(a) = v.as_array() else {
                return Err(invalid(format!("chunking {key} is invalid")));
            };
            if a.len() > max_len
                || (key == "separators" && a.is_empty())
                || a.iter().any(|v| {
                    !v.as_str()
                        .is_some_and(|s| !s.is_empty() && s.chars().count() <= item_max)
                })
            {
                return Err(invalid(format!("chunking {key} is invalid")));
            }
        }
    }
    if let Some(seg) = value.get("segmentation").filter(|v| !v.is_null()) {
        if !seg.as_object().is_some_and(|m| {
            m.len() == 1
                && m.get("max_tokens")
                    .and_then(Value::as_u64)
                    .is_some_and(|n| (1..=100_000).contains(&n))
        }) {
            return Err(invalid("chunking segmentation is invalid"));
        }
    }
    let overlap = n(
        value,
        "chunk_overlap",
        n(
            value,
            "overlap",
            if b(value, "use_token_count", true) {
                50
            } else {
                300
            },
        ),
    );
    if overlap >= n(value, "chunk_size", 2000) {
        return Err(invalid("chunking overlap must be smaller than chunk_size"));
    }
    if n(value, "min_chunk_size", 400) > n(value, "max_chunk_size", 3000) {
        return Err(invalid("min_chunk_size must not exceed max_chunk_size"));
    }
    if n(
        value,
        "child_overlap",
        n(value, "child_chunk_overlap", overlap),
    ) >= n(value, "child_chunk_size", 2000)
    {
        return Err(invalid(
            "child overlap must be smaller than child_chunk_size",
        ));
    }
    if n(
        value,
        "parent_overlap",
        n(value, "parent_chunk_overlap", overlap),
    ) >= n(value, "parent_chunk_size", 8000)
    {
        return Err(invalid(
            "parent overlap must be smaller than parent_chunk_size",
        ));
    }
    Ok(())
}
#[derive(Clone)]
pub struct TokenCounter {
    encoder: Arc<tiktoken_rs::CoreBPE>,
}
fn arabic(c: char) -> bool {
    matches!(c as u32,0x600..=0x6ff|0x750..=0x77f|0x8a0..=0x8ff|0xfb50..=0xfdff|0xfe70..=0xfeff)
}
fn cjk(c: char) -> bool {
    matches!(c as u32,0x3400..=0x4dbf|0x4e00..=0x9fff)
}
impl TokenCounter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            encoder: Arc::new(
                tiktoken_rs::cl100k_base()
                    .map_err(|_| invalid("cl100k_base tokenizer is unavailable"))?,
            ),
        })
    }
    pub fn count(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        let total = text.chars().count();
        let ar = text.chars().filter(|c| arabic(*c)).count();
        if ar * 2 > total {
            let clean: String = text
                .chars()
                .filter(|c| !matches!(*c as u32, 0x64b..=0x652 | 0x670))
                .collect();
            let (mut arab, mut other) = (0, 0);
            for word in clean.split_whitespace() {
                if word.chars().any(arabic) {
                    arab += 1
                } else {
                    other += 1
                }
            }
            return (arab as f64 * 3.2 + other as f64 * 1.3) as usize;
        }
        let mut block = String::new();
        let mut count = 0;
        let mut chars = 0;
        for c in text.chars() {
            block.push(c);
            chars += 1;
            if chars == 65_536 {
                count += self.encoder.encode_ordinary(&block).len();
                block.clear();
                chars = 0
            }
        }
        count + self.encoder.encode_ordinary(&block).len()
    }
    pub fn fixed_windows(&self, text: &str, limit: usize, overlap: usize) -> Result<Vec<String>> {
        if text.is_empty() || limit == 0 {
            return Ok(vec![]);
        }
        let overlap = overlap.min(limit.saturating_sub(1));
        let step = limit - overlap;
        if text.len() > step.saturating_mul(MAX_CHUNK_OUTPUTS) {
            return Err(invalid("chunking output exceeds the 10000 chunk limit"));
        }
        if text.len() > 4 * 1024 * 1024 {
            return Err(invalid(
                "text exceeds the safe whole-tokenizer materialization limit",
            ));
        }
        let tokens = self.encoder.encode_ordinary(text);
        if tokens.len().div_ceil(step) > MAX_CHUNK_OUTPUTS {
            return Err(invalid("chunking output exceeds the 10000 chunk limit"));
        }
        // Original token byte offsets estimate budgets; every cut is scalar-safe.
        let mut offsets = vec![0usize];
        for bytes in self.encoder._decode_native_and_split(tokens.clone()) {
            let offset = *offsets.last().unwrap();
            let end = offset + bytes.len();
            if text.as_bytes().get(offset..end) != Some(bytes.as_slice()) {
                return Err(invalid("tokenizer bytes do not match source text"));
            }
            offsets.push(end);
        }
        if offsets.last().copied() != Some(text.len()) {
            return Err(invalid("tokenizer bytes do not match source text"));
        }
        let mut out = vec![];
        let (mut start, mut previous_end) = (0, 0);
        while start < text.len() {
            let start_token = offsets.partition_point(|offset| *offset <= start) - 1;
            let mut end = offsets[start_token.saturating_add(limit).min(tokens.len())];
            while end > start && !text.is_char_boundary(end) {
                end -= 1;
            }
            while end > start {
                if self.encoder.encode_ordinary(text[start..end].trim()).len() <= limit {
                    break;
                }
                end -= 1;
                while end > start && !text.is_char_boundary(end) {
                    end -= 1;
                }
            }
            // Reduce overlap if it would prevent coverage of new source text.
            if end <= previous_end && start < previous_end {
                start = previous_end;
                continue;
            }
            if end <= start {
                // A scalar may cross original tokens; measure it independently.
                end = start + 1;
                while end < text.len() && !text.is_char_boundary(end) {
                    end += 1;
                }
            }
            let value = text[start..end].trim();
            if self.encoder.encode_ordinary(value).len() > limit {
                return Err(invalid(
                    "token limit cannot contain the next Unicode character; increase token_limit",
                ));
            }
            if !value.is_empty() {
                push(&mut out, value.to_owned())?;
            }
            if end == text.len() {
                break;
            }
            previous_end = end;
            let end_token = offsets.partition_point(|offset| *offset < end);
            let mut next_start = end.min((start + 1).max(offsets[end_token.saturating_sub(overlap)]));
            while next_start < end && !text.is_char_boundary(next_start) {
                next_start += 1;
            }
            while next_start < end {
                if self.encoder.encode_ordinary(&text[next_start..end]).len() <= overlap {
                    break;
                }
                next_start += 1;
                while next_start < end && !text.is_char_boundary(next_start) {
                    next_start += 1;
                }
            }
            start = next_start;
        }
        Ok(out)
    }
}
fn push<T>(out: &mut Vec<T>, value: T) -> Result<()> {
    if out.len() >= MAX_CHUNK_OUTPUTS {
        return Err(invalid(format!(
            "chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )));
    }
    out.push(value);
    Ok(())
}
fn char_windows(text: &str, size: usize, overlap: usize, min: usize) -> Result<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let overlap = overlap.min(size / 2);
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + size).min(chars.len());
        if end < chars.len() {
            let search_start = end.saturating_sub(size / 5).max(start);
            let tail: String = chars[search_start..end].iter().collect();
            for sep in ["\n\n", "\n", "。", ".", "！", "!", "？", "?", "۔", "؟"] {
                if let Some(pos) = tail.rfind(sep).filter(|p| *p > 0) {
                    end = search_start + tail[..pos + sep.len()].chars().count();
                    break;
                }
            }
        }
        let piece: String = chars[start..end].iter().collect();
        if piece.trim().chars().count() >= min {
            push(&mut out, piece.trim().into())?
        }
        if end == chars.len() {
            break;
        }
        start = (start + size.saturating_sub(overlap).max(1)).max(end.saturating_sub(overlap));
    }
    Ok(out)
}
#[derive(Clone)]
pub struct Chunker {
    pub tokens: TokenCounter,
}
impl Chunker {
    pub fn new() -> Result<Self> {
        Ok(Self {
            tokens: TokenCounter::new()?,
        })
    }
    pub(super) fn split(&self, text: &str, c: &ChunkingConfig) -> Result<Vec<String>> {
        if c.use_token_count {
            self.tokens
                .fixed_windows(text, c.token_limit, c.chunk_overlap)
        } else {
            char_windows(text, c.chunk_size, c.chunk_overlap, c.min_chunk_size)
        }
    }
    fn measure(&self, text: &str, c: &ChunkingConfig) -> usize {
        if c.use_token_count {
            self.tokens.count(text)
        } else {
            text.chars().count()
        }
    }
    fn semantic_tokens(&self, text: &str, limit: usize, overlap: usize) -> Result<Vec<String>> {
        if text.is_empty() || limit == 0 {
            return Ok(vec![]);
        }
        let overlap = overlap.min(limit / 2);
        if text.len() > (limit - overlap).max(1) * MAX_CHUNK_OUTPUTS {
            return Err(invalid("chunking output exceeds the 10000 chunk limit"));
        }
        let re = regex::Regex::new(r"([.!?。！？۔؟]\s*|\n\n+)").unwrap();
        let mut sentences = vec![];
        let mut start = 0;
        for m in re.find_iter(text) {
            let value = &text[start..m.end()];
            if !value.trim().is_empty() {
                push(&mut sentences, value)?
            }
            start = m.end()
        }
        if !text[start..].trim().is_empty() {
            push(&mut sentences, &text[start..])?
        }
        let mut out = vec![];
        let mut current = String::new();
        let mut tokens = 0;
        let mut buffer: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
        let mut buffer_tokens = 0;
        for sentence in sentences {
            let count = self.tokens.count(sentence);
            if count > limit {
                if !current.trim().is_empty() {
                    push(&mut out, current.trim().to_owned())?;
                    current.clear();
                    tokens = 0;
                }
                let mut word_chunk = String::new();
                let mut word_tokens = 0;
                for word in sentence.split_whitespace() {
                    let count = self.tokens.count(&format!("{word} "));
                    if word_tokens + count > limit {
                        if !word_chunk.trim().is_empty() {
                            push(&mut out, word_chunk.trim().to_owned())?
                        }
                        word_chunk = format!("{word} ");
                        word_tokens = count;
                    } else {
                        word_chunk.push_str(word);
                        word_chunk.push(' ');
                        word_tokens += count;
                    }
                }
                if !word_chunk.trim().is_empty() {
                    current = word_chunk;
                    tokens = self.tokens.count(&current);
                }
                buffer.clear();
                buffer_tokens = 0;
                continue;
            }
            if tokens + count > limit {
                if !current.trim().is_empty() {
                    push(&mut out, current.trim().to_owned())?
                }
                if !buffer.is_empty() && buffer_tokens + count <= limit {
                    current = buffer.iter().copied().collect::<String>() + sentence;
                    tokens = buffer_tokens + count;
                } else {
                    current = sentence.into();
                    tokens = count;
                }
                buffer.clear();
                buffer.push_back(sentence);
                buffer_tokens = count;
            } else {
                current.push_str(sentence);
                tokens += count;
                buffer.push_back(sentence);
                buffer_tokens += count;
                while buffer_tokens > overlap && buffer.len() > 1 {
                    buffer_tokens = buffer_tokens
                        .saturating_sub(self.tokens.count(buffer.pop_front().unwrap()));
                }
            }
        }
        if !current.trim().is_empty() {
            push(&mut out, current.trim().to_owned())?
        }
        let mut final_chunks = vec![];
        for value in out {
            if self.tokens.count(&value) <= limit {
                push(&mut final_chunks, value)?;
                continue;
            }
            let mut buf = String::new();
            let mut tokens = 0;
            for word in value.split_whitespace() {
                let n = self.tokens.count(&format!("{word} "));
                if tokens + n > limit {
                    if !buf.trim().is_empty() {
                        push(&mut final_chunks, buf.trim().to_owned())?
                    }
                    buf = format!("{word} ");
                    tokens = n;
                } else {
                    buf.push_str(word);
                    buf.push(' ');
                    tokens += n;
                }
            }
            if !buf.trim().is_empty() {
                push(&mut final_chunks, buf.trim().to_owned())?
            }
        }
        Ok(final_chunks)
    }
    fn split_limits(&self, text: &str, c: &ChunkingConfig) -> Result<Vec<String>> {
        if !c.use_token_count {
            return char_windows(text, c.chunk_size, c.chunk_overlap, c.min_chunk_size);
        }
        let mut out = vec![];
        for part in self.semantic_tokens(text, c.token_limit, 50)? {
            if part.chars().count() <= c.chunk_size {
                push(&mut out, part)?
            } else {
                for sub in char_windows(&part, c.chunk_size, c.chunk_overlap, c.min_chunk_size)? {
                    push(&mut out, sub)?
                }
            }
        }
        Ok(out)
    }
    fn recursive(&self, text: &str, c: &ChunkingConfig, seps: &[String]) -> Result<Vec<String>> {
        self.recursive_depth(text, c, seps, 0)
    }
    fn recursive_depth(
        &self,
        text: &str,
        c: &ChunkingConfig,
        seps: &[String],
        depth: usize,
    ) -> Result<Vec<String>> {
        if text.trim().is_empty() {
            return Ok(vec![]);
        }
        if depth > 20 {
            return self.split_limits(text, c);
        }
        let chars = text.chars().count();
        let tokens = if c.use_token_count {
            self.tokens.count(text)
        } else {
            0
        };
        if (c.use_token_count && tokens <= c.token_limit && chars <= c.chunk_size)
            || (!c.use_token_count && chars <= c.chunk_size)
            || c.use_token_count
                && c.min_chunk_tokens.is_some_and(|m| tokens < m)
                && chars < c.chunk_size * 2
        {
            return Ok(vec![text.trim().into()]);
        }
        if c.use_token_count && text.len() > c.token_limit * MAX_CHUNK_OUTPUTS {
            return Err(invalid("chunking output exceeds the 10000 chunk limit"));
        }
        let Some((sep, rest)) = seps.split_first() else {
            return self.split_limits(text, c);
        };
        if sep.is_empty() {
            return self.split_limits(text, c);
        }
        if text.matches(sep).count() + 1 > 65_536 {
            return Err(invalid("literal split exceeds part limit"));
        }
        if !text.contains(sep) {
            return self.recursive_depth(text, c, rest, depth + 1);
        }
        let mut out: Vec<String> = vec![];
        let mut parts: Vec<&str> = vec![];
        let mut current_tokens = 0;
        let mut current_chars = 0;
        let sep_tokens = self.tokens.count(sep);
        let sep_chars = sep.chars().count();
        for raw in text.split(sep) {
            let part = if c.use_token_count { raw.trim() } else { raw };
            if c.use_token_count && part.is_empty() {
                continue;
            }
            let n = part.chars().count();
            let t = if c.use_token_count {
                self.tokens.count(part)
            } else {
                0
            };
            if n > c.chunk_size || c.use_token_count && t > c.token_limit {
                if !parts.is_empty() {
                    push(&mut out, parts.join(sep))?;
                    parts.clear();
                    current_tokens = 0;
                    current_chars = 0;
                }
                for sub in self.recursive_depth(part, c, rest, depth + 1)? {
                    push(&mut out, sub)?;
                }
                continue;
            }
            let projected_chars = current_chars
                + n
                + if c.use_token_count {
                    if parts.is_empty() { 0 } else { sep_chars }
                } else {
                    sep_chars
                };
            let projected_tokens =
                current_tokens + t + if parts.is_empty() { 0 } else { sep_tokens };
            if projected_chars > c.chunk_size
                || c.use_token_count && projected_tokens > c.token_limit
            {
                if !parts.is_empty() {
                    push(&mut out, parts.join(sep))?;
                }
                parts = vec![part];
                current_chars = n;
                current_tokens = t;
            } else {
                if !parts.is_empty() {
                    current_chars += sep_chars;
                    current_tokens += sep_tokens;
                }
                parts.push(part);
                current_chars += n;
                current_tokens += t;
            }
        }
        if !parts.is_empty() {
            let value = parts.join(sep);
            let merge = c.use_token_count
                && c.min_chunk_tokens
                    .is_some_and(|m| self.tokens.count(&value) < m)
                && c.max_chunk_tokens.is_some()
                && !out.is_empty();
            if merge {
                let combined = format!("{}{sep}{value}", out.last().unwrap());
                if self.tokens.count(&combined) <= c.max_chunk_tokens.unwrap() {
                    *out.last_mut().unwrap() = combined
                } else {
                    push(&mut out, value)?
                }
            } else {
                push(&mut out, value)?
            }
        }
        Ok(out)
    }
    fn named_parts(
        &self,
        text: &str,
        c: &ChunkingConfig,
        mode: &str,
    ) -> Result<Vec<(String, Value)>> {
        let mut out = vec![];
        let oversized = |text: &str| {
            if c.use_token_count {
                self.tokens.count(text) > c.token_limit
            } else {
                text.chars().count() > c.chunk_size
            }
        };
        let subparts = |text: &str| {
            if c.use_token_count {
                self.semantic_tokens(
                    text,
                    c.token_limit,
                    ((c.token_limit as f64 * 0.15) as usize).max(10),
                )
            } else {
                self.recursive(text, c, &c.separators)
            }
        };
        if mode == "paragraph" || mode == "qa" {
            let regex = regex::Regex::new(r"\n\s*\n").unwrap();
            if regex.find_iter(text).count() + 1 > 65_536 {
                return Err(invalid("paragraph split exceeds part limit"));
            }
            let mut current = String::new();
            let mut count = 0;
            for paragraph in regex.split(text).map(str::trim).filter(|s| !s.is_empty()) {
                let n = self.measure(paragraph, c);
                let fits = if c.use_token_count {
                    count + n <= c.token_limit
                } else {
                    current.chars().count() + paragraph.chars().count() + 2 <= c.chunk_size
                };
                if fits {
                    if !current.is_empty() {
                        current.push_str("\n\n")
                    }
                    current.push_str(paragraph);
                    count += n;
                } else {
                    if !current.is_empty() {
                        push(&mut out, (std::mem::take(&mut current), json!({})))?
                    }
                    if oversized(paragraph) {
                        let parts = if c.use_token_count {
                            subparts(paragraph)?
                        } else {
                            char_windows(
                                paragraph,
                                c.chunk_size,
                                c.chunk_overlap,
                                c.min_chunk_size,
                            )?
                        };
                        for part in parts {
                            push(&mut out, (part, json!({})))?
                        }
                        count = 0;
                    } else {
                        current = paragraph.into();
                        count = n;
                    }
                }
            }
            if !current.is_empty() {
                push(&mut out, (current, json!({})))?
            }
            return Ok(out);
        }
        if mode == "heading" {
            let re = regex::Regex::new(
                r"^#{1,6}\s+.+$|^第[一二三四五六七八九十\d]+[章节条款]|^[A-Z][A-Z \t]{4,}:?$",
            )
            .unwrap();
            let mut sections = vec![];
            let mut heading: Option<String> = None;
            let mut content = vec![];
            for line in text.split('\n') {
                if re.is_match(line) {
                    if heading.is_some() || !content.is_empty() {
                        sections.push((heading.take(), content.join("\n")));
                        content.clear()
                    }
                    heading = Some(line.into());
                } else {
                    content.push(line)
                }
            }
            if heading.is_some() || !content.is_empty() {
                sections.push((heading, content.join("\n")))
            }
            for (heading, content) in sections {
                let value = if let Some(h) = &heading {
                    format!("{h}\n\n{}", content.trim())
                } else {
                    content.trim().into()
                };
                if value.trim().is_empty() {
                    continue;
                }
                if oversized(&value) {
                    for (i, part) in subparts(&value)?.into_iter().enumerate() {
                        let mut m = json!({"heading":heading});
                        if c.use_token_count {
                            m["sub_chunk"] = json!(i + 1)
                        }
                        push(&mut out, (part, m))?
                    }
                } else {
                    push(&mut out, (value, json!({"heading":heading})))?
                }
            }
            return Ok(out);
        }
        if mode == "page" || mode == "separator" {
            let separator = if mode == "page" {
                "\u{c}"
            } else {
                &c.primary_separator
            };
            if separator.is_empty() {
                return self.named_parts(text, c, "recursive");
            }
            if text.matches(separator).count() + 1 > 65_536 {
                return Err(invalid("literal split exceeds part limit"));
            }
            for (i, part) in text.split(separator).enumerate() {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let meta = if mode == "page" {
                    json!({"page":i+1})
                } else {
                    json!({})
                };
                if oversized(part) {
                    let parts = if mode == "page" && !c.use_token_count {
                        char_windows(part, c.chunk_size, c.chunk_overlap, c.min_chunk_size)?
                    } else {
                        subparts(part)?
                    };
                    for (j, sub) in parts.into_iter().enumerate() {
                        let mut meta = meta.clone();
                        if mode == "page" {
                            meta["sub_chunk"] = json!(j + 1)
                        }
                        push(&mut out, (sub, meta))?
                    }
                } else {
                    push(&mut out, (part.into(), meta))?
                }
            }
            return Ok(out);
        }
        if mode == "image_aware" {
            let patterns = [
                r"\[Image\]",
                r"\[图片\]",
                r"!\[.*?\]\(.*?\)",
                r"<img[^>]+>",
                r"\[IMAGE:.*?\]",
            ];
            let chars: Vec<char> = text.chars().collect();
            let mut positions = vec![];
            for pattern in patterns {
                let re = regex::Regex::new(pattern).unwrap();
                for m in re.find_iter(text) {
                    push(
                        &mut positions,
                        (
                            text[..m.start()].chars().count(),
                            text[..m.end()].chars().count(),
                        ),
                    )?
                }
            }
            positions.sort_by_key(|p| p.0);
            let mut current = 0;
            let context = n(&c.raw, "image_context_chars", 200);
            for (start, end) in positions {
                if current < start {
                    let before: String = chars[current..start].iter().collect();
                    if before.trim().chars().count() > c.min_chunk_size {
                        for part in self.recursive(before.trim(), c, &c.separators)? {
                            push(&mut out, (part, json!({})))?
                        }
                    }
                }
                let mut a = start.saturating_sub(context);
                let mut b = (end + context).min(chars.len());
                while a > 0 && !".。!！?？\n".contains(chars[a]) {
                    a -= 1
                }
                while b < chars.len() && !".。!！?？\n".contains(chars[b - 1]) {
                    b += 1
                }
                let value: String = chars[a..b].iter().collect();
                if !value.trim().is_empty() {
                    push(
                        &mut out,
                        (
                            value.trim().into(),
                            json!({"has_image":true,"chunk_type":"image_context"}),
                        ),
                    )?
                }
                current = b;
            }
            if current < chars.len() {
                let value: String = chars[current..].iter().collect();
                if value.trim().chars().count() > c.min_chunk_size {
                    for part in self.recursive(value.trim(), c, &c.separators)? {
                        push(&mut out, (part, json!({})))?
                    }
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
        let parts = if mode == "fixed_size" {
            self.split(text, c)?
        } else {
            self.recursive(text, c, &c.separators)?
        };
        for part in parts {
            push(&mut out, (part, json!({})))?
        }
        Ok(out)
    }
    pub fn process_document(
        &self,
        text: &str,
        config: &ChunkingConfig,
        dataset_id: &str,
        document_id: &str,
        source_type: &str,
    ) -> Result<Vec<Chunk>> {
        let text = config.preprocess(text)?;
        if text.is_empty() {
            return Ok(vec![]);
        }
        let language = detect_language(&text);
        let mut output: Vec<Chunk> = vec![];
        let mut make = |text: String,
                        mut metadata: Value,
                        parent: Option<String>,
                        level: i32|
         -> Result<String> {
            let position = output.len() as i32;
            let text = sanitize_text(text.trim())?;
            let id = stable_segment_id(document_id, "text", position);
            let hash = sha256(text.as_bytes());
            metadata["document_id"] = json!(document_id);
            if config.strict_section_traceability && metadata.get("section_title").is_none() {
                metadata["section_title"] = json!(document_id)
            }
            let chunk = Chunk {
                segment_id: id.clone(),
                dataset_id: dataset_id.into(),
                document_id: document_id.into(),
                position,
                level,
                parent_segment_id: parent,
                token_count: self.tokens.count(&text) as i32,
                word_count: text.split_whitespace().count() as i32,
                text,
                metadata,
                enabled: true,
                status: "indexing".into(),
                content_type: "text".into(),
                content_hash: Some(hash.clone()),
                index_node_id: Some(stable_index_node_id(document_id, "text", position)),
                index_node_hash: Some(hash),
                source_type: Some(source_type.into()),
                language: Some(language.into()),
                keywords: json!([]),
                source_reference: json!({}),
                ..Chunk::default()
            };
            push(&mut output, chunk)?;
            Ok(id)
        };
        let detected_mode = if config.mode == "automatic" {
            let headings=regex::Regex::new(r"(?m)^#{1,6}\s+.+$|^第[一二三四五六七八九十\d]+[章节条款].*$|^[A-Z][A-Z \t]{4,}:?$").unwrap();
            if b(&config.raw, "preserve_images", true)
                && regex::Regex::new(r"\[Image\]|\[图片\]|!\[.*?\]\(.*?\)|<img[^>]+>|\[IMAGE:.*?\]")
                    .unwrap()
                    .is_match(&text)
            {
                "image_aware"
            } else if headings.find_iter(&text).count() >= 2 {
                "heading"
            } else if text.chars().count() > 5000 {
                "hierarchical"
            } else {
                "recursive"
            }
        } else {
            config.mode.as_str()
        };
        let mode = detected_mode;
        if ["automatic", "hierarchical"].contains(&mode) {
            let mut parent = config.clone();
            parent.token_limit = config.parent_token_limit;
            parent.chunk_size = config.parent_chunk_size;
            parent.chunk_overlap = config.parent_overlap;
            parent.use_token_count = true;
            parent.min_chunk_tokens = None;
            parent.max_chunk_tokens = None;
            let mut child = config.clone();
            child.token_limit = config.child_token_limit;
            child.chunk_size = config.child_chunk_size;
            child.chunk_overlap = config.child_overlap;
            child.use_token_count = true;
            child.min_chunk_tokens = None;
            child.max_chunk_tokens = None;
            let fixed =
                ["fixed", "fixed_size", "window", "token"].contains(&config.parent_mode.as_str());
            let parents = if fixed {
                self.split(&text, &parent)?
            } else {
                self.recursive(&text, &parent, &config.separators)?
            };
            for (i, p) in parents.into_iter().enumerate() {
                let parent_id = make(
                    p.clone(),
                    json!({"is_parent":true,"chunk_type":"parent","parent_index":i}),
                    None,
                    2,
                )?;
                let children = if fixed {
                    self.split(&p, &child)?
                } else {
                    self.recursive(&p, &child, &config.separators)?
                };
                for (j, c) in children.into_iter().enumerate() {
                    make(
                        c,
                        json!({"is_child":true,"chunk_type":"child","parent_index":i,"child_position":j}),
                        Some(parent_id.clone()),
                        3,
                    )?;
                }
            }
        } else {
            for (text, metadata) in self.named_parts(&text, config, mode)? {
                make(text, metadata, None, 3)?;
            }
        }
        if config.extract_metadata {
            let fields: Vec<String> = config.raw["metadata_fields"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![
                        "title".into(),
                        "author".into(),
                        "date".into(),
                        "keywords".into(),
                    ]
                });
            let metadata = extract_metadata(&text, &fields);
            for chunk in &mut output {
                if let (Some(dst), Some(src)) =
                    (chunk.metadata.as_object_mut(), metadata.as_object())
                {
                    dst.extend(src.clone());
                }
            }
        }
        Ok(output)
    }
}
pub fn detect_language(text: &str) -> &'static str {
    let sample: Vec<_> = text.chars().take(5000).collect();
    let total = sample.len().max(1);
    let ranges = [
        ("zh", sample.iter().filter(|c| cjk(**c)).count()),
        ("ar", sample.iter().filter(|c| arabic(**c)).count()),
        (
            "ru",
            sample
                .iter()
                .filter(|c| matches!(**c as u32, 0x400..=0x4ff))
                .count(),
        ),
        (
            "ko",
            sample
                .iter()
                .filter(|c| matches!(**c as u32,0xac00..=0xd7af|0x1100..=0x11ff))
                .count(),
        ),
        (
            "ja",
            sample
                .iter()
                .filter(|c| matches!(**c as u32, 0x3040..=0x30ff))
                .count(),
        ),
    ];
    let (lang, count) =
        ranges.into_iter().fold(
            ("en", 0),
            |best, item| if item.1 > best.1 { item } else { best },
        );
    if count as f64 / total as f64 > 0.05 {
        lang
    } else {
        "en"
    }
}

impl Chunker {
    /// `_process_standard_chunks`: flatten to leaves, merge outside fixed and
    /// hierarchical modes, then attach the canonical source traceability keys.
    pub fn standard_chunks(
        &self,
        text: &str,
        config: &ChunkingConfig,
        dataset: &str,
        document: &str,
        name: &str,
        source: &str,
    ) -> Result<Vec<Chunk>> {
        let all = self.process_document(text, config, dataset, document, source)?;
        let parents: std::collections::BTreeSet<_> = all
            .iter()
            .filter_map(|c| c.parent_segment_id.clone())
            .collect();
        let mut leaves: Vec<_> = all
            .into_iter()
            .filter(|c| !parents.contains(&c.segment_id))
            .collect();
        if !["fixed_size", "hierarchical"].contains(&config.mode.as_str()) {
            leaves = self.merge_small(leaves, config.min_chunk_size, config.max_chunk_size)?;
            leaves = self.merge_small(leaves, config.min_chunk_size, config.max_chunk_size)?;
        }
        for (position, c) in leaves.iter_mut().enumerate() {
            c.position = position as i32;
            c.level = 3;
            c.parent_segment_id = None;
            c.segment_id = stable_segment_id(document, "text", position as i32);
            c.index_node_id = Some(stable_index_node_id(document, "text", position as i32));
            let hash = sha256(c.text.as_bytes());
            c.content_hash = Some(hash.clone());
            c.index_node_hash = Some(hash);
            c.token_count = self.tokens.count(&c.text) as i32;
            c.word_count = c.text.split_whitespace().count() as i32;
            for (k, v) in [
                ("chunk_index", json!(position)),
                ("paragraph_index", json!(position)),
                ("source_document", json!(name)),
                ("source_document_id", json!(document)),
                ("source_dataset_id", json!(dataset)),
                ("source_type", json!(source)),
                ("language", json!(detect_language(&c.text))),
            ] {
                c.metadata[k] = v
            }
        }
        Ok(leaves)
    }
    pub fn merge_small(&self, chunks: Vec<Chunk>, min: usize, max: usize) -> Result<Vec<Chunk>> {
        self.merge_small_with_tokens(chunks, min, max, None, None)
    }
    pub fn merge_small_with_tokens(
        &self,
        chunks: Vec<Chunk>,
        min: usize,
        max: usize,
        min_tokens: Option<usize>,
        max_tokens: Option<usize>,
    ) -> Result<Vec<Chunk>> {
        if chunks.is_empty() || min == 0 {
            return Ok(chunks);
        }
        let small = |text: &str| {
            text.chars().count() < min || min_tokens.is_some_and(|n| self.tokens.count(text) < n)
        };
        let within = |text: &str| {
            let count = self.tokens.count(text);
            match max_tokens {
                None => text.chars().count() <= max,
                Some(limit) if count <= limit => text.chars().count() <= max,
                Some(limit) => text.chars().count() <= max + min && count <= limit.max(512),
            }
        };
        fn merge(engine: &Chunker, left: Chunk, right: Chunk, forward: bool) -> Chunk {
            let text = format!("{}\n\n{}", left.text, right.text.trim());
            let mut result = if forward { right.clone() } else { left.clone() };
            let (mut meta, extra) = if forward {
                (object(&left.metadata), object(&right.metadata))
            } else {
                (object(&right.metadata), object(&left.metadata))
            };
            meta.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let mut images = left
                .extra
                .get("associated_images")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            images.extend(
                right
                    .extra
                    .get("associated_images")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            result.text = text;
            result.metadata = meta;
            result.token_count = engine.tokens.count(&result.text) as i32;
            result.word_count = result.text.split_whitespace().count() as i32;
            result
                .extra
                .insert("char_count".into(), json!(result.text.chars().count()));
            result
                .extra
                .insert("associated_images".into(), json!(images));
            result
        }
        let mut merged: Vec<Chunk> = vec![];
        for row in chunks {
            if row.text.trim().is_empty() {
                continue;
            }
            if small(row.text.trim()) {
                if let Some(prev) = merged.last() {
                    if within(&format!("{}\n\n{}", prev.text, row.text.trim())) {
                        let prev = merged.pop().unwrap();
                        merged.push(merge(self, prev, row, false));
                        continue;
                    }
                }
            }
            push(&mut merged, row)?;
        }
        if merged.len() >= 2
            && small(&merged[0].text)
            && within(&format!("{}\n\n{}", merged[0].text, merged[1].text))
        {
            let first = merged.remove(0);
            let second = merged.remove(0);
            merged.insert(0, merge(self, first, second, true));
        }
        for _ in 0..10 {
            let mut changed = false;
            let mut next: Vec<Chunk> = vec![];
            for row in merged {
                if small(row.text.trim()) {
                    if let Some(prev) = next.last() {
                        if within(&format!("{}\n\n{}", prev.text, row.text.trim())) {
                            let prev = next.pop().unwrap();
                            next.push(merge(self, prev, row, false));
                            changed = true;
                            continue;
                        }
                    }
                }
                push(&mut next, row)?;
            }
            merged = next;
            if !changed {
                break;
            }
        }
        for (index, row) in merged.iter_mut().enumerate() {
            row.position = index as i32;
        }
        Ok(merged)
    }
}

pub fn extract_metadata(text: &str, fields: &[String]) -> Value {
    let mut m = json!({});
    let has = |key: &str| fields.iter().any(|s| s == key);
    if has("title") {
        if let Some(line) = text.trim_start().lines().next() {
            let line = line.trim();
            if !line.is_empty()
                && line.chars().count() < 200
                && (line.starts_with('#') || line.chars().count() < 100)
            {
                m["title"] = json!(line.trim_start_matches('#').trim())
            }
        }
    }
    if has("date") {
        let sample: String = text.chars().take(1000).collect();
        for pattern in [
            r"\d{4}[-/]\d{1,2}[-/]\d{1,2}",
            r"\d{1,2}[-/]\d{1,2}[-/]\d{4}",
        ] {
            if let Some(found) = regex::Regex::new(pattern).unwrap().find(&sample) {
                m["date"] = json!(found.as_str());
                break;
            }
        }
    }
    if has("language") {
        m["language"] = json!(detect_language(text))
    }
    if has("keywords") {
        let stop = [
            "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was",
            "one", "our", "out", "has", "have", "this", "that", "with", "from", "they", "been",
            "said", "each", "which", "their", "will", "other", "about", "many", "then", "them",
            "these", "some", "would", "make", "like", "into", "more", "than", "its", "over",
            "such", "also", "most",
        ];
        let mut words = Vec::<(String, usize)>::new();
        let mut positions = std::collections::BTreeMap::<String, usize>::new();
        for hit in regex::Regex::new(r"\b[a-zA-Z\x{4e00}-\x{9fff}]{3,}\b")
            .unwrap()
            .find_iter(text)
            .take(100000)
        {
            let word = hit.as_str().to_lowercase();
            if stop.contains(&word.as_str()) {
                continue;
            }
            if let Some(i) = positions.get(&word) {
                words[*i].1 += 1
            } else {
                positions.insert(word.clone(), words.len());
                words.push((word, 1));
            }
        }
        words.sort_by(|a, b| b.1.cmp(&a.1));
        m["keywords"] = json!(words.into_iter().take(10).map(|v| v.0).collect::<Vec<_>>());
    }
    if has("word_count") || has("char_count") {
        m["word_count"] = json!(text.split_whitespace().count());
        m["char_count"] = json!(text.chars().count())
    }
    m
}

#[cfg(test)]
mod unicode_token_windows_tests {
    use super::*;

    fn assert_coverage(counter: &TokenCounter, text: &str, chunks: &[String], limit: usize, overlap: usize) {
        let (mut previous_start, mut covered) = (None, 0);
        for chunk in chunks {
            assert!(!chunk.is_empty());
            assert!(counter.encoder.encode_ordinary(chunk).len() <= limit);
            let found = text.char_indices().map(|(index, _)| index).find(|start| {
                previous_start.is_none_or(|previous| *start > previous)
                    && *start <= covered
                    && text[*start..].starts_with(chunk)
                    && *start + chunk.len() > covered
                    && counter.encoder.encode_ordinary(&text[*start..covered]).len() <= overlap
            });
            let start = found.unwrap_or_else(|| {
                let start = text.len() - text[covered..].trim_start().len();
                assert!(text[start..].starts_with(chunk));
                start
            });
            assert!(start <= covered || text[covered..start].trim().is_empty());
            previous_start = Some(start);
            covered = start + chunk.len();
        }
        assert!(text[covered..].trim().is_empty());
        if !text.contains('\u{fffd}') {
            assert!(chunks.iter().all(|chunk| !chunk.contains('\u{fffd}')));
        }
    }

    #[test]
    fn shared_unicode_windows_preserve_coverage_and_strict_budget() {
        let counter = TokenCounter::new().unwrap();
        let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/knowledge/token_window_unicode.json"
        ))).unwrap();
        for case in cases {
            let text = case["text"].as_str().unwrap();
            let limit = case["limit"].as_u64().unwrap() as usize;
            let overlap = case["overlap"].as_u64().unwrap() as usize;
            let result = counter.fixed_windows(text, limit, overlap);
            if case["error"].as_bool().unwrap() {
                assert!(matches!(result, Err(IngestError::Validation(message)) if message.contains("increase token_limit")), "{}", case["name"]);
            } else {
                let chunks = result.unwrap();
                assert_coverage(&counter, text, &chunks, limit, overlap);
            }
        }
    }

    #[test]
    fn document_pipeline_preserves_realistic_unicode_boundary() {
        let chunker = Chunker::new().unwrap();
        let text = format!("{}世界😀", "hello ".repeat(510));
        let config = ChunkingConfig::from_value(&json!({
            "mode": "fixed_size", "use_token_count": true, "token_limit": 512,
            "chunk_overlap": 0, "min_chunk_size": 50, "remove_extra_spaces": false,
            "normalize_whitespace": false, "extract_metadata": false
        })).unwrap();
        let chunks = chunker.process_document(&text, &config, "dataset", "document", "text").unwrap();
        let values: Vec<String> = chunks.into_iter().map(|chunk| chunk.text).collect();
        assert_coverage(&chunker.tokens, &text, &values, 512, 0);
    }

    #[test]
    fn terminal_overlap_does_not_emit_duplicate_tail() {
        let counter = TokenCounter::new().unwrap();
        assert_eq!(counter.fixed_windows("hello world", 4, 3).unwrap(), vec!["hello world"]);
    }

    #[test]
    fn small_budget_overlap_cross_product() {
        let counter = TokenCounter::new().unwrap();
        for text in ["A世|B😀|Cक्षि|D\u{301}|E�Z", "甲x乙y🚀z丙", "qé:rالعربية:s"] {
            for limit in 1..=8 {
                for overlap in 0..limit {
                    match counter.fixed_windows(text, limit, overlap) {
                        Ok(chunks) => assert_coverage(&counter, text, &chunks, limit, overlap),
                        Err(IngestError::Validation(message)) => {
                            assert!(message.contains("increase token_limit"));
                            assert!(text.chars().any(|c| counter.encoder.encode_ordinary(&c.to_string()).len() > limit));
                        }
                        Err(error) => panic!("unexpected chunking error: {error:?}"),
                    }
                }
            }
        }
    }

}
