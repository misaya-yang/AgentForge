use super::qa_stream::QaProviderStream;
use super::{
    config::RetrieveRequest,
    service::RetrievalService,
    types::{Dataset, IndexGeneration, QueryError, Result, RetrieveResult, UserContext},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful assistant that answers questions based on the provided context.\n\nRules:\n1. Only answer based on the provided context\n2. If the context doesn't contain relevant information, say \"I don't have enough information to answer this question based on the provided context.\"\n3. Be concise and accurate\n4. Cite the relevant parts of the context when appropriate\n\nContext will be provided in the user message.";
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmSelector {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<usize>,
    pub system_prompt: Option<String>,
}
#[derive(Clone)]
pub struct QaConfig {
    pub provider: String,
    pub model: String,
    pub allowed_providers: BTreeSet<String>,
    pub allowed_models: BTreeSet<String>,
    pub api_key: String,
    pub base_url: String,
    pub timeout: Duration,
}
#[derive(Clone)]
pub struct QaService {
    pub retrieval: Arc<RetrievalService>,
    pub batch_timeout: Duration,
    config: Arc<QaConfig>,
}
impl QaService {
    pub fn new(retrieval: Arc<RetrievalService>, _client: Client, mut config: QaConfig) -> Self {
        config.api_key.clear();
        config.base_url.clear();
        Self {
            retrieval,
            batch_timeout: Duration::from_secs(180),
            config: Arc::new(config),
        }
    }
    pub fn validate_selector(&self, selector: &LlmSelector) -> Result<()> {
        let provider = selector
            .provider
            .as_deref()
            .unwrap_or(&self.config.provider)
            .trim()
            .to_lowercase();
        let model = selector
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .trim();
        if !self.config.allowed_providers.contains(&provider)
            || !self.config.allowed_models.contains(model)
        {
            return Err(QueryError::Validation(
                "QA provider or model is not allowlisted".into(),
            ));
        }
        if provider != "dashscope" {
            return Err(QueryError::Validation(
                "Only the server-owned DashScope QA provider is enabled".into(),
            ));
        }
        if selector
            .temperature
            .is_some_and(|v| !v.is_finite() || !(0.0..=2.0).contains(&v))
            || selector
                .max_tokens
                .is_some_and(|v| !(1..=4096).contains(&v))
            || selector
                .system_prompt
                .as_ref()
                .is_some_and(|s| s.chars().count() > 4000)
        {
            return Err(QueryError::Validation(
                "QA generation preferences are outside permitted bounds".into(),
            ));
        }
        Ok(())
    }
    fn payload(
        &self,
        query: &str,
        results: &[RetrieveResult],
        selector: &LlmSelector,
        stream: bool,
    ) -> Value {
        let context = format_context(results, 4000);
        let system = selector
            .system_prompt
            .as_deref()
            .unwrap_or(DEFAULT_SYSTEM_PROMPT);
        let mut payload = json!({"model":selector.model.as_deref().unwrap_or(&self.config.model),"messages":[{"role":"system","content":system},{"role":"user","content":format!("Context:\n{context}\n\nQuestion: {query}\n\nPlease answer the question based on the context provided above.")}],"temperature":selector.temperature.unwrap_or(0.1),"max_tokens":selector.max_tokens.unwrap_or(2048)});
        if stream {
            payload["stream"] = json!(true)
        }
        payload
    }
    async fn require_editor(&self, user: &UserContext, id: &str) -> Result<Dataset> {
        if user.anonymous()
            || !(user.user_tier.trim().eq_ignore_ascii_case("admin")
                || user
                    .roles
                    .iter()
                    .any(|r| r.trim().eq_ignore_ascii_case("admin")))
        {
            return Err(QueryError::Forbidden);
        }
        self.retrieval
            .store
            .require_dataset_access(user, id, "editor")
            .await
    }
    pub async fn query(
        &self,
        user: &UserContext,
        id: &str,
        request: RetrieveRequest,
        selector: LlmSelector,
        include_raw: bool,
    ) -> Result<Value> {
        super::model_plane::run_current(
            super::model_plane::ModelResource {
                dataset_id: Some(id.into()),
                document_id: None,
                execution_id: None,
            },
            user.clone(),
            self.query_scoped(user, id, request, selector, include_raw),
        )
        .await
    }
    async fn query_scoped(
        &self,
        user: &UserContext,
        id: &str,
        request: RetrieveRequest,
        selector: LlmSelector,
        include_raw: bool,
    ) -> Result<Value> {
        self.validate_selector(&selector)?;
        let started = Instant::now();
        let dataset = self.require_editor(user, id).await?;
        let generation = dataset.generation()?;
        let retrieve_started = Instant::now();
        let (results, meta) = self.retrieval.retrieve(user, id, request.clone()).await?;
        let retrieval_ms = retrieve_started.elapsed().as_millis();
        let llm = Instant::now();
        let (status, data) = super::model_plane::execute(
            selector
                .provider
                .as_deref()
                .unwrap_or(&self.config.provider),
            selector.model.as_deref().unwrap_or(&self.config.model),
            "",
            super::model_plane::ModelPurpose::Qa,
            super::model_plane::ModelWire::ChatCompletions,
            self.payload(&request.query, &results, &selector, false),
            self.config.timeout,
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(QueryError::Provider(status));
        }
        let choices = data["choices"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or(QueryError::InvalidProviderResponse)?;
        let answer = choices[0]["message"]["content"].as_str().unwrap_or("");
        let llm_ms = llm.elapsed().as_millis();
        let context = self
            .context_segments(user, &dataset, &generation, &results, include_raw)
            .await?;
        Ok(
            json!({"query":request.query,"answer":answer,"context_segments":context,"retrieval_metadata":meta,"timing":{"retrieval_ms":retrieval_ms,"llm_ms":llm_ms,"total_ms":started.elapsed().as_millis()},"model":selector.model.as_deref().unwrap_or(&self.config.model),"tokens_used":data["usage"]["total_tokens"],"trace_id":meta["trace_id"],"query_fingerprint":meta["query_fingerprint"]}),
        )
    }
    pub async fn context_segments(
        &self,
        user: &UserContext,
        dataset: &Dataset,
        generation: &IndexGeneration,
        results: &[RetrieveResult],
        include_raw: bool,
    ) -> Result<Vec<Value>> {
        self.retrieval
            .store
            .require_generation(user, &dataset.dataset_id, generation)
            .await?;
        let ids: Vec<_> = results
            .iter()
            .map(|r| r.document_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut identity_scope = dataset.clone();
        identity_scope.tenant_id = user.tenant_id.clone();
        let identities = self
            .retrieval
            .store
            .published_source_identities(&identity_scope, &ids)
            .await?;
        self.retrieval
            .store
            .require_generation(user, &dataset.dataset_id, generation)
            .await?;
        let mut out = Vec::new();
        for result in results {
            let mut metadata = if include_raw {
                result.metadata.clone()
            } else {
                json!({})
            };
            if let Some(m) = metadata.as_object_mut() {
                m.remove("source_version");
                m.remove("source_hash");
            }
            let mut item = json!({"segment_id":result.segment_id,"document_id":result.document_id,"text":result.text,"score":result.score,"metadata":metadata});
            if !manual_source(&result.metadata) {
                if let Some((version, hash)) = identities.get(&result.document_id) {
                    if valid_identity(*version, hash) {
                        item["source_version"] = json!(version);
                        item["source_hash"] = json!(hash);
                        if include_raw {
                            item["metadata"]["source_version"] = json!(version);
                            item["metadata"]["source_hash"] = json!(hash)
                        }
                    }
                }
            }
            out.push(item)
        }
        Ok(out)
    }
    pub async fn query_stream(
        &self,
        user: UserContext,
        id: String,
        request: RetrieveRequest,
        selector: LlmSelector,
        include_raw: bool,
    ) -> Result<tokio::sync::mpsc::Receiver<Value>> {
        self.validate_selector(&selector)?;
        self.require_editor(&user, &id).await?.generation()?;
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let service = self.clone();
        tokio::spawn(super::model_plane::inherit(async move {
            if let Err(error) = service
                .stream_to(&user, &id, &request, &selector, include_raw, &tx)
                .await
            {
                let mut data = json!({"message":error.to_string()});
                if matches!(
                    error,
                    QueryError::GenerationChanged | QueryError::PublicationInProgress
                ) {
                    data["status"] = json!(409);
                    data["retry_after"] = json!(1)
                }
                let _ = tx.send(json!({"event":"error","data":data})).await;
            }
        }));
        Ok(rx)
    }
    async fn stream_to(
        &self,
        user: &UserContext,
        id: &str,
        request: &RetrieveRequest,
        selector: &LlmSelector,
        include_raw: bool,
        tx: &tokio::sync::mpsc::Sender<Value>,
    ) -> Result<()> {
        super::model_plane::run_current(
            super::model_plane::ModelResource {
                dataset_id: Some(id.into()),
                document_id: None,
                execution_id: None,
            },
            user.clone(),
            self.stream_to_scoped(user, id, request, selector, include_raw, tx),
        )
        .await
    }
    async fn stream_to_scoped(
        &self,
        user: &UserContext,
        id: &str,
        request: &RetrieveRequest,
        selector: &LlmSelector,
        include_raw: bool,
        tx: &tokio::sync::mpsc::Sender<Value>,
    ) -> Result<()> {
        let started = Instant::now();
        let dataset = self.require_editor(user, id).await?;
        let generation = dataset.generation()?;
        let (results, meta) = self.retrieval.retrieve(user, id, request.clone()).await?;
        let retrieval_ms = started.elapsed().as_millis();
        let context = self
            .context_segments(user, &dataset, &generation, &results, include_raw)
            .await?;
        if tx.send(json!({"event":"retrieval","data":{"query":request.query,"context_segments":context,"retrieval_metadata":meta,"trace_id":meta["trace_id"],"query_fingerprint":meta["query_fingerprint"],"timing":{"retrieval_ms":retrieval_ms}}})).await.is_err(){return Ok(())}
        let llm = Instant::now();
        let mut lines = super::model_plane::execute_stream(
            selector
                .provider
                .as_deref()
                .unwrap_or(&self.config.provider),
            selector.model.as_deref().unwrap_or(&self.config.model),
            "",
            super::model_plane::ModelPurpose::Qa,
            self.payload(&request.query, &results, selector, true),
            self.config.timeout,
        )
        .await?;
        let mut parser = QaProviderStream::default();
        let mut answer = String::new();
        let mut tokens = Value::Null;
        loop {
            if tx.is_closed() {
                return Ok(());
            }
            let (event, eof) = match lines.next().await? {
                Some(value) => {
                    parser
                        .line(&format!("data: {value}"))
                        .map_err(|_| QueryError::Provider(502))?;
                    (
                        parser.line("").map_err(|_| QueryError::Provider(502))?,
                        false,
                    )
                }
                None => (
                    parser.finish().map_err(|_| QueryError::Provider(502))?,
                    true,
                ),
            };
            let closed = event.as_ref().is_some_and(|event| event.closed);
            if let Some(event) = event {
                if let Some(total) = event.tokens {
                    tokens = json!(total);
                }
                if let Some(delta) = event.content.filter(|s| !s.is_empty()) {
                    answer.push_str(&delta);
                    if tx
                        .send(json!({"event":"delta","data":{"content":delta}}))
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
            }
            if eof || closed {
                break;
            }
        }
        lines.close();
        let result = json!({"query":request.query,"answer":answer,"context_segments":context,"retrieval_metadata":meta,"timing":{"retrieval_ms":retrieval_ms,"llm_ms":llm.elapsed().as_millis(),"total_ms":started.elapsed().as_millis()},"model":selector.model.as_deref().unwrap_or(&self.config.model),"tokens_used":tokens,"trace_id":meta["trace_id"],"query_fingerprint":meta["query_fingerprint"]});
        let _ = tx
            .send(json!({"event":"done","data":{"result":result}}))
            .await;
        Ok(())
    }
}
pub fn manual_source(metadata: &Value) -> bool {
    metadata["source_type"] == "manual"
        || metadata["manual_source_override"] == true
        || metadata["metadata"]["manual_source_override"] == true
}
pub fn valid_identity(version: i64, hash: &str) -> bool {
    version > 0
        && hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn format_context(results: &[RetrieveResult], max_tokens: usize) -> String {
    if results.is_empty() {
        return "No relevant context found.".into();
    }
    let mut parts = Vec::new();
    let mut used = 0;
    for (i, result) in results.iter().enumerate() {
        let citation = &result.metadata["citation"];
        let label = if citation.is_object() {
            format!(
                " (document {}, version {}, page {}, block {})",
                result.document_id,
                citation["source"]["document_version"]
                    .as_str()
                    .unwrap_or(""),
                citation["page_number"],
                citation["block_id"].as_str().unwrap_or("")
            )
        } else {
            String::new()
        };
        let text = format!("[{}]{} {}", i + 1, label, result.text.trim());
        if used + text.chars().count() > max_tokens * 4 {
            break;
        }
        used += text.chars().count();
        parts.push(text)
    }
    parts.join("\n\n")
}
/// Application-owned QA evaluation endpoint data, not migration fixtures.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QaCase {
    pub query: String,
    pub expected_answer: Option<String>,
    pub expected_segments: Option<Vec<String>>,
}
impl QaService {
    pub async fn evaluate_answer(
        &self,
        query: &str,
        answer: &str,
        expected: &str,
        selector: &LlmSelector,
    ) -> Result<(bool, f64, String)> {
        self.validate_selector(selector)?;
        let prompt = format!(
            "You are an answer evaluator. Compare the given answer with the expected answer and determine if they are semantically equivalent.\n\nQuestion: {query}\n\nExpected Answer: {expected}\n\nGiven Answer: {answer}\n\nRespond in JSON format:\n{{\n    \"is_correct\": true/false,\n    \"confidence\": 0.0-1.0,\n    \"explanation\": \"brief explanation\"\n}}"
        );
        let payload = json!({"model":selector.model.as_deref().unwrap_or(&self.config.model),"messages":[{"role":"system","content":"You are a precise answer evaluator. Output only valid JSON."},{"role":"user","content":prompt}],"temperature":0,"max_tokens":selector.max_tokens.unwrap_or(2048)});
        let (status, body) = super::model_plane::execute(
            selector
                .provider
                .as_deref()
                .unwrap_or(&self.config.provider),
            selector.model.as_deref().unwrap_or(&self.config.model),
            "",
            super::model_plane::ModelPurpose::Judge,
            super::model_plane::ModelWire::ChatCompletions,
            payload,
            self.config.timeout,
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(QueryError::Provider(status));
        }
        let text = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or(QueryError::InvalidProviderResponse)?;
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            if !v.is_object() {
                return Err(QueryError::InvalidProviderResponse);
            }
            let correct = match &v["is_correct"] {
                Value::Null => false,
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
                Value::String(s) => !s.is_empty(),
                Value::Array(v) => !v.is_empty(),
                Value::Object(v) => !v.is_empty(),
            };
            let confidence = if v.get("confidence").is_none() {
                0.5
            } else {
                v["confidence"]
                    .as_f64()
                    .or_else(|| v["confidence"].as_str().and_then(|s| s.parse().ok()))
                    .ok_or(QueryError::InvalidProviderResponse)?
            };
            Ok((
                correct,
                confidence,
                v["explanation"].as_str().unwrap_or("").into(),
            ))
        } else {
            let lower = text.to_lowercase();
            Ok((
                lower.contains("true") && lower.contains("is_correct"),
                0.5,
                text.into(),
            ))
        }
    }
    pub async fn run_test_case(
        &self,
        user: &UserContext,
        id: &str,
        case: QaCase,
        mut request: RetrieveRequest,
        selector: LlmSelector,
    ) -> Result<Value> {
        request.query = case.query.clone();
        let result = self
            .query(user, id, request, selector.clone(), true)
            .await?;
        let correct =
            if let Some(expected) = case.expected_answer.as_deref().filter(|s| !s.is_empty()) {
                Some(
                    self.evaluate_answer(
                        &case.query,
                        result["answer"].as_str().unwrap_or(""),
                        expected,
                        &selector,
                    )
                    .await?
                    .0,
                )
            } else {
                None
            };
        let (mut recall, mut precision) = (None, None);
        if let Some(ids) = case.expected_segments.as_ref().filter(|v| !v.is_empty()) {
            let expected: BTreeSet<_> = ids.iter().map(String::as_str).collect();
            let retrieved: BTreeSet<_> = result["context_segments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["segment_id"].as_str())
                .collect();
            let hit = retrieved.intersection(&expected).count() as f64;
            recall = Some(hit / expected.len() as f64);
            if !retrieved.is_empty() {
                precision = Some(hit / retrieved.len() as f64)
            }
        }
        Ok(
            json!({"test_case":case,"result":result,"evaluation":{"answer_correct":correct,"retrieval_recall":recall,"retrieval_precision":precision}}),
        )
    }
    pub async fn run_test_batch(
        &self,
        user: &UserContext,
        id: &str,
        cases: Vec<QaCase>,
        request: RetrieveRequest,
        selector: LlmSelector,
        concurrency: usize,
    ) -> Result<Vec<Value>> {
        if cases.is_empty()
            || cases.len() > 10
            || cases.iter().any(|c| {
                c.query.is_empty()
                    || c.query.chars().count() > 4096
                    || c.expected_answer
                        .as_ref()
                        .is_some_and(|s| s.chars().count() > 20000)
                    || c.expected_segments.as_ref().is_some_and(|v| v.len() > 200)
            })
        {
            return Err(QueryError::Validation("invalid QA batch cases".into()));
        }
        self.require_editor(user, id).await?;
        self.validate_selector(&selector)?;
        let count = cases.len();
        let limit = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let mut tasks = tokio::task::JoinSet::new();
        for (index, case) in cases.into_iter().enumerate() {
            let service = self.clone();
            let user = user.clone();
            let id = id.to_owned();
            let request = request.clone();
            let selector = selector.clone();
            let semaphore = limit.clone();
            tasks.spawn(super::model_plane::inherit(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| QueryError::DependenciesFailed)?;
                service
                    .run_test_case(&user, &id, case, request, selector)
                    .await
                    .map(|v| (index, v))
            }));
        }
        let mut results = vec![Value::Null; count];
        let mut failures = 0;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((index, v))) => results[index] = v,
                _ => failures += 1,
            }
        }
        if failures > 0 || results.iter().any(Value::is_null) {
            return Err(QueryError::Validation(format!(
                "QA batch incomplete: {failures} of {count} cases failed"
            )));
        }
        Ok(results)
    }
}
pub fn aggregate_test_results(results: &[Value]) -> Value {
    if results.is_empty() {
        return json!({"total":0});
    }
    let average = |key: &str| {
        let values: Vec<_> = results
            .iter()
            .filter_map(|r| r["evaluation"][key].as_f64())
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f64>() / values.len() as f64)
        }
    };
    let answers: Vec<_> = results
        .iter()
        .filter_map(|r| r["evaluation"]["answer_correct"].as_bool())
        .collect();
    let accuracy = if answers.is_empty() {
        None
    } else {
        Some(answers.iter().filter(|v| **v).count() as f64 / answers.len() as f64)
    };
    let time = |key: &str| {
        super::parents::round(
            results
                .iter()
                .map(|r| r["result"]["timing"][key].as_f64().unwrap_or(0.0))
                .sum::<f64>()
                / results.len() as f64,
            2,
        )
    };
    json!({"total":results.len(),"answer_accuracy":accuracy,"avg_retrieval_recall":average("retrieval_recall"),"avg_retrieval_precision":average("retrieval_precision"),"timing":{"avg_retrieval_ms":time("retrieval_ms"),"avg_llm_ms":time("llm_ms"),"avg_total_ms":time("total_ms")}})
}
