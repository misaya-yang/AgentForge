//! Canonical parsing cascade, IR adapters and rendering. Optional native engines
//! use the same injected page-client boundary as the Python implementation.
use super::parser::{Block, DocIR, DocumentParser, PageIR, require_text_budget};
use super::{IngestError, Result, canonical_json, integer, object, sha256, string};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use tokio::sync::{Mutex, Semaphore};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageSignals {
    pub has_text_layer: bool,
    pub text_length: usize,
    pub image_count: usize,
    pub mime: Option<String>,
}
#[derive(Clone, Debug)]
pub struct PageJob {
    pub doc_id: String,
    pub page_number: usize,
    pub content_hash: String,
    pub image_bytes: Option<Vec<u8>>,
    pub text_layer: String,
    pub page_width: Option<f64>,
    pub page_height: Option<f64>,
    pub filename: String,
    pub mime: Option<String>,
    pub signals: PageSignals,
    pub options: Value,
}
impl PageJob {
    pub fn text(doc: &str, text: &str, mime: Option<String>) -> Self {
        Self {
            doc_id: doc.into(),
            page_number: 1,
            content_hash: sha256(text.as_bytes()),
            image_bytes: None,
            text_layer: text.into(),
            page_width: None,
            page_height: None,
            filename: String::new(),
            mime: mime.clone(),
            signals: PageSignals {
                has_text_layer: !text.trim().is_empty(),
                text_length: text.chars().count(),
                image_count: 0,
                mime,
            },
            options: json!({}),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CascadeStage {
    pub backend: String,
    #[serde(default)]
    pub min_confidence: f64,
    #[serde(default)]
    pub require_text_layer: bool,
    #[serde(default)]
    pub require_image: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CascadeConfig {
    pub stages: Vec<CascadeStage>,
    #[serde(default)]
    pub backend_options: BTreeMap<String, Value>,
    #[serde(default = "one")]
    pub parallelism: usize,
    #[serde(default)]
    pub allow_partial: bool,
}
fn one() -> usize {
    1
}
impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            stages: vec![
                CascadeStage {
                    backend: "text_layer".into(),
                    min_confidence: 0.0,
                    require_text_layer: true,
                    require_image: false,
                },
                CascadeStage {
                    backend: "legacy_ocr_vlm".into(),
                    min_confidence: 0.0,
                    require_text_layer: false,
                    require_image: true,
                },
                CascadeStage {
                    backend: "general_vlm_fallback".into(),
                    min_confidence: 0.0,
                    require_text_layer: false,
                    require_image: false,
                },
            ],
            backend_options: BTreeMap::new(),
            parallelism: 1,
            allow_partial: false,
        }
    }
}
fn invalid(field: &str, reason: &str) -> IngestError {
    IngestError::Validation(format!("index_config.parsing.{field}: {reason}"))
}
fn confidence(v: &Value) -> bool {
    v.as_f64()
        .is_some_and(|x| x.is_finite() && (0.0..=1.0).contains(&x))
}
pub fn resolve_config(index: &Value) -> Result<Option<CascadeConfig>> {
    let Some(raw) = index.get("parsing").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(map) = raw.as_object() else {
        return Err(invalid("config", "must be an object"));
    };
    if map
        .keys()
        .any(|k| !["enabled", "cascade"].contains(&k.as_str()))
    {
        return Err(invalid("config", "contains unsupported fields"));
    }
    let enabled = raw.get("enabled").cloned().unwrap_or(json!(false));
    if !enabled.is_boolean() {
        return Err(invalid("enabled", "must be a boolean"));
    }
    let cascade = raw
        .get("cascade")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(serde_json::to_value(CascadeConfig::default())?);
    let Some(map) = cascade.as_object() else {
        return Err(invalid("cascade", "must be an object"));
    };
    if map.keys().any(|k| {
        !["stages", "backend_options", "parallelism", "allow_partial"].contains(&k.as_str())
    }) {
        return Err(invalid("cascade", "contains unsupported fields"));
    }
    let known = [
        "text_layer",
        "legacy_ocr_vlm",
        "general_vlm_fallback",
        "mineru",
        "paddle_ppstructure_v3",
        "paddleocr_vl",
    ];
    let stages = cascade["stages"]
        .as_array()
        .filter(|a| (1..=8).contains(&a.len()))
        .ok_or_else(|| invalid("cascade.stages", "must contain 1..8 stages"))?;
    for (i, stage) in stages.iter().enumerate() {
        let field = format!("cascade.stages[{i}]");
        if !stage.as_object().is_some_and(|m| {
            m.keys().all(|k| {
                [
                    "backend",
                    "min_confidence",
                    "require_text_layer",
                    "require_image",
                ]
                .contains(&k.as_str())
            })
        }) {
            return Err(invalid(
                &field,
                "must be an object with supported stage fields",
            ));
        }
        if !stage["backend"]
            .as_str()
            .is_some_and(|s| known.contains(&s))
        {
            return Err(invalid(
                &format!("{field}.backend"),
                "unknown parser backend",
            ));
        }
        if stage.get("min_confidence").is_some_and(|v| !confidence(v)) {
            return Err(invalid(
                &format!("{field}.min_confidence"),
                "must be a finite number in 0..1",
            ));
        }
        for key in ["require_text_layer", "require_image"] {
            if stage.get(key).is_some_and(|v| !v.is_boolean()) {
                return Err(invalid(&format!("{field}.{key}"), "must be a boolean"));
            }
        }
    }
    if cascade
        .get("parallelism")
        .is_some_and(|v| !v.as_u64().is_some_and(|n| (1..=8).contains(&n)))
    {
        return Err(invalid("cascade.parallelism", "must be an integer in 1..8"));
    }
    if cascade
        .get("allow_partial")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid("cascade.allow_partial", "must be a boolean"));
    }
    if let Some(options) = cascade.get("backend_options") {
        let options = options
            .as_object()
            .ok_or_else(|| invalid("cascade.backend_options", "must be an object"))?;
        for (backend, values) in options {
            let values = values.as_object().ok_or_else(|| {
                invalid(
                    "cascade.backend_options",
                    "must map known backends to option objects",
                )
            })?;
            if !known.contains(&backend.as_str()) {
                return Err(invalid(
                    "cascade.backend_options",
                    "must map known backends to option objects",
                ));
            }
            let allowed: &[&str] = match backend.as_str() {
                "text_layer" => &["confidence_floor_chars", "preserve_boundaries"],
                "legacy_ocr_vlm" | "general_vlm_fallback" => &["confidence"],
                _ => &[],
            };
            if values.keys().any(|k| !allowed.contains(&k.as_str())) {
                return Err(invalid(
                    &format!("cascade.backend_options.{backend}"),
                    "unsupported or server-owned option",
                ));
            }
            if values.get("confidence").is_some_and(|v| !confidence(v)) {
                return Err(invalid(
                    "cascade.backend_options.confidence",
                    "must be a finite number in 0..1",
                ));
            }
            if values
                .get("preserve_boundaries")
                .is_some_and(|v| !v.is_boolean())
            {
                return Err(invalid(
                    "cascade.backend_options.text_layer.preserve_boundaries",
                    "must be a boolean",
                ));
            }
            if values
                .get("confidence_floor_chars")
                .is_some_and(|v| !v.as_u64().is_some_and(|n| (1..=100000).contains(&n)))
            {
                return Err(invalid(
                    "cascade.backend_options.text_layer.confidence_floor_chars",
                    "must be an integer in 1..100000",
                ));
            }
        }
    }
    if enabled != true {
        return Ok(None);
    }
    let mut config: CascadeConfig = serde_json::from_value(cascade)?;
    if config.stages.iter().any(|s| s.backend == "text_layer") {
        config
            .backend_options
            .entry("text_layer".into())
            .or_insert_with(|| json!({}))["preserve_boundaries"] = json!(true)
    }
    Ok(Some(config))
}
pub type PageClient =
    Arc<dyn Fn(PageJob) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync>;
