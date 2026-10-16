//! FastAPI/Pydantic-shaped request validation for the native management slices.
//! Validation finishes before permission lookups or persistence side effects.
use axum::{
    Json,
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
pub(super) struct SourceJson(pub Value);
impl<S: Send + Sync> FromRequest<S> for SourceJson {
    type Rejection = Response;
    async fn from_request(request: Request, state: &S) -> Result<Self, Response> {
        source_json(request, state, false).await.map(Self)
    }
}
async fn source_json<S: Send + Sync>(
    request: Request,
    state: &S,
    optional: bool,
) -> Result<Value, Response> {
    let json_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| {
            let mime = v
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            mime == "application/json"
                || (mime.starts_with("application/") && mime.ends_with("+json"))
        });
    let bytes = axum::body::Bytes::from_request(request, state)
        .await
        .map_err(IntoResponse::into_response)?;
    if bytes.is_empty() {
        if optional {
            return Ok(json!({}));
        }
        return Err(response(vec![error(
            "missing",
            json!(["body"]),
            "Field required",
            Value::Null,
            None,
        )]));
    }
    if !json_type {
        return Err(response(vec![error(
            "model_attributes_type",
            json!(["body"]),
            "Input should be a valid dictionary or object to extract fields from",
            json!(String::from_utf8_lossy(&bytes)),
            None,
        )]));
    }
    let parsed: Value = serde_json::from_slice(&bytes).map_err(|e| {
        let offset = bytes
            .split(|b| *b == b'\n')
            .take(e.line().saturating_sub(1))
            .map(|l| l.len() + 1)
            .sum::<usize>()
            + e.column().saturating_sub(1);
        let raw = e.to_string();
        let message = raw.split(" at line ").next().unwrap_or(&raw);
        let message = match message {
            "expected value" => "Expecting value",
            "expected `,` or `}`" | "expected `,` or `]`" => "Expecting ',' delimiter",
            "key must be a string" => "Expecting property name enclosed in double quotes",
            other => other,
        };
        response(vec![error(
            "json_invalid",
            json!(["body", offset]),
            "JSON decode error",
            json!({}),
            Some(json!({"error":message})),
        )])
    })?;
    if parsed.is_null() {
        if optional {
            return Ok(json!({}));
        }
        return Err(response(vec![error(
            "missing",
            json!(["body"]),
            "Field required",
            Value::Null,
            None,
        )]));
    }
    Ok(parsed)
}