#[derive(Clone, Default)]
pub struct BackendRegistry {
    pub clients: BTreeMap<String, PageClient>,
    pub versions: BTreeMap<String, String>,
}
impl BackendRegistry {
    pub fn register(&mut self, name: &str, version: &str, client: PageClient) {
        self.clients.insert(name.into(), client);
        self.versions.insert(name.into(), version.into());
    }
    pub fn register_tesseract(&mut self, parser: DocumentParser) {
        let client: PageClient = Arc::new(move |job| {
            let parser = parser.clone();
            Box::pin(async move {
                let bytes = job.image_bytes.ok_or_else(|| {
                    IngestError::Parser("legacy_ocr_vlm needs page image bytes".into())
                })?;
                let text =
                    tokio::task::spawn_blocking(move || parser.ocr_image_bytes(&bytes, true))
                        .await
                        .map_err(|_| IngestError::Parser("OCR task failed".into()))??;
                Ok(json!(text))
            })
        });
        self.register("legacy_ocr_vlm", "1", client)
    }
}
#[derive(Clone)]
pub struct ParserCascade {
    pub config: CascadeConfig,
    pub registry: BackendRegistry,
    cache: Arc<Mutex<BTreeMap<String, PageIR>>>,
    pub persistent: Option<PostgresPageCache>,
}
#[derive(Clone, Debug)]
pub struct PageOutcome {
    pub page: PageIR,
    pub backend: String,
    pub attempts: Vec<String>,
    pub from_cache: bool,
}
pub fn backend_version(name: &str, options: &Value) -> &'static str {
    match name {
        "text_layer" => {
            if options["preserve_boundaries"].as_bool() == Some(true) {
                "1-boundary"
            } else {
                "1"
            }
        }
        "legacy_ocr_vlm" => "1",
        "general_vlm_fallback" => "qwen3-vl",
        "mineru" => "3.0",
        "paddle_ppstructure_v3" => "ppstructurev3-1",
        "paddleocr_vl" => "paddleocr-vl-1.6",
        _ => "1",
    }
}
pub fn page_cache_key(
    hash: &str,
    page: usize,
    backend: &str,
    version: &str,
    config_hash: &str,
) -> String {
    sha256(format!("1|{hash}|{page}|{backend}|{version}|{config_hash}").as_bytes())
}
pub fn page_confidence(page: &PageIR) -> f64 {
    if let Some(score) = page.confidence {
        return score;
    }
    let scores: Vec<_> = page.blocks.iter().filter_map(|b| b.confidence).collect();
    if !scores.is_empty() {
        scores.iter().sum::<f64>() / scores.len() as f64
    } else if page.blocks.iter().any(|b| !b.text.trim().is_empty()) {
        1.0
    } else {
        0.0
    }
}
impl ParserCascade {
    pub fn new(config: CascadeConfig, registry: BackendRegistry) -> Result<Self> {
        if config.stages.is_empty() {
            return Err(IngestError::Parser("cascade config has no stages".into()));
        }
        Ok(Self {
            config,
            registry,
            cache: Arc::new(Mutex::new(BTreeMap::new())),
            persistent: None,
        })
    }
    fn version(&self, name: &str) -> String {
        self.registry
            .versions
            .get(name)
            .cloned()
            .unwrap_or_else(|| {
                backend_version(
                    name,
                    self.config
                        .backend_options
                        .get(name)
                        .unwrap_or(&Value::Null),
                )
                .into()
            })
    }
    pub fn bundle_version(&self) -> String {
        sha256(
            self.config
                .stages
                .iter()
                .map(|s| format!("{}={}", s.backend, self.version(&s.backend)))
                .collect::<Vec<_>>()
                .join("|")
                .as_bytes(),
        )[..16]
            .into()
    }
    pub async fn parse_page(&self, job: PageJob) -> Result<PageOutcome> {
        let mut attempts = vec![];
        let mut reasons = BTreeMap::new();
        let mut best: Option<(PageIR, f64, String, String)> = None;
        for stage in &self.config.stages {
            let name = &stage.backend;
            let options = self
                .config
                .backend_options
                .get(name)
                .cloned()
                .unwrap_or(json!({}));
            let version = self.version(name);
            if stage.require_text_layer && !job.signals.has_text_layer {
                continue;
            }
            if stage.require_image && job.image_bytes.is_none() && job.signals.image_count == 0 {
                continue;
            }
            if name == "text_layer" {
                if job.text_layer.trim().is_empty() {
                    continue;
                }
            } else if !self.registry.clients.contains_key(name) {
                continue;
            }
            if ["legacy_ocr_vlm", "general_vlm_fallback"].contains(&name.as_str())
                && job.image_bytes.is_none()
            {
                continue;
            }
            let hash = if job.content_hash.is_empty() {
                format!("doc:{}", job.doc_id)
            } else {
                job.content_hash.clone()
            };
            let key = page_cache_key(
                &hash,
                job.page_number,
                name,
                &version,
                string(&job.options, "parser_config_hash"),
            );
            if let Some(cache) = &self.persistent {
                if let Some(page) = cache.get(&key).await? {
                    return Ok(PageOutcome {
                        page,
                        backend: name.clone(),
                        attempts,
                        from_cache: true,
                    });
                }
            }
            if let Some(page) = self.cache.lock().await.get(&key).cloned() {
                return Ok(PageOutcome {
                    page,
                    backend: name.clone(),
                    attempts,
                    from_cache: true,
                });
            }
            let result: Result<PageIR> = async {
                if name == "text_layer" {
                    let text = &job.text_layer;
                    let blocks = if options["preserve_boundaries"].as_bool() == Some(true)
                        && !text.is_empty()
                    {
                        vec![make_block(
                            format!("p{}b0", job.page_number),
                            "text",
                            text,
                            0,
                            name,
                            &version,
                        )]
                    } else {
                        paragraph_blocks(text, job.page_number, name, &version)
                    };
                    let mut p = page(&job, name, &version, blocks);
                    p.confidence = Some(
                        if text.trim().chars().count()
                            >= options["confidence_floor_chars"].as_u64().unwrap_or(1) as usize
                        {
                            1.0
                        } else {
                            0.0
                        },
                    );
                    Ok(p)
                } else {
                    match (self.registry.clients.get(name).expect("registered client"))(job.clone())
                        .await
                    {
                        Err(e) => Err(e),
                        Ok(value) => {
                            if name == "legacy_ocr_vlm" {
                                let text = value.as_str().ok_or_else(|| {
                                    IngestError::Parser(
                                        "OCR client returned non-text output".into(),
                                    )
                                })?;
                                let mut p = page(
                                    &job,
                                    name,
                                    &version,
                                    paragraph_blocks(text, job.page_number, name, &version),
                                );
                                p.confidence = Some(if text.trim().is_empty() {
                                    0.0
                                } else {
                                    options["confidence"].as_f64().unwrap_or(0.8)
                                });
                                Ok(p)
                            } else if name == "general_vlm_fallback" {
                                let text = value.as_str().ok_or_else(|| {
                                    IngestError::Parser(
                                        "VLM client returned non-text output".into(),
                                    )
                                })?;
                                let blocks = markdown_to_blocks(text, name, &version)?;
                                let score = if blocks.is_empty() {
                                    0.0
                                } else {
                                    options["confidence"].as_f64().unwrap_or(0.7)
                                };
                                let mut p = page(&job, name, &version, blocks);
                                p.confidence = Some(score);
                                Ok(p)
                            } else {
                                payload_to_page(&value, &job, name, &version)
                            }
                        }
                    }
                }
            }
            .await;
            let mut parsed = match result {
                Ok(p) => p,
                Err(e) => {
                    attempts.push(name.clone());
                    reasons.insert(name.clone(), e.to_string());
                    continue;
                }
            };
            if parsed.parser.is_empty() {
                parsed.parser = name.clone()
            }
            if parsed.parser_version.is_empty() {
                parsed.parser_version = version.clone()
            }
            for block in &mut parsed.blocks {
                if block.parser.is_empty() {
                    block.parser = name.clone()
                }
                if block.parser_version.is_empty() {
                    block.parser_version = version.clone()
                }
            }
            let score = page_confidence(&parsed);
            if score >= stage.min_confidence {
                if let Some(cache) = &self.persistent {
                    cache.put(&key, &parsed).await?
                }
                self.cache.lock().await.insert(key, parsed.clone());
                return Ok(PageOutcome {
                    page: parsed,
                    backend: name.clone(),
                    attempts,
                    from_cache: false,
                });
            }
            attempts.push(name.clone());
            reasons.insert(
                name.clone(),
                format!("low-confidence {score:.2} < {:.2}", stage.min_confidence),
            );
            if best.as_ref().is_none_or(|(_, s, _, _)| score > *s) {
                best = Some((parsed, score, name.clone(), key))
            }
        }
        if let Some((mut page, _, backend, key)) = best {
            page.hard_page = true;
            page.metadata["cascade_attempts"] = json!(attempts);
            if let Some(cache) = &self.persistent {
                cache.put(&key, &page).await?
            }
            self.cache.lock().await.insert(key, page.clone());
            return Ok(PageOutcome {
                page,
                backend,
                attempts,
                from_cache: false,
            });
        }
        Err(IngestError::Parser(format!(
            "all cascade stages failed for page {}: {}",
            job.page_number,
            attempts
                .iter()
                .map(|a| format!("{a}: {}", reasons.get(a).map(String::as_str).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join("; ")
        )))
    }
    pub async fn parse_document(
        &self,
        doc_id: &str,
        mut jobs: Vec<PageJob>,
        content_hash: &str,
        filename: &str,
        mime: Option<String>,
    ) -> Result<DocIR> {
        jobs.sort_by_key(|j| j.page_number);
        let semaphore = Arc::new(Semaphore::new(self.config.parallelism.max(1)));
        let mut pending = tokio::task::JoinSet::new();
        for (index, job) in jobs.iter().cloned().enumerate() {
            let cascade = self.clone();
            let sem = semaphore.clone();
            pending.spawn(crate::migration_knowledge_query::model_plane::inherit(
                async move {
                    let permit = sem
                        .acquire_owned()
                        .await
                        .map_err(|_| IngestError::Cancelled)?;
                    let result = cascade.parse_page(job).await;
                    drop(permit);
                    Ok::<_, IngestError>((index, result))
                },
            ));
        }
        let mut results = BTreeMap::new();
        while let Some(result) = pending.join_next().await {
            let (index, page) =
                result.map_err(|_| IngestError::Parser("page parser task failed".into()))??;
            results.insert(index, page);
        }
        let mut doc = DocIR {
            doc_id: doc_id.into(),
            content_hash: content_hash.into(),
            filename: filename.into(),
            mime,
            schema_version: "1".into(),
            pages: vec![],
            metadata: json!({"parser_bundle":self.bundle_version()}),
        };
        let mut failed = vec![];
        for (i, job) in jobs.iter().enumerate() {
            let result = results
                .remove(&i)
                .ok_or_else(|| IngestError::Parser("missing page parsing result".into()))?;
            let page = match result {
                Ok(mut out) => {
                    if out.page.metadata.get("served_by").is_none() {
                        out.page.metadata["served_by"] = json!(out.backend)
                    }
                    if out.from_cache {
                        out.page.metadata["from_cache"] = json!(true)
                    }
                    out.page
                }
                Err(e) => {
                    if !self.config.allow_partial {
                        return Err(e);
                    }
                    failed.push(job.page_number);
                    let mut p = page(job, "", "", vec![]);
                    p.hard_page = true;
                    p.metadata = json!({"error":e.to_string()});
                    p
                }
            };
            doc.pages.push(page)
        }
        if !failed.is_empty() {
            doc.metadata["failed_pages"] = json!(failed)
        }
        Ok(doc)
    }
}
fn make_block(
    id: String,
    kind: &str,
    text: &str,
    order: usize,
    backend: &str,
    version: &str,
) -> Block {
    Block {
        block_id: id,
        kind: kind.into(),
        text: text.into(),
        order,
        bbox: None,
        parser: backend.into(),
        parser_version: version.into(),
        confidence: None,
        table: None,
        formula: None,
        figure: None,
        metadata: json!({}),
    }
}
fn paragraph_blocks(text: &str, page: usize, backend: &str, version: &str) -> Vec<Block> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .enumerate()
        .map(|(order, text)| {
            make_block(
                format!("p{page}b{order}"),
                "text",
                text,
                order,
                backend,
                version,
            )
        })
        .collect()
}
fn page(job: &PageJob, backend: &str, version: &str, blocks: Vec<Block>) -> PageIR {
    PageIR {
        page_number: job.page_number,
        blocks,
        parser: backend.into(),
        parser_version: version.into(),
        page_width: job.page_width,
        page_height: job.page_height,
        confidence: None,
        hard_page: false,
        metadata: json!({}),
    }
}
pub fn payload_to_page(
    payload: &Value,
    job: &PageJob,
    backend: &str,
    version: &str,
) -> Result<PageIR> {
    let paddle = backend.starts_with("paddle");
    let empty = vec![];
    let raw = payload[if paddle { "regions" } else { "blocks" }]
        .as_array()
        .unwrap_or(&empty);
    if raw.len() > 10_000 {
        return Err(IngestError::Parser(
            "parser output exceeds block budget".into(),
        ));
    }
    let mut blocks = vec![];
    for (i, r) in raw.iter().enumerate() {
        let label = r[if paddle { "label" } else { "type" }]
            .as_str()
            .unwrap_or("text")
            .to_lowercase();
        let kind = match label.as_str() {
            "text" | "abstract" | "content" | "header" | "footer" => "text",
            "title" | "heading" | "paragraph_title" => "heading",
            "table" => "table",
            "formula" | "equation" | "display_formula" | "inline_formula" => "formula",
            "figure" | "image" | "chart" => "figure",
            "list" | "reference" => "list",
            "code" => "code",
            "caption" | "table_caption" | "figure_caption" => "caption",
            _ => "unknown",
        };
        let order = if paddle {
            i
        } else {
            r["order"].as_u64().unwrap_or(i as u64) as usize
        };
        let mut b = make_block(
            if paddle {
                format!("{backend}-{i}")
            } else {
                format!("mineru-{order}")
            },
            kind,
            string(r, "text"),
            order,
            backend,
            version,
        );
        require_text_budget(&b.text)?;
        if let Some(a) = r["bbox"].as_array().filter(|a| a.len() >= 4) {
            b.bbox = Some(json!({"x0":a[0],"y0":a[1],"x1":a[2],"y1":a[3]}))
        }
        b.confidence = r[if paddle { "confidence" } else { "score" }].as_f64();
        match kind {
            "table" => {
                let md = if b.text.is_empty() {
                    string(r, "markdown")
                } else {
                    &b.text
                };
                let html = r["html"].clone();
                b.table = Some(
                    json!({"markdown":md,"html":html,"n_rows":r[if paddle{"rows"}else{"n_rows"}].as_i64().unwrap_or_else(||if paddle{0}else{md.lines().filter(|s|s.trim().starts_with('|')).count() as i64}),"n_cols":integer(r,if paddle{"cols"}else{"n_cols"}),"header_rows":1,"has_merged_cells":!paddle&&(r["has_merged_cells"].as_bool()==Some(true)||html.as_str().is_some_and(|s|s.contains("<td colspan")||s.contains("<td rowspan"))),"column_semantics":r["column_semantics"].as_array().cloned().unwrap_or_default(),"summary":r["summary"]}),
                );
            }
            "formula" => {
                b.formula = Some(
                    json!({"latex":if string(r,"latex").is_empty(){b.text.as_str()}else{string(r,"latex")},"display":if paddle{!label.contains("inline")}else{r["display"].as_bool().unwrap_or(true)}}),
                )
            }
            "figure" if !paddle => {
                let image = &r["image"];
                b.figure = Some(
                    json!({"image_ref":image.get("ref").or_else(||r.get("image_ref")),"mime":image["mime"],"description":image.get("description").or_else(||r.get("text"))}),
                );
                if b.text.is_empty() {
                    b.text = string(image, "description").into()
                }
            }
            _ => {}
        }
        blocks.push(b);
    }
    let mut result = page(job, backend, version, blocks);
    result.page_width = payload["width"].as_f64();
    result.page_height = payload["height"].as_f64();
    if !paddle {
        result.confidence = payload["score"].as_f64()
    }
    Ok(result)
}
pub fn markdown_to_blocks(markdown: &str, backend: &str, version: &str) -> Result<Vec<Block>> {
    require_text_budget(markdown)?;
    let lines: Vec<_> = markdown.lines().collect();
    let heading = regex::Regex::new(r"^(#{1,6})\s+(.*)$").unwrap();
    let table = regex::Regex::new(r"^\s*\|.*\|\s*$").unwrap();
    let formula = regex::Regex::new(r"(?s)\$\$(.+?)\$\$").unwrap();
    let image = regex::Regex::new(r"!\[([^\]]*)\]\(([^)]*)\)").unwrap();
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() {
            i += 1;
            continue;
        }
        let mut b = make_block(
            format!("{backend}-{}", out.len()),
            "text",
            line,
            out.len(),
            backend,
            version,
        );
        if let Some(c) = heading.captures(line) {
            b.kind = "heading".into();
            b.text = c[2].into();
            b.metadata = json!({"level":c[1].len()});
            i += 1;
        } else if table.is_match(line) {
            let mut rows = vec![];
            while i < lines.len() && table.is_match(lines[i].trim()) {
                rows.push(lines[i].trim());
                i += 1
            }
            let md = rows.join("\n");
            b.kind = "table".into();
            b.text = md.clone();
            b.table = Some(
                json!({"markdown":md,"html":null,"n_rows":rows.iter().filter(|r|!r.contains("--")).count() as i64-1,"n_cols":rows.first().map(|s|s.matches('|').count().saturating_sub(1)).unwrap_or(0),"header_rows":1,"has_merged_cells":false,"column_semantics":[],"summary":null}),
            );
        } else if line.starts_with("$$") {
            let mut buf = vec![line];
            while !formula.is_match(&buf.join("\n")) && i + 1 < lines.len() {
                i += 1;
                buf.push(lines[i].trim())
            }
            let joined = buf.join("\n");
            let latex = formula
                .captures(&joined)
                .map(|c| c[1].trim().to_owned())
                .unwrap_or_else(|| joined.trim_matches(['$', ' ']).into());
            b.kind = "formula".into();
            b.text = latex.clone();
            b.formula = Some(json!({"latex":latex,"display":true}));
            i += 1;
        } else {
            let mut para = vec![line];
            while i + 1 < lines.len() {
                let next = lines[i + 1].trim();
                if next.is_empty()
                    || heading.is_match(next)
                    || table.is_match(next)
                    || next.starts_with("$$")
                {
                    break;
                }
                para.push(next);
                i += 1
            }
            let text = para.join("\n");
            if let Some(c) = image.captures(&text) {
                b.kind = "figure".into();
                b.text = c[1].into();
                b.figure = Some(
                    json!({"image_ref":if c[2].is_empty(){None}else{Some(c[2].to_owned())},"description":if c[1].is_empty(){None}else{Some(c[1].to_owned())},"mime":null}),
                );
            } else {
                b.text = text
            }
            i += 1
        }
        if out.len() >= 10_000 {
            return Err(IngestError::Parser(
                "parser output exceeds block budget".into(),
            ));
        }
        out.push(b);
    }
    Ok(out)
}
pub fn render_block_markdown(b: &Block) -> String {
    match b.kind.as_str() {
        "heading" => format!(
            "{} {}",
            "#".repeat(integer(&b.metadata, "level").clamp(1, 6) as usize),
            b.text.trim()
        ),
        "table" => b
            .table
            .as_ref()
            .map(|t| {
                if !string(t, "markdown").is_empty() {
                    string(t, "markdown").into()
                } else if t["html"].as_str().is_some_and(|s| !s.is_empty()) {
                    t["summary"].as_str().unwrap_or("[table]").into()
                } else {
                    b.text.clone()
                }
            })
            .unwrap_or_else(|| b.text.clone()),
        "formula" => b
            .formula
            .as_ref()
            .map(|f| {
                let d = if f["display"].as_bool().unwrap_or(true) {
                    "$$"
                } else {
                    "$"
                };
                format!("{d}{}{d}", string(f, "latex"))
            })
            .unwrap_or_else(|| b.text.clone()),
        "figure" => b
            .figure
            .as_ref()
            .map(|f| {
                format!(
                    "![{}]({})",
                    f["description"]
                        .as_str()
                        .unwrap_or(&b.text)
                        .replace('\n', " ")
                        .trim(),
                    string(f, "image_ref")
                )
            })
            .unwrap_or_else(|| b.text.clone()),
        "code" => format!("```\n{}\n```", b.text),
        _ => b.text.clone(),
    }
}
pub fn render_document_markdown(doc: &DocIR, page_markers: bool) -> String {
    let mut pages: Vec<_> = doc.pages.iter().collect();
    pages.sort_by_key(|p| p.page_number);
    pages
        .into_iter()
        .map(|p| {
            let mut blocks: Vec<_> = p.blocks.iter().collect();
            blocks.sort_by_key(|b| b.order);
            let body = blocks
                .into_iter()
                .map(render_block_markdown)
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if page_markers {
                format!("[Page {}]\n{body}", p.page_number)
            } else {
                body
            }
        })
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}
pub fn citation_anchor(b: &Block, page: usize) -> Value {
    json!({"block_id":b.block_id,"page_number":page,"bbox":b.bbox,"parser":b.parser,"parser_version":b.parser_version})
}
pub fn iter_chunk_payloads(doc: &DocIR) -> Vec<Value> {
    let mut pages: Vec<_> = doc.pages.iter().collect();
    pages.sort_by_key(|p| p.page_number);
    pages
        .into_iter()
        .flat_map(|p| {
            p.sorted_blocks()
                .into_iter()
                .map(move |b| derive_chunk_payload(b, p.page_number))
        })
        .collect()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableTier {
    Whole,
    Segmented,
    RowLevel,
}
pub fn classify_table(rows: usize, whole_max: usize, segment_max: usize) -> TableTier {
    if rows <= whole_max {
        TableTier::Whole
    } else if rows <= segment_max {
        TableTier::Segmented
    } else {
        TableTier::RowLevel
    }
}
pub fn parse_markdown_rows(markdown: &str) -> (Vec<&str>, Vec<&str>) {
    let rows: Vec<_> = markdown
        .lines()
        .map(str::trim)
        .filter(|s| s.starts_with('|'))
        .collect();
    match rows.iter().position(|row| {
        let compact = row.replace(' ', "");
        compact.contains('-') && compact.chars().all(|c| "|-:".contains(c))
    }) {
        Some(index) => (rows[..=index].to_vec(), rows[index + 1..].to_vec()),
        None => (vec![], rows),
    }
}
/// Pure opt-in rewrite; the default parsing cascade does not activate it.
pub fn apply_table_policy(
    doc: &DocIR,
    rows_per_segment: usize,
    overlap_rows: usize,
    whole_max: usize,
    segment_max: usize,
) -> DocIR {
    let mut result = doc.clone();
    for page in &mut result.pages {
        page.blocks = page
            .blocks
            .iter()
            .flat_map(|b| {
                split_table_block(b, rows_per_segment, overlap_rows, whole_max, segment_max)
            })
            .collect();
    }
    result
}
pub fn derive_chunk_payload(b: &Block, page: usize) -> Value {
    let mut p = json!({"text":render_block_markdown(b),"block_type":b.kind,"page_number":page,"citation":citation_anchor(b,page)});
    if let Some(t) = &b.table {
        p["table_html"] = t["html"].clone();
        p["table_summary"] = t["summary"].clone()
    }
    if let Some(f) = &b.formula {
        p["formula_latex"] = f["latex"].clone()
    }
    if let Some(c) = b.confidence {
        p["confidence"] = json!(c)
    }
    p
}
pub fn split_table_block(
    block: &Block,
    rows_per_segment: usize,
    overlap_rows: usize,
    whole_max: usize,
    segment_max: usize,
) -> Vec<Block> {
    let Some(table) = block.table.as_ref().filter(|_| block.kind == "table") else {
        return vec![block.clone()];
    };
    let md = string(table, "markdown");
    if md.is_empty() {
        return vec![block.clone()];
    }
    let (header, data) = parse_markdown_rows(md);
    let tier = classify_table(data.len(), whole_max, segment_max);
    if tier == TableTier::Whole {
        return vec![block.clone()];
    }
    let size = if tier == TableTier::Segmented {
        rows_per_segment.max(1)
    } else {
        1
    };
    let step = if size == 1 {
        1
    } else {
        size.saturating_sub(overlap_rows).max(1)
    };
    (0..data.len())
        .step_by(step)
        .enumerate()
        .map(|(seq, start)| {
            let window = &data[start..(start + size).min(data.len())];
            let markdown = header
                .iter()
                .chain(window)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            let mut b = block.clone();
            b.block_id = format!("{}#t{seq}", block.block_id);
            b.text = markdown.clone();
            let mut t = table.clone();
            t["markdown"] = json!(markdown);
            t["html"] = Value::Null;
            t["n_rows"] = json!(window.len());
            b.table = Some(t);
            b
        })
        .collect()
}

#[derive(Clone)]
pub struct PostgresPageCache {
    pub pool: sqlx::PgPool,
    pub tenant_id: String,
    pub dataset_id: String,
    pub document_id: String,
    pub generation_key: String,
    pub parser_config_hash: String,
    pub page_content_hashes: BTreeMap<usize, String>,
}
impl PostgresPageCache {
    pub async fn get(&self, key: &str) -> Result<Option<PageIR>> {
        let query = format!(
            "SELECT to_jsonb(q) FROM ({}) q",
            super::artifact_sql::LOAD_PARSING_PAGE_CACHE_0
        );
        let row: Option<Value> = sqlx::query_scalar(&query)
            .bind(&self.tenant_id)
            .bind(&self.dataset_id)
            .bind(&self.document_id)
            .bind(&self.generation_key)
            .bind(key)
            .bind(&self.parser_config_hash)
            .fetch_optional(crate::database::executor(&self.pool))
            .await?;
        let Some(row) = row else { return Ok(None) };
        let p: PageIR = serde_json::from_value(row["page_ir"].clone())?;
        if p.page_number != integer(&row, "page_number") as usize {
            return Err(IngestError::Parser(
                "persisted parsing page cache identity mismatch".into(),
            ));
        }
        Ok(Some(p))
    }
    pub async fn put(&self, key: &str, page: &PageIR) -> Result<()> {
        let content = self
            .page_content_hashes
            .get(&page.page_number)
            .ok_or_else(|| {
                IngestError::Parser("page cache write has no source content hash".into())
            })?;
        let row = sqlx::query(super::artifact_sql::STORE_PARSING_PAGE_CACHE_0)
            .bind(&self.tenant_id)
            .bind(&self.dataset_id)
            .bind(&self.document_id)
            .bind(&self.generation_key)
            .bind(key)
            .bind(content)
            .bind(page.page_number as i32)
            .bind(&page.parser)
            .bind(&page.parser_version)
            .bind(&self.parser_config_hash)
            .bind(serde_json::to_value(page)?)
            .bind(page.confidence)
            .bind(page.hard_page)
            .fetch_optional(crate::database::executor(&self.pool))
            .await?;
        if row.is_none() {
            return Err(IngestError::GenerationChanged);
        }
        Ok(())
    }
    pub async fn load_ir(&self, bundle: &str) -> Result<Option<DocIR>> {
        let query = format!(
            "SELECT ir FROM ({}) q",
            super::artifact_sql::LOAD_PARSING_IR_0
        );
        let value: Option<Value> = sqlx::query_scalar(&query)
            .bind(&self.tenant_id)
            .bind(&self.dataset_id)
            .bind(&self.document_id)
            .bind(&self.generation_key)
            .bind(bundle)
            .bind(&self.parser_config_hash)
            .fetch_optional(crate::database::executor(&self.pool))
            .await?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
    pub async fn store_ir(&self, doc: &DocIR, config: &CascadeConfig, stats: &Value) -> Result<()> {
        let bundle = string(&doc.metadata, "parser_bundle");
        let stored = sqlx::query(super::artifact_sql::STORE_PARSING_IR_0)
            .bind(&self.tenant_id)
            .bind(&self.dataset_id)
            .bind(&self.document_id)
            .bind(&self.generation_key)
            .bind(&doc.content_hash)
            .bind(&doc.schema_version)
            .bind(bundle)
            .bind(&self.parser_config_hash)
            .bind(serde_json::to_value(config)?)
            .bind(serde_json::to_value(doc)?)
            .bind(stats)
            .fetch_optional(crate::database::executor(&self.pool))
            .await?;
        if stored.is_none() {
            return Err(IngestError::GenerationChanged);
        }
        Ok(())
    }
}