pub(super) fn error(
    kind: &str,
    loc: Value,
    message: impl AsRef<str>,
    input: Value,
    ctx: Option<Value>,
) -> Value {
    let mut value = json!({"type":kind,"loc":loc,"msg":message.as_ref(),"input":input});
    if let Some(ctx) = ctx {
        value["ctx"] = ctx;
    }
    value
}
pub(super) fn response(errors: Vec<Value>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"detail":errors})),
    )
        .into_response()
}
struct Validator {
    value: Value,
    original: Value,
    loc: Vec<Value>,
    errors: Vec<Value>,
}
impl Validator {
    fn new(value: Value, loc: Vec<Value>) -> Self {
        Self {
            original: value.clone(),
            value,
            loc,
            errors: vec![],
        }
    }
    fn at(&self, key: &str) -> Value {
        let mut loc = self.loc.clone();
        loc.push(json!(key));
        json!(loc)
    }
    fn fail(&mut self, key: &str, kind: &str, message: impl AsRef<str>, ctx: Option<Value>) {
        self.errors.push(error(
            kind,
            self.at(key),
            message,
            if kind == "missing" {
                if self.loc.first().and_then(Value::as_str) == Some("query") {
                    Value::Null
                } else {
                    self.original.clone()
                }
            } else {
                self.original[key].clone()
            },
            ctx,
        ));
    }
    fn root(&mut self, message: &str) {
        self.errors.push(error(
            "value_error",
            json!(self.loc),
            format!("Value error, {message}"),
            self.original.clone(),
            Some(json!({"error":{}})),
        ));
    }
    fn object(&mut self) -> bool {
        if !self.value.is_object() {
            self.errors.push(error(
                "model_attributes_type",
                json!(self.loc),
                "Input should be a valid dictionary or object to extract fields from",
                self.original.clone(),
                None,
            ));
            false
        } else {
            true
        }
    }
    fn extra(&mut self, keys: &[&str]) {
        let names: Vec<_> = self
            .value
            .as_object()
            .into_iter()
            .flat_map(|m| m.keys())
            .filter(|k| !keys.contains(&k.as_str()))
            .cloned()
            .collect();
        for key in names {
            self.fail(
                &key,
                "extra_forbidden",
                "Extra inputs are not permitted",
                None,
            );
        }
    }
    fn selected(&mut self, key: &str, required: bool, nullable: bool) -> bool {
        match self.value.get(key) {
            None => {
                if required {
                    self.fail(key, "missing", "Field required", None);
                }
                false
            }
            Some(value) => !(nullable && value.is_null()),
        }
    }
    fn string(&mut self, key: &str, required: bool, nullable: bool, min: usize, max: usize) {
        if !self.selected(key, required, nullable) {
            return;
        }
        let Some(value) = self.value[key].as_str() else {
            self.fail(key, "string_type", "Input should be a valid string", None);
            return;
        };
        let n = value.chars().count();
        if n < min {
            self.fail(
                key,
                "string_too_short",
                format!(
                    "String should have at least {min} character{}",
                    if min == 1 { "" } else { "s" }
                ),
                Some(json!({"min_length":min})),
            );
        }
        if n > max {
            self.fail(
                key,
                "string_too_long",
                format!("String should have at most {max} characters"),
                Some(json!({"max_length":max})),
            );
        }
    }
    fn pattern(&mut self, key: &str, pattern: &str) {
        if self.value[key]
            .as_str()
            .is_some_and(|v| !regex::Regex::new(pattern).unwrap().is_match(v))
        {
            self.fail(
                key,
                "string_pattern_mismatch",
                format!("String should match pattern '{pattern}'"),
                Some(json!({"pattern":pattern})),
            );
        }
    }
    fn integer(&mut self, key: &str, required: bool, nullable: bool, min: i64, max: Option<i64>) {
        if !self.selected(key, required, nullable) {
            return;
        }
        let original = &self.value[key];
        let parsed = integer(original);
        let Some(value) = parsed else {
            let (kind, message) = if original.is_string() {
                (
                    "int_parsing",
                    "Input should be a valid integer, unable to parse string as an integer",
                )
            } else if original.is_number() {
                (
                    "int_from_float",
                    "Input should be a valid integer, got a number with a fractional part",
                )
            } else {
                ("int_type", "Input should be a valid integer")
            };
            self.fail(key, kind, message, None);
            return;
        };
        self.value[key] = json!(value);
        if value < min {
            self.fail(
                key,
                "greater_than_equal",
                format!("Input should be greater than or equal to {min}"),
                Some(json!({"ge":min})),
            );
        }
        if let Some(max) = max {
            if value > max {
                self.fail(
                    key,
                    "less_than_equal",
                    format!("Input should be less than or equal to {max}"),
                    Some(json!({"le":max})),
                );
            }
        }
    }
    fn boolean(&mut self, key: &str, nullable: bool) {
        if !self.selected(key, false, nullable) {
            return;
        }
        let value = &self.value[key];
        let parsed = match value {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => n.as_f64().and_then(|n| {
                if n == 0. {
                    Some(false)
                } else if n == 1. {
                    Some(true)
                } else {
                    None
                }
            }),
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "t" | "yes" | "y" | "on" => Some(true),
                "0" | "false" | "f" | "no" | "n" | "off" => Some(false),
                _ => None,
            },
            _ => None,
        };
        if let Some(value) = parsed {
            self.value[key] = json!(value);
        } else {
            let kind = if value.is_string() || value.is_number() {
                "bool_parsing"
            } else {
                "bool_type"
            };
            self.fail(
                key,
                kind,
                if kind == "bool_parsing" {
                    "Input should be a valid boolean, unable to interpret input"
                } else {
                    "Input should be a valid boolean"
                },
                None,
            );
        }
    }
    fn container(
        &mut self,
        key: &str,
        required: bool,
        nullable: bool,
        list: bool,
        min: usize,
        max: usize,
    ) -> bool {
        if !self.selected(key, required, nullable) {
            return false;
        }
        let n = if list {
            self.value[key].as_array().map(Vec::len)
        } else {
            self.value[key].as_object().map(serde_json::Map::len)
        };
        let Some(n) = n else {
            self.fail(
                key,
                if list { "list_type" } else { "dict_type" },
                if list {
                    "Input should be a valid list"
                } else {
                    "Input should be a valid dictionary"
                },
                None,
            );
            return false;
        };
        let kind = if list { "List" } else { "Dictionary" };
        if n < min {
            self.fail(
                key,
                "too_short",
                format!(
                    "{kind} should have at least {min} item{} after validation, not {n}",
                    if min == 1 { "" } else { "s" }
                ),
                Some(json!({"field_type":kind,"min_length":min,"actual_length":n})),
            );
        }
        if n > max {
            self.fail(
                key,
                "too_long",
                format!("{kind} should have at most {max} items after validation, not {n}"),
                Some(json!({"field_type":kind,"max_length":max,"actual_length":n})),
            );
        }
        true
    }
    fn strings(
        &mut self,
        key: &str,
        required: bool,
        nullable: bool,
        min: usize,
        max: usize,
        item_min: usize,
        item_max: usize,
    ) {
        if self.container(key, required, nullable, true, min, max) {
            for (index, value) in self.value[key].as_array().unwrap().iter().enumerate() {
                let mut loc = self.loc.clone();
                loc.extend([json!(key), json!(index)]);
                let issue = match value.as_str() {
                    None => Some(("string_type", "Input should be a valid string".into(), None)),
                    Some(s) if s.chars().count() < item_min => Some((
                        "string_too_short",
                        format!(
                            "String should have at least {item_min} character{}",
                            if item_min == 1 { "" } else { "s" }
                        ),
                        Some(json!({"min_length":item_min})),
                    )),
                    Some(s) if s.chars().count() > item_max => Some((
                        "string_too_long",
                        format!("String should have at most {item_max} characters"),
                        Some(json!({"max_length":item_max})),
                    )),
                    _ => None,
                };
                if let Some((kind, message, ctx)) = issue {
                    self.errors
                        .push(error(kind, json!(loc), message, value.clone(), ctx));
                }
            }
        }
    }
    fn finish(self) -> Result<Value, Response> {
        if self.errors.is_empty() {
            Ok(self.value)
        } else {
            Err(response(self.errors))
        }
    }
}
fn integer(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    if let Some(b) = value.as_bool() {
        return Some(i64::from(b));
    }
    if let Some(n) = value.as_f64() {
        return (n.is_finite() && n.fract() == 0. && n >= i64::MIN as f64 && n < i64::MAX as f64)
            .then_some(n as i64);
    }
    let raw = value.as_str()?.trim();
    let regex = regex::Regex::new(r"^[+-]?[0-9](?:_?[0-9])*(?:\.0+)?$").unwrap();
    if !regex.is_match(raw) {
        return None;
    }
    raw.split('.').next()?.replace('_', "").parse().ok()
}
pub(super) fn registry(value: Value) -> Result<Value, Response> {
    let mut v = Validator::new(value, vec![json!("body")]);
    if !v.object() {
        return v.finish();
    }
    v.integer("expected_revision", true, false, 0, None);
    if v.container("fields", false, false, true, 0, 32) {
        for (index, field) in v.value["fields"]
            .as_array()
            .unwrap()
            .clone()
            .into_iter()
            .enumerate()
        {
            let mut f = Validator::new(field, vec![json!("body"), json!("fields"), json!(index)]);
            if f.object() {
                f.string("name", true, false, 0, usize::MAX);
                f.pattern("name", r"^[a-z][a-z0-9_]{0,63}$");
                f.string("label", true, false, 1, 128);
                f.string("type", true, false, 0, usize::MAX);
                f.pattern("type", r"^(string|number|datetime)$");
                f.string("description", false, true, 0, 512);
                f.extra(&["name", "label", "type", "description"]);
            }
            v.errors.extend(f.errors);
        }
    }
    v.extra(&["expected_revision", "fields"]);
    v.finish()
}
pub(super) fn metadata_batch(value: Value) -> Result<Value, Response> {
    let mut v = Validator::new(value, vec![json!("body")]);
    if !v.object() {
        return v.finish();
    }
    v.strings("document_ids", true, false, 1, 500, 1, 255);
    v.container("metadata_patch", false, false, false, 0, 32);
    v.strings("metadata_remove", false, false, 0, 32, 0, usize::MAX);
    v.integer("metadata_schema_revision", true, false, 0, None);
    v.extra(&[
        "document_ids",
        "metadata_patch",
        "metadata_remove",
        "metadata_schema_revision",
    ]);
    if v.errors.is_empty() {
        if v.value["metadata_patch"]
            .as_object()
            .is_none_or(|m| m.is_empty())
            && v.value["metadata_remove"]
                .as_array()
                .is_none_or(|a| a.is_empty())
        {
            v.root("metadata_patch or metadata_remove is required");
        } else if let Some(ids) = v.value["document_ids"].as_array() {
            if ids
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != ids.len()
            {
                v.root("document_ids must be unique");
            }
        }
    }
    v.finish()
}
pub(super) fn document_update(value: Value) -> Result<Value, Response> {
    let mut v = Validator::new(value, vec![json!("body")]);
    if !v.object() {
        return v.finish();
    }
    v.string("title", false, true, 1, 512);
    v.container("metadata", false, true, false, 0, 64);
    v.container("metadata_patch", false, true, false, 0, 32);
    v.strings("metadata_remove", false, true, 0, 32, 0, usize::MAX);
    v.integer("metadata_schema_revision", false, true, 0, None);
    v.string("doc_type", false, true, 0, 64);
    v.string("doc_language", false, true, 0, 64);
    if v.errors.is_empty() {
        let patch = !v.value["metadata_patch"].is_null() || !v.value["metadata_remove"].is_null();
        if patch && !v.value["metadata"].is_null() {
            v.root("metadata replacement and metadata merge-patch are mutually exclusive");
        } else if patch
            && ["title", "doc_type", "doc_language"]
                .iter()
                .any(|k| !v.value[*k].is_null())
        {
            v.root("metadata merge-patch must be submitted separately");
        } else if patch && v.value["metadata_schema_revision"].is_null() {
            v.root("metadata_schema_revision is required for metadata merge-patch");
        } else if !patch && !v.value["metadata_schema_revision"].is_null() {
            v.root("metadata_schema_revision requires metadata_patch or metadata_remove");
        }
    }
    v.finish()
}
pub(super) fn page(raw: Option<&str>, cap: i64, datasets: bool) -> Result<Value, Response> {
    let mut query = json!({});
    if let Some(raw) = raw {
        if let Ok(url) = reqwest::Url::parse(&format!("http://query.invalid/?{raw}")) {
            for (key, value) in url.query_pairs() {
                query[key.as_ref()] = json!(value);
            }
        }
    }
    let mut v = Validator::new(query, vec![json!("query")]);
    v.integer("limit", false, false, 1, Some(cap));
    if datasets {
        v.string("cursor", false, true, 0, 1024);
        v.boolean("archived", false);
    } else {
        v.integer("offset", false, false, 0, None);
    }
    let mut out = v.finish()?;
    if out.get("limit").is_none() {
        out["limit"] = json!(cap);
    }
    if out.get("offset").is_none() {
        out["offset"] = json!(0);
    }
    if out.get("archived").is_none() {
        out["archived"] = json!(false);
    }
    Ok(out)
}
impl Validator {
    fn float(&mut self, key: &str, nullable: bool, min: f64, max: f64) {
        if !self.selected(key, false, nullable) {
            return;
        }
        let value = &self.value[key];
        let number = value
            .as_f64()
            .or_else(|| value.as_bool().map(|b| if b { 1. } else { 0. }))
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
        let Some(number) = number else {
            self.fail(
                key,
                if value.is_string() {
                    "float_parsing"
                } else {
                    "float_type"
                },
                if value.is_string() {
                    "Input should be a valid number, unable to parse string as a number"
                } else {
                    "Input should be a valid number"
                },
                None,
            );
            return;
        };
        if !number.is_finite() {
            self.fail(
                key,
                "finite_number",
                "Input should be a finite number",
                None,
            );
            return;
        }
        self.value[key] = json!(number);
        if number < min {
            self.fail(
                key,
                "greater_than_equal",
                format!("Input should be greater than or equal to {min}"),
                Some(json!({"ge":min})),
            );
        }
        if number > max {
            self.fail(
                key,
                "less_than_equal",
                format!("Input should be less than or equal to {max}"),
                Some(json!({"le":max})),
            );
        }
    }
}
pub(super) fn dataset_config(value: Value) -> Result<Value, Response> {
    let mut v = Validator::new(value, vec![json!("body")]);
    if !v.object() {
        return v.finish();
    }
    for key in ["chunking_config", "retrieval_config"] {
        if !v.selected(key, false, true) {
            continue;
        }
        let mut nested = Validator::new(v.value[key].clone(), vec![json!("body"), json!(key)]);
        if nested.object() {
            if key == "chunking_config" {
                chunking(&mut nested);
            } else {
                retrieval(&mut nested);
            }
        }
        v.value[key] = nested.value;
        v.errors.extend(nested.errors);
    }
    v.string("embedding_provider", false, true, 1, 64);
    v.string("embedding_model", false, true, 1, 256);
    v.integer("embedding_dimension", false, true, 1, Some(8192));
    v.finish()
}
fn retrieval(v: &mut Validator) {
    for key in ["mode", "fusion_strategy"] {
        v.string(key, false, false, 0, usize::MAX);
    }
    for (key, default, min, max) in [
        ("top_k", 5, 1, 100),
        ("vector_top_k", 20, 1, 1000),
        ("keyword_top_k", 20, 1, 1000),
        ("rrf_k", 60, 1, 10000),
    ] {
        v.integer(key, false, false, min, Some(max));
        let _ = default;
    }
    v.integer("rerank_top_n", false, true, 1, Some(1000));
    v.string("rerank_model", false, false, 1, 256);
    for key in [
        "vector_enabled",
        "keyword_enabled",
        "rerank_enabled",
        "mmr_enabled",
    ] {
        v.boolean(key, false);
    }
    v.float("score_threshold", true, 0., 1.);
    v.float("alpha", false, 0., 1.);
    v.float("mmr_lambda", false, 0., 1.);
}
fn chunking(v: &mut Validator) {
    v.string("mode", false, false, 1, 32);
    for (key, min, max, nullable) in [
        ("chunk_size", 50, 100000, false),
        ("chunk_overlap", 0, 50000, false),
        ("overlap", 0, 50000, true),
        ("max_chunk_size", 50, 100000, true),
        ("min_chunk_size", 50, 100000, true),
        ("token_limit", 1, 100000, true),
        ("max_tokens", 1, 100000, true),
        ("min_chunk_tokens", 1, 100000, true),
        ("max_chunk_tokens", 1, 100000, true),
        ("parent_token_limit", 1, 100000, true),
        ("child_token_limit", 1, 100000, true),
        ("min_paragraph_length", 1, 100000, true),
        ("parent_chunk_size", 50, 100000, true),
        ("parent_overlap", 0, 50000, true),
        ("parent_chunk_overlap", 0, 50000, true),
        ("child_chunk_size", 10, 100000, true),
        ("child_overlap", 0, 50000, true),
        ("child_chunk_overlap", 0, 50000, true),
        ("image_context_chars", 0, 1000000, true),
    ] {
        v.integer(key, false, nullable, min, Some(max));
    }
    for (key, max, nullable) in [
        ("separator", 1024, false),
        ("primary_separator", 1024, true),
        ("regex_pattern", 256, true),
        ("heading_level", 32, true),
        ("parent_mode", 32, true),
        ("question_prefix", 128, true),
        ("answer_prefix", 128, true),
        ("page_marker", 16, true),
    ] {
        v.string(key, false, nullable, 0, max);
    }
    for key in [
        "use_token_count",
        "remove_extra_spaces",
        "remove_urls_emails",
    ] {
        v.boolean(key, false);
    }
    for key in [
        "keep_separator",
        "merge_short_paragraphs",
        "normalize_whitespace",
        "strip_html",
        "extract_metadata",
        "strict_section_traceability",
        "preserve_images",
    ] {
        v.boolean(key, true);
    }
    for (key, max) in [
        ("separators", 32),
        ("heading_patterns", 20),
        ("metadata_fields", 32),
    ] {
        v.strings(key, false, true, 0, max, 0, usize::MAX);
    }
    if v.selected("segmentation", false, true) {
        let mut n = Validator::new(v.value["segmentation"].clone(), {
            let mut loc = v.loc.clone();
            loc.push(json!("segmentation"));
            loc
        });
        if n.object() {
            n.integer("max_tokens", true, false, 1, Some(100000));
            n.extra(&["max_tokens"]);
        }
        v.value["segmentation"] = n.value;
        v.errors.extend(n.errors);
    }
    v.extra(&[
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
        "parent_token_limit",
        "child_token_limit",
        "separators",
        "separator",
        "primary_separator",
        "keep_separator",
        "regex_pattern",
        "heading_level",
        "heading_patterns",
        "min_paragraph_length",
        "merge_short_paragraphs",
        "parent_mode",
        "parent_chunk_size",
        "parent_overlap",
        "parent_chunk_overlap",
        "child_chunk_size",
        "child_overlap",
        "child_chunk_overlap",
        "question_prefix",
        "answer_prefix",
        "remove_extra_spaces",
        "remove_urls_emails",
        "normalize_whitespace",
        "strip_html",
        "extract_metadata",
        "metadata_fields",
        "page_marker",
        "strict_section_traceability",
        "preserve_images",
        "image_context_chars",
        "segmentation",
    ]);
    if !v.errors.is_empty() {
        return;
    }
    let mode = v.value["mode"]
        .as_str()
        .unwrap_or("automatic")
        .trim()
        .to_lowercase();
    let safe_patterns = json!([
        r"^#{1,6}\s+.+$",
        r"^第[一二三四五六七八九十\d]+[章节条款]",
        r"^[A-Z][A-Z \t]{4,}:?$"
    ]);
    let message = if ![
        "automatic",
        "auto",
        "fixed_size",
        "fixed",
        "custom",
        "paragraph",
        "page",
        "heading",
        "section",
        "regex",
        "separator",
        "recursive",
        "hierarchical",
        "parent_child",
        "qa",
    ]
    .contains(&mode.as_str())
    {
        Some("unsupported chunking mode")
    } else if mode == "regex"
        || v.value["regex_pattern"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    {
        Some("custom regex chunking is disabled")
    } else if v.value["heading_patterns"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
        && v.value["heading_patterns"] != safe_patterns
    {
        Some("custom heading regex patterns are disabled")
    } else if v.value["page_marker"]
        .as_str()
        .is_some_and(|s| !["", r"\f", "\u{c}"].contains(&s))
    {
        Some("custom page marker regex is disabled")
    } else if v.value["separators"].as_array().is_some_and(|a| {
        a.is_empty()
            || a.iter().any(|s| {
                s.as_str()
                    .is_some_and(|s| s.is_empty() || s.chars().count() > 1024)
            })
    }) {
        Some("separators must contain non-empty strings up to 1024 chars")
    } else if v.value["metadata_fields"].as_array().is_some_and(|a| {
        a.iter().any(|s| {
            s.as_str()
                .is_some_and(|s| s.is_empty() || s.chars().count() > 64)
        })
    }) {
        Some("metadata_fields must contain non-empty names up to 64 chars")
    } else if v.value["overlap"]
        .as_i64()
        .or(v.value["chunk_overlap"].as_i64())
        .unwrap_or(300)
        >= v.value["chunk_size"].as_i64().unwrap_or(2000)
    {
        Some("chunk_overlap must be smaller than chunk_size")
    } else if greater(&v.value, "min_chunk_size", "max_chunk_size", false) {
        Some("min_chunk_size must not exceed max_chunk_size")
    } else if v.value["child_overlap"]
        .as_i64()
        .or(v.value["child_chunk_overlap"].as_i64())
        .zip(v.value["child_chunk_size"].as_i64())
        .is_some_and(|(a, b)| a >= b)
    {
        Some("child_overlap must be smaller than child_chunk_size")
    } else if v.value["parent_overlap"]
        .as_i64()
        .or(v.value["parent_chunk_overlap"].as_i64())
        .zip(v.value["parent_chunk_size"].as_i64())
        .is_some_and(|(a, b)| a >= b)
    {
        Some("parent_overlap must be smaller than parent_chunk_size")
    } else if greater(&v.value, "min_chunk_tokens", "max_chunk_tokens", false) {
        Some("min_chunk_tokens must not exceed max_chunk_tokens")
    } else {
        None
    };
    if let Some(message) = message {
        v.root(message);
    }
}
fn greater(value: &Value, a: &str, b: &str, equal: bool) -> bool {
    value[a]
        .as_i64()
        .zip(value[b].as_i64())
        .is_some_and(|(a, b)| a > b || (equal && a == b))
}

pub(super) const DATASET_CREATE: u8 = 1;
pub(super) const DATASET_UPDATE: u8 = 2;
pub(super) const TEXT_CREATE: u8 = 3;
pub(super) const URL_CREATE: u8 = 4;
pub(super) const DOCUMENT_BATCH: u8 = 5;
pub(super) const DATASET_ARCHIVE: u8 = 6;
pub(super) const DOCUMENT_ARCHIVE: u8 = 7;
pub(super) const PERMISSION_GRANT: u8 = 8;
pub(super) const ENABLED: u8 = 9;
pub(super) const SEGMENT_CREATE: u8 = 10;
pub(super) const SEGMENT_UPDATE: u8 = 11;
pub(super) const SEGMENTS_ENABLED: u8 = 12;
pub(super) const BATCH_REINDEX: u8 = 13;
pub(super) const BATCH_DELETE: u8 = 14;
pub(super) const PREVIEW: u8 = 15;
pub(super) const PREVIEW_CONFIGURED: u8 = 16;
pub(super) const DATASET_DELETE: u8 = 17;
pub(super) const RESTORE: u8 = 18;
pub(super) const TEXT_SOURCE_REPLACE: u8 = 19;
pub(super) const MAX_TEXT_SOURCE_REPLACE_BYTES: usize = 1024 * 1024;
pub(super) struct TypedJson<T, const KIND: u8>(pub T);
impl<S: Send + Sync, T: serde::de::DeserializeOwned + Send, const KIND: u8> FromRequest<S>
    for TypedJson<T, KIND>
{
    type Rejection = Response;
    async fn from_request(request: Request, state: &S) -> Result<Self, Response> {
        let SourceJson(value) = SourceJson::from_request(request, state).await?;
        let value = legacy_body(KIND, value)?;
        serde_json::from_value(value.clone())
            .map(Self)
            .map_err(|_| {
                response(vec![error(
                    "model_attributes_type",
                    json!(["body"]),
                    "Input should be a valid dictionary or object to extract fields from",
                    value,
                    None,
                )])
            })
    }
}
impl Validator {
    fn required_bool(&mut self, key: &str) {
        if self.selected(key, true, false) {
            self.boolean(key, false);
        }
    }
    fn field_value_error(&mut self, key: &str, message: &str) {
        self.fail(
            key,
            "value_error",
            format!("Value error, {message}"),
            Some(json!({"error":{}})),
        );
    }
    fn nested_process_rule(&mut self, key: &str) {
        if !self.selected(key, false, true) {
            return;
        }
        let mut n = Validator::new(self.value[key].clone(), {
            let mut loc = self.loc.clone();
            loc.push(json!(key));
            loc
        });
        if n.object() {
            n.string("mode", false, false, 0, usize::MAX);
            if n.container("pre_processing_rules", false, false, true, 0, usize::MAX) {
                for (index, rule) in n.value["pre_processing_rules"]
                    .as_array()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .enumerate()
                {
                    let mut r = Validator::new(rule, {
                        let mut loc = n.loc.clone();
                        loc.extend([json!("pre_processing_rules"), json!(index)]);
                        loc
                    });
                    if r.object() {
                        r.string("id", true, false, 0, usize::MAX);
                        r.boolean("enabled", false);
                    }
                    n.value["pre_processing_rules"][index] = r.value;
                    n.errors.extend(r.errors);
                }
            }
            if n.selected("segmentation", false, false) {
                let mut seg = Validator::new(n.value["segmentation"].clone(), {
                    let mut loc = n.loc.clone();
                    loc.push(json!("segmentation"));
                    loc
                });
                if seg.object() {
                    seg.string("separator", false, false, 0, usize::MAX);
                    seg.integer("max_tokens", false, false, i64::MIN, None);
                    seg.integer("chunk_overlap", false, false, i64::MIN, None);
                }
                n.value["segmentation"] = seg.value;
                n.errors.extend(seg.errors);
            }
            n.string("parent_mode", false, true, 0, usize::MAX);
            n.integer("child_chunk_size", false, true, i64::MIN, None);
        }
        self.value[key] = n.value;
        self.errors.extend(n.errors);
    }
}
fn text_create(v: &mut Validator) {
    v.string("title", true, false, 1, 512);
    v.string("content", true, false, 1, 200000);
    v.container("metadata", false, false, false, 0, 64);
    v.nested_process_rule("process_rule");
    v.string("doc_form", false, false, 0, usize::MAX);
    v.string("doc_language", false, true, 0, 64);
}
pub(super) fn legacy_body(kind: u8, value: Value) -> Result<Value, Response> {
    let mut v = Validator::new(value, vec![json!("body")]);
    if !v.object() {
        return v.finish();
    }
    match kind {
        DATASET_CREATE | DATASET_UPDATE => {
            let create = kind == DATASET_CREATE;
            if create {
                v.string("dataset_id", false, true, 1, 128);
            }
            v.string("name", create, !create, 1, 200);
            v.string("description", false, true, 0, 8000);
            for key in ["visibility", "kb_type", "use_case"] {
                v.string(key, false, !create, 0, usize::MAX);
            }
            v.string("embedding_provider", false, !create, 1, 64);
            v.string("embedding_model", false, !create, 1, 256);
            v.integer("embedding_dimension", false, true, 1, Some(8192));
            for key in ["embedding_config", "index_config"] {
                v.container(key, false, !create, false, 0, usize::MAX);
            }
            if create {
                v.string("collection_name", false, true, 1, 255);
            }
            v.string("indexing_technique", false, !create, 0, usize::MAX);
            if create {
                v.container("process_rule", false, true, false, 0, usize::MAX);
            }
            // DatasetUpdateSchema.model_dump(exclude_none=True).
            if !create {
                v.value
                    .as_object_mut()
                    .unwrap()
                    .retain(|_, value| !value.is_null());
            }
            // Deliberately keep omitted create keys omitted so the explicit
            // local-semantic defaults profile cannot replace a supplied tuple.
        }
        TEXT_CREATE => text_create(&mut v),
        URL_CREATE => {
            v.string("url", true, false, 1, 2048);
            v.string("title", false, true, 0, 512);
            v.container("metadata", false, false, false, 0, 64);
            v.nested_process_rule("process_rule");
        }
        DOCUMENT_BATCH => {
            if v.container("documents", true, false, true, 1, 50) {
                for (index, document) in v.value["documents"]
                    .as_array()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .enumerate()
                {
                    let mut d = Validator::new(
                        document,
                        vec![json!("body"), json!("documents"), json!(index)],
                    );
                    if d.object() {
                        text_create(&mut d);
                    }
                    v.value["documents"][index] = d.value;
                    v.errors.extend(d.errors);
                }
            }
            v.nested_process_rule("process_rule");
            v.string("batch_name", false, true, 0, 256);
            if v.errors.is_empty()
                && v.value["documents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d["content"].as_str().unwrap().chars().count())
                    .sum::<usize>()
                    > 2000000
            {
                v.root("batch document content exceeds 2000000 characters");
            }
        }
        DATASET_ARCHIVE | DOCUMENT_ARCHIVE => {
            v.required_bool("archived");
            v.string(
                "reason",
                false,
                true,
                0,
                if kind == DATASET_ARCHIVE { 500 } else { 255 },
            );
        }
        PERMISSION_GRANT => {
            for key in ["subject_type", "subject_id", "permission"] {
                v.string(key, true, false, 0, usize::MAX);
            }
        }
        ENABLED => v.required_bool("enabled"),
        SEGMENT_CREATE | SEGMENT_UPDATE => {
            v.string(
                if kind == SEGMENT_CREATE {
                    "content"
                } else {
                    "text"
                },
                true,
                false,
                1,
                200000,
            );
            v.string("answer", false, true, 0, 200000);
            let before = v.errors.len();
            v.strings("keywords", false, true, 0, 100, 0, usize::MAX);
            if v.errors.len() == before
                && v.value["keywords"].as_array().is_some_and(|items| {
                    items.iter().any(|v| {
                        v.as_str()
                            .is_some_and(|s| s.is_empty() || s.chars().count() > 256)
                    })
                })
            {
                v.field_value_error("keywords", "keywords must contain 1-256 characters");
            }
        }
        SEGMENTS_ENABLED => {
            let before = v.errors.len();
            v.strings("segment_ids", true, false, 1, 500, 0, usize::MAX);
            if v.errors.len() == before {
                if let Some(items) = v.value["segment_ids"].as_array() {
                    let ids: Vec<_> = items
                        .iter()
                        .map(|v| v.as_str().unwrap().trim().to_owned())
                        .collect();
                    if ids.iter().any(|s| s.is_empty() || s.chars().count() > 256) {
                        v.field_value_error(
                            "segment_ids",
                            "segment IDs must contain 1-256 characters",
                        );
                    } else {
                        v.value["segment_ids"] = json!(ids);
                    }
                }
            }
            v.required_bool("enabled");
        }
        BATCH_REINDEX | BATCH_DELETE => {
            if kind == BATCH_DELETE {
                v.value.as_object_mut().unwrap().remove("all_documents");
            }
            v.strings(
                "document_ids",
                kind == BATCH_DELETE,
                false,
                usize::from(kind == BATCH_DELETE),
                1000,
                1,
                255,
            );
            if kind == BATCH_REINDEX {
                v.boolean("all_documents", false);
            }
            if v.errors.is_empty() {
                let ids = v.value["document_ids"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if kind == BATCH_REINDEX
                    && v.value["all_documents"].as_bool().unwrap_or(false) == !ids.is_empty()
                {
                    v.root("choose exactly one of all_documents or document_ids");
                } else if ids
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != ids.len()
                {
                    v.root("document_ids must be unique");
                }
            }
        }
        PREVIEW | PREVIEW_CONFIGURED => {
            v.string("text", true, false, 1, 200000);
            let key = if kind == PREVIEW {
                "config"
            } else {
                "chunking_config"
            };
            if v.selected(key, false, true) {
                if kind == PREVIEW {
                    let mut n =
                        Validator::new(v.value[key].clone(), vec![json!("body"), json!(key)]);
                    if n.object() {
                        chunking(&mut n);
                    }
                    v.value[key] = n.value;
                    v.errors.extend(n.errors);
                } else {
                    v.container(key, false, true, false, 0, usize::MAX);
                }
            }
            if kind == PREVIEW {
                v.string("document_id", false, true, 1, 256);
            }
            if v.errors.is_empty() {
                let c = &v.value[key];
                if c["mode"]
                    .as_str()
                    .is_some_and(|m| m.trim().eq_ignore_ascii_case("regex"))
                    || ["regex_pattern", "regex", "heading_patterns", "page_marker"]
                        .iter()
                        .any(|key| truthy(&c[*key]))
                {
                    v.root("custom regex chunk preview is disabled");
                }
            }
        }
        DATASET_DELETE => {
            v.container(
                "_gateway_delete_confirmation",
                false,
                true,
                false,
                0,
                usize::MAX,
            );
            v.string("reason", false, true, 0, 500);
        }
        RESTORE => v.string("reason", false, true, 0, usize::MAX),
        TEXT_SOURCE_REPLACE => {
            v.string("content", true, false, 1, 1_000_000);
            v.integer(
                "expected_current_version",
                true,
                false,
                1,
                Some(i32::MAX as i64),
            );
            v.extra(&["content", "expected_current_version"]);
            if v.errors.is_empty() {
                let content = v.value["content"].as_str().unwrap();
                if content.as_bytes().len() > MAX_TEXT_SOURCE_REPLACE_BYTES {
                    v.field_value_error(
                        "content",
                        "text source replacement exceeds the 1 MiB byte limit",
                    );
                } else if content.trim().is_empty() {
                    v.field_value_error("content", "text source replacement cannot be blank");
                }
            }
        }
        _ => {}
    }
    v.finish()
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.),
    }
}

pub(super) const PERMISSION_QUERY: u8 = 1;
pub(super) const SOURCE_QUERY: u8 = 2;
pub(super) const COMPARE_QUERY: u8 = 3;
pub(super) const VERSIONS_QUERY: u8 = 4;
pub(super) const DEDUPE_QUERY: u8 = 5;
pub(super) struct TypedQuery<T, const KIND: u8>(pub T);
impl<S: Send + Sync, T: serde::de::DeserializeOwned + Send, const KIND: u8>
    axum::extract::FromRequestParts<S> for TypedQuery<T, KIND>
{
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Response> {
        let value = legacy_query(KIND, parts.uri.query())?;
        serde_json::from_value(value.clone())
            .map(Self)
            .map_err(|_| {
                response(vec![error(
                    "int_parsing",
                    json!(["query"]),
                    "Input should be a valid integer, unable to parse string as an integer",
                    value,
                    None,
                )])
            })
    }
}
pub(super) fn legacy_query(kind: u8, raw: Option<&str>) -> Result<Value, Response> {
    let mut value = json!({});
    if let Some(raw) = raw {
        if let Ok(url) = reqwest::Url::parse(&format!("http://query.invalid/?{raw}")) {
            for (k, v) in url.query_pairs() {
                value[k.as_ref()] = json!(v);
            }
        }
    }
    let mut v = Validator::new(value, vec![json!("query")]);
    match kind {
        PERMISSION_QUERY => {
            v.string("subject_type", true, false, 0, usize::MAX);
            v.string("subject_id", true, false, 0, usize::MAX);
        }
        SOURCE_QUERY => v.string("hash", true, false, 64, 64),
        COMPARE_QUERY => {
            v.integer("from_version", true, false, i64::MIN, None);
            v.integer("to_version", true, false, i64::MIN, None);
        }
        VERSIONS_QUERY => {
            v.integer("limit", false, false, 1, Some(100));
            v.integer("offset", false, false, 0, None);
        }
        DEDUPE_QUERY => v.boolean("dry_run", false),
        _ => {}
    }
    v.finish()
}

pub(super) struct OptionalTypedJson<T, const KIND: u8>(pub T);
impl<S: Send + Sync, T: serde::de::DeserializeOwned + Send, const KIND: u8> FromRequest<S>
    for OptionalTypedJson<T, KIND>
{
    type Rejection = Response;
    async fn from_request(request: Request, state: &S) -> Result<Self, Response> {
        let value = legacy_body(KIND, source_json(request, state, true).await?)?;
        serde_json::from_value(value.clone())
            .map(Self)
            .map_err(|_| {
                response(vec![error(
                    "model_attributes_type",
                    json!(["body"]),
                    "Input should be a valid dictionary or object to extract fields from",
                    value,
                    None,
                )])
            })
    }
}
/// Starlette's :int GET converter is digits-only and produces a route 404;
/// restore uses the ordinary typed path parameter and therefore emits a 422.
pub(super) struct VersionPath<const DIGITS_ONLY: bool>(pub (String, String, i32));
impl<S: Send + Sync, const DIGITS_ONLY: bool> axum::extract::FromRequestParts<S>
    for VersionPath<DIGITS_ONLY>
{
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Response> {
        let axum::extract::Path((dataset, document, raw)) =
            axum::extract::Path::<(String, String, String)>::from_request_parts(parts, state)
                .await
                .map_err(IntoResponse::into_response)?;
        let parsed = integer(&json!(raw)).and_then(|n| i32::try_from(n).ok());
        if DIGITS_ONLY
            && (!raw.bytes().all(|b| b.is_ascii_digit()) || raw.is_empty() || parsed.is_none())
        {
            return Err(
                (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
            );
        }
        match parsed {
            Some(number) => Ok(Self((dataset, document, number))),
            None => Err(response(vec![error(
                "int_parsing",
                json!(["path", "version_number"]),
                "Input should be a valid integer, unable to parse string as an integer",
                json!(raw),
                None,
            )])),
        }
    }
}

#[cfg(test)]
#[path = "knowledge_migration_parity_schema.rs"]
mod knowledge_migration_parity_schema;

#[cfg(test)]
mod text_source_replace_validation_tests {
    use super::*;

    #[test]
    fn text_source_replace_requires_bounded_text_and_positive_expected_version() {
        let valid = json!({"content":"next source", "expected_current_version":1});
        assert!(legacy_body(TEXT_SOURCE_REPLACE, valid).is_ok());

        for invalid in [
            json!({"content":"next source", "expected_current_version":0}),
            json!({"content":"   ", "expected_current_version":1}),
            json!({"content":"next source", "expected_current_version":1, "title":"ignored"}),
        ] {
            assert!(legacy_body(TEXT_SOURCE_REPLACE, invalid).is_err());
        }

        let oversized = "x".repeat(MAX_TEXT_SOURCE_REPLACE_BYTES + 1);
        assert!(legacy_body(
            TEXT_SOURCE_REPLACE,
            json!({"content":oversized, "expected_current_version":1})
        )
        .is_err());
    }
}
