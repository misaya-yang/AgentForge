//! Native callable equivalents of the historical LangGraph/LangChain KB adapters.
//! The descriptor carries schema data and the native object carries execution; no Python runtime.
use crate::migration_knowledge_query::{
    config::RetrieveRequest,
    service::RetrievalService,
    types::{QueryError, Result, RetrieveResult, UserContext},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KBSearchResult {
    pub content: String,
    pub score: f64,
    pub segment_id: String,
    pub document_id: String,
    pub metadata: Value,
}
impl KBSearchResult {
    pub fn to_dict(&self) -> Value {
        json!({"content":self.content,"score":self.score,"segment_id":self.segment_id,"document_id":self.document_id,"metadata":self.metadata})
    }
}
impl From<RetrieveResult> for KBSearchResult {
    fn from(r: RetrieveResult) -> Self {
        Self {
            content: r.text,
            score: r.score,
            segment_id: r.segment_id,
            document_id: r.document_id,
            metadata: r.metadata,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KBRetrievalOutput {
    pub segments: Vec<KBSearchResult>,
    pub metadata: Value,
    pub query: String,
    pub dataset_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KBRetrievalInput {
    pub query: String,
    pub dataset_id: String,
    pub top_k: usize,
    pub mode: String,
    pub intent: String,
    pub document_id: Option<String>,
    pub rerank: bool,
    pub mmr: bool,
}
impl Default for KBRetrievalInput {
    fn default() -> Self {
        Self {
            query: String::new(),
            dataset_id: String::new(),
            top_k: 5,
            mode: "hybrid".into(),
            intent: "general".into(),
            document_id: None,
            rerank: false,
            mmr: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KBToolConfig {
    pub top_k: usize,
    pub mode: String,
    pub rerank: bool,
    pub mmr: bool,
    pub include_scores: bool,
    pub max_content_length: i64,
}
impl Default for KBToolConfig {
    fn default() -> Self {
        Self {
            top_k: 5,
            mode: "hybrid".into(),
            rerank: false,
            mmr: false,
            include_scores: true,
            max_content_length: 2000,
        }
    }
}
fn invalid(message: &str) -> QueryError {
    QueryError::Validation(message.into())
}
fn authenticate(user: &UserContext) -> Result<()> {
    if user.is_authenticated {
        Ok(())
    } else {
        Err(QueryError::Forbidden)
    }
}
fn query(value: &Value) -> Result<String> {
    let value = value
        .as_str()
        .ok_or_else(|| invalid("knowledge tool query must contain 1-4096 characters"))?
        .trim();
    if !(1..=4096).contains(&value.chars().count()) {
        return Err(invalid(
            "knowledge tool query must contain 1-4096 characters",
        ));
    }
    Ok(value.into())
}
fn top_k(value: Option<&Value>, default: usize) -> Result<usize> {
    let n = match value.filter(|v| !v.is_null()) {
        Some(v) => v
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| invalid("knowledge tool top_k must be an integer between 1 and 20"))?,
        None => default,
    };
    if !(1..=20).contains(&n) {
        return Err(invalid(
            "knowledge tool top_k must be an integer between 1 and 20",
        ));
    }
    Ok(n)
}
fn intent(value: Option<&Value>) -> Result<()> {
    if value.is_some_and(|v| {
        !v.is_null()
            && !v.is_string()
            && *v != false
            && v.as_f64() != Some(0.0)
            && v.as_array().is_none_or(|a| !a.is_empty())
            && v.as_object().is_none_or(|m| !m.is_empty())
    }) {
        return Err(invalid(
            "knowledge tool intent must be general or find_document",
        ));
    }
    let text = value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("general")
        .trim()
        .to_lowercase();
    if ["general", "find_document"].contains(&text.as_str()) {
        Ok(())
    } else {
        Err(invalid(
            "knowledge tool intent must be general or find_document",
        ))
    }
}
fn threshold(value: Option<&Value>) -> Result<Option<f64>> {
    match value.filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
            .map(Some)
            .ok_or_else(|| {
                invalid("knowledge tool score_threshold must be finite and between 0 and 1")
            }),
    }
}
fn datasets(ids: Vec<String>) -> Result<Vec<String>> {
    if !(1..=8).contains(&ids.len()) {
        return Err(invalid(
            "knowledge tool dataset_ids must be a list containing 1-8 entries",
        ));
    }
    let ids = ids
        .into_iter()
        .map(|s| s.trim().to_owned())
        .collect::<Vec<_>>();
    if ids.iter().any(String::is_empty) || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(invalid(
            "knowledge tool dataset_ids must be non-empty and unique",
        ));
    }
    Ok(ids)
}
fn invocation(input: Value, kwargs: Value) -> Result<Value> {
    let mut input = if input.is_string() {
        json!({"query":input})
    } else {
        input
    };
    let map = input
        .as_object_mut()
        .ok_or_else(|| invalid("knowledge tool input must be text or an object"))?;
    if let Some(kwargs) = kwargs.as_object() {
        for (k, v) in kwargs {
            if map.contains_key(k) {
                return Err(invalid(&format!("multiple values for argument {k}")));
            }
            map.insert(k.clone(), v.clone());
        }
    }
    Ok(input)
}
fn request(
    config: &KBToolConfig,
    arguments: &Value,
    validate_intent: bool,
) -> Result<RetrieveRequest> {
    let q = query(&arguments["query"])?;
    let k = top_k(arguments.get("top_k"), config.top_k)?;
    if validate_intent {
        intent(arguments.get("intent"))?;
    }
    threshold(arguments.get("score_threshold"))?;
    let mut value =
        json!({"query":q,"top_k":k,"mode":config.mode,"rerank":config.rerank,"mmr":config.mmr});
    if let Some(args) = arguments.as_object() {
        for (key, v) in args {
            if !["query", "top_k", "dataset_id", "user"].contains(&key.as_str())
                && (key != "intent" || !validate_intent)
            {
                value[key] = v.clone();
            }
        }
    }
    // Text-only intent is validated but deliberately not forwarded by the source adapters.
    serde_json::from_value(value).map_err(|_| invalid("invalid knowledge retrieval arguments"))
}
fn schema(k: usize) -> Value {
    json!({"type":"object","properties":{"query":{"type":"string","description":"The search query describing what information you're looking for","minLength":1,"maxLength":4096},"intent":{"type":"string","enum":["general","find_document"],"description":"Text retrieval intent"},"top_k":{"type":"integer","description":"Number of results to return","default":k,"minimum":1,"maximum":20}},"required":["query"]})
}
fn openai(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":parameters}})
}
pub fn format_results_for_llm(
    results: &[KBSearchResult],
    include_scores: bool,
    max_content_length: i64,
) -> String {
    if results.is_empty() {
        return "No relevant information found in the knowledge base.".into();
    }
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut content = r.content.clone();
            let length = content.chars().count();
            if (length as i128) > i128::from(max_content_length) {
                let end = if max_content_length < 0 {
                    length.saturating_sub(max_content_length.unsigned_abs() as usize)
                } else {
                    max_content_length as usize
                };
                content = content.chars().take(end).collect::<String>() + "...";
            }
            if include_scores {
                format!("[{}] (score: {:.3})\n{content}", i + 1, r.score)
            } else {
                format!("[{}]\n{content}", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}
#[derive(Clone)]
pub struct KnowledgeRetriever {
    pub service: Arc<RetrievalService>,
    pub dataset_id: String,
    pub user: UserContext,
    pub config: KBToolConfig,
}
impl KnowledgeRetriever {
    pub fn new(
        service: Arc<RetrievalService>,
        dataset_id: String,
        user: UserContext,
        config: KBToolConfig,
    ) -> Result<Self> {
        authenticate(&user)?;
        top_k(None, config.top_k)?;
        Ok(Self {
            service,
            dataset_id,
            user,
            config,
        })
    }
    pub fn create_system_context() -> UserContext {
        UserContext {
            user_id: "system".into(),
            tenant_id: "system".into(),
            is_authenticated: true,
            user_tier: "admin".into(),
            user_type: "user".into(),
            roles: vec!["admin".into()],
        }
    }
    pub async fn retrieve(&self, arguments: Value) -> Result<Vec<KBSearchResult>> {
        let mut args = arguments.clone();
        for key in ["mode", "rerank", "mmr"] {
            if args[key].is_null() || key == "mode" && args[key] == "" {
                args.as_object_mut().map(|m| m.remove(key));
            }
        }
        let request = request(&self.config, &args, true)?;
        let dataset = arguments["dataset_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.dataset_id);
        let (rows, _) = self.service.retrieve(&self.user, dataset, request).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
    pub async fn retrieve_texts(&self, arguments: Value) -> Result<Vec<String>> {
        Ok(self
            .retrieve(arguments)
            .await?
            .into_iter()
            .map(|r| r.content)
            .collect())
    }
    pub async fn retrieve_with_metadata(&self, arguments: Value) -> Result<KBRetrievalOutput> {
        // These fields are already positional in the original kwargs call.
        for key in ["dataset_id", "mode", "rerank", "mmr", "user"] {
            if arguments.get(key).is_some() {
                return Err(invalid(&format!("multiple values for argument {key}")));
            }
        }
        let request = request(&self.config, &arguments, false)?;
        let query = request.query.clone();
        let (rows, metadata) = self
            .service
            .retrieve(&self.user, &self.dataset_id, request)
            .await?;
        Ok(KBRetrievalOutput {
            segments: rows.into_iter().map(Into::into).collect(),
            metadata,
            query,
            dataset_id: self.dataset_id.clone(),
        })
    }
    pub fn as_openai_function(&self) -> Value {
        openai(
            "search_knowledge_base",
            &format!(
                "Search the knowledge base for relevant text information. Dataset: {}",
                self.dataset_id
            ),
            schema(5),
        )
    }
    pub fn as_langchain_tool(&self) -> NativeRetrievalTool {
        let mut parameters = schema(5);
        parameters["properties"]["query"]["description"] =
            json!("The search query to find relevant information");
        parameters["properties"]["intent"]["default"] = json!("general");
        NativeRetrievalTool {
            retriever: self.clone(),
            descriptor: json!({"name":format!("kb_search_{}",self.dataset_id),"description":format!("Search the knowledge base '{}' for relevant information. Use this tool to find answers to questions or gather context.",self.dataset_id),"args_schema":parameters}),
            simple: false,
        }
    }
}
#[derive(Clone)]
pub struct NativeRetrievalTool {
    pub retriever: KnowledgeRetriever,
    pub descriptor: Value,
    simple: bool,
}
impl NativeRetrievalTool {
    pub async fn ainvoke(&self, input: Value) -> Result<String> {
        let mut args = invocation(input, Value::Null)?;
        if args.get("top_k").is_none() {
            args["top_k"] = json!(5);
        }
        let rows = self.retriever.retrieve(args).await?;
        if self.simple {
            return Ok(format_results_for_llm(&rows, true, i64::MAX));
        }
        if rows.is_empty() {
            return Ok("No relevant information found.".into());
        }
        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, r)| format!("[{}] {}", i + 1, r.content))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}
#[derive(Clone)]
pub struct MultiDatasetRetriever {
    pub retrievers: Vec<KnowledgeRetriever>,
    pub dataset_ids: Vec<String>,
    pub default_top_k: usize,
}
impl MultiDatasetRetriever {
    pub fn new(
        service: Arc<RetrievalService>,
        dataset_ids: Vec<String>,
        user: UserContext,
        default_top_k: usize,
        default_mode: String,
    ) -> Result<Self> {
        authenticate(&user)?;
        let dataset_ids = datasets(dataset_ids)?;
        top_k(None, default_top_k)?;
        let retrievers = dataset_ids
            .iter()
            .map(|id| {
                KnowledgeRetriever::new(
                    service.clone(),
                    id.clone(),
                    user.clone(),
                    KBToolConfig {
                        top_k: default_top_k,
                        mode: default_mode.clone(),
                        ..Default::default()
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            retrievers,
            dataset_ids,
            default_top_k,
        })
    }
    pub async fn retrieve(
        &self,
        mut arguments: Value,
        merge_results: bool,
    ) -> Result<Vec<KBSearchResult>> {
        query(&arguments["query"])?;
        let k = top_k(arguments.get("top_k"), self.default_top_k)?;
        intent(arguments.get("intent"))?;
        threshold(arguments.get("score_threshold"))?;
        if let Some(id) = arguments["dataset_id"].as_str().filter(|s| !s.is_empty()) {
            let retriever = self
                .retrievers
                .iter()
                .find(|r| r.dataset_id == id)
                .ok_or_else(|| invalid(&format!("Dataset {id} not configured")))?;
            return retriever.retrieve(arguments).await;
        }
        arguments["top_k"] = json!(k);
        arguments.as_object_mut().map(|m| m.remove("dataset_id"));
        let mut jobs = tokio::task::JoinSet::new();
        for (i, retriever) in self.retrievers.iter().cloned().enumerate() {
            let args = arguments.clone();
            jobs.spawn(async move { (i, retriever.retrieve(args).await) });
        }
        let mut ordered = Vec::new();
        while let Some(result) = jobs.join_next().await {
            if let Ok((i, Ok(rows))) = result {
                ordered.push((i, rows));
            }
        }
        ordered.sort_by_key(|(i, _)| *i);
        let mut rows = ordered
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .collect::<Vec<_>>();
        if merge_results {
            rows.sort_by(|a, b| b.score.total_cmp(&a.score));
            rows.truncate(k);
        }
        Ok(rows)
    }
    pub fn as_openai_function(&self) -> Value {
        let list = self.dataset_ids.join(", ");
        let mut parameters = schema(5);
        parameters["properties"]["dataset_id"] = json!({"type":"string","description":format!("Optional: specific dataset to search. Options: {list}. If not provided, searches all datasets."),"enum":self.dataset_ids});
        openai(
            "search_knowledge_bases",
            &format!(
                "Search knowledge bases for relevant text information. Available datasets: {list}"
            ),
            parameters,
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DifyExternalDatasetConfig {
    pub dataset_id: String,
    pub retrieval_model: Value,
}
impl DifyExternalDatasetConfig {
    pub fn to_dict(&self) -> Value {
        json!({"dataset_id":self.dataset_id,"retrieval_model":self.retrieval_model})
    }
}
#[derive(Clone)]
pub struct DifyCompatibleKBAPI {
    service: Arc<RetrievalService>,
    user: UserContext,
}
impl DifyCompatibleKBAPI {
    pub fn new(service: Arc<RetrievalService>, user: UserContext) -> Result<Self> {
        authenticate(&user)?;
        Ok(Self { service, user })
    }
    pub async fn retrieve(
        &self,
        dataset_id: &str,
        query_value: Value,
        model: Value,
    ) -> Result<Value> {
        let query = query(&query_value)?;
        if model.get("top_k").is_some_and(Value::is_null) {
            return Err(invalid(
                "knowledge tool top_k must be an integer between 1 and 20",
            ));
        }
        let k = top_k(model.get("top_k"), 5)?;
        let threshold = threshold(model.get("score_threshold"))?;
        let mode = match model["search_method"]
            .as_str()
            .unwrap_or("hybrid_search")
            .to_lowercase()
            .as_str()
        {
            "semantic_search" => "vector",
            "full_text_search" => "keyword",
            _ => "hybrid",
        };
        let request = RetrieveRequest {
            query: query.clone(),
            top_k: k,
            mode: mode.into(),
            rerank: Some(model["reranking_enable"].as_bool().unwrap_or(false)),
            ..Default::default()
        };
        let (rows, _) = self
            .service
            .retrieve(&self.user, dataset_id, request)
            .await?;
        let records=rows.into_iter().filter(|r|threshold.is_none_or(|t|r.score>=t)).map(|r|json!({"segment":{"id":r.segment_id,"document_id":r.document_id,"content":r.text,"position":r.metadata.get("position").cloned().unwrap_or(json!(0)),"word_count":r.text.split_whitespace().count(),"tokens":r.metadata.get("token_count").cloned().unwrap_or(json!(0)),"keywords":r.metadata.get("keywords").cloned().unwrap_or(json!([])),"hit_count":r.metadata.get("hit_count").cloned().unwrap_or(json!(0))},"score":r.score,"tsne_position":null})).collect::<Vec<_>>();
        Ok(
            json!({"metadata":{"total":records.len(),"query":query,"dataset_id":dataset_id},"records":records}),
        )
    }
}
#[derive(Clone)]
pub struct KnowledgeBaseTool {
    pub retriever: KnowledgeRetriever,
    pub name: String,
    pub description: String,
    pub args_schema: Value,
}
impl KnowledgeBaseTool {
    pub fn new(
        service: Arc<RetrievalService>,
        dataset_id: String,
        user: UserContext,
        name: Option<String>,
        description: Option<String>,
        config: KBToolConfig,
    ) -> Result<Self> {
        let retriever = KnowledgeRetriever::new(service, dataset_id.clone(), user, config)?;
        let name = name
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("search_{}", dataset_id.replace('-', "_")));
        let description=description.filter(|s|!s.is_empty()).unwrap_or_else(||format!("Search the '{dataset_id}' knowledge base for relevant information. Use this text retrieval tool when you need to find specific facts, documentation, or context."));
        let mut args_schema = schema(retriever.config.top_k);
        args_schema["properties"]["top_k"]["description"] =
            json!("Number of results to return (default: 5)");
        Ok(Self {
            retriever,
            name,
            description,
            args_schema,
        })
    }
    pub async fn arun(&self, arguments: Value) -> Result<String> {
        // Input validation is outside the source exception-to-string boundary.
        query(&arguments["query"])?;
        top_k(arguments.get("top_k"), self.retriever.config.top_k)?;
        intent(arguments.get("intent"))?;
        threshold(arguments.get("score_threshold"))?;
        let response = async {
            let req = request(&self.retriever.config, &arguments, true)?;
            let dataset = arguments["dataset_id"]
                .as_str()
                .unwrap_or(&self.retriever.dataset_id);
            let (rows, _) = self
                .retriever
                .service
                .retrieve(&self.retriever.user, dataset, req)
                .await?;
            Ok::<_, QueryError>(
                rows.into_iter()
                    .map(KBSearchResult::from)
                    .collect::<Vec<_>>(),
            )
        }
        .await;
        Ok(match response {
            Ok(rows) => format_results_for_llm(
                &rows,
                self.retriever.config.include_scores,
                self.retriever.config.max_content_length,
            ),
            Err(error) => format!("Error searching knowledge base: {error}"),
        })
    }
    pub async fn ainvoke(&self, input: Value, kwargs: Value) -> Result<String> {
        self.arun(invocation(input, kwargs)?).await
    }
    pub fn invoke(&self, input: Value, kwargs: Value) -> Result<String> {
        sync_bridge(self.ainvoke(input, kwargs))
    }
    pub fn call(&self, query: String, kwargs: Value) -> Result<String> {
        self.invoke(json!(query), kwargs)
    }
    pub fn to_openai_function(&self) -> Value {
        openai(&self.name, &self.description, self.args_schema.clone())
    }
    pub fn to_langchain_tool(&self) -> NativeToolDescriptor {
        NativeToolDescriptor {
            definition: json!({"name":self.name,"description":self.description,"args_schema":self.args_schema}),
            execution: NativeToolExecution::Single(self.clone()),
        }
    }
}
#[derive(Clone)]
pub struct MultiKnowledgeBaseTool {
    pub service: Arc<RetrievalService>,
    pub dataset_ids: Vec<String>,
    pub user: UserContext,
    pub config: KBToolConfig,
    pub name: String,
    pub description: String,
    pub args_schema: Value,
}
impl MultiKnowledgeBaseTool {
    pub fn new(
        service: Arc<RetrievalService>,
        dataset_ids: Vec<String>,
        user: UserContext,
        name: String,
        description: Option<String>,
        config: KBToolConfig,
    ) -> Result<Self> {
        authenticate(&user)?;
        let dataset_ids = datasets(dataset_ids)?;
        top_k(None, config.top_k)?;
        let list = dataset_ids
            .iter()
            .map(|d| format!("'{d}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let description=description.filter(|s|!s.is_empty()).unwrap_or_else(||format!("Search across multiple knowledge bases for relevant information. Available text datasets: {list}. Optionally specify a dataset_id to search only that dataset."));
        let mut args_schema = schema(config.top_k);
        args_schema["properties"]["top_k"]["description"] = json!("Number of results (default: 5)");
        args_schema["properties"]["dataset_id"] = json!({"type":"string","description":format!("Optional: specific dataset to search ({})",dataset_ids.join(", ")),"enum":dataset_ids});
        Ok(Self {
            service,
            dataset_ids,
            user,
            config,
            name,
            description,
            args_schema,
        })
    }
    pub async fn arun(&self, arguments: Value) -> Result<String> {
        query(&arguments["query"])?;
        let k = top_k(arguments.get("top_k"), self.config.top_k)?;
        intent(arguments.get("intent"))?;
        threshold(arguments.get("score_threshold"))?;
        let selected = match arguments.get("dataset_id").filter(|v| !v.is_null()) {
            None => self.dataset_ids.clone(),
            Some(v) => {
                let id = v
                    .as_str()
                    .ok_or_else(|| invalid("knowledge tool dataset_id is not configured"))?;
                if !self.dataset_ids.iter().any(|s| s == id) {
                    return Err(invalid("knowledge tool dataset_id is not configured"));
                }
                vec![id.into()]
            }
        };
        let mut jobs = tokio::task::JoinSet::new();
        for (i, id) in selected.into_iter().enumerate() {
            let service = self.service.clone();
            let user = self.user.clone();
            let config = self.config.clone();
            let args = arguments.clone();
            jobs.spawn(async move {
                let rows = async {
                    let request = request(&config, &args, true)?;
                    let (rows, _) = service.retrieve(&user, &id, request).await?;
                    Ok::<_, QueryError>(
                        rows.into_iter()
                            .map(|r| {
                                let mut r = KBSearchResult::from(r);
                                if !r.metadata.is_object() {
                                    r.metadata = json!({});
                                }
                                r.metadata["dataset_id"] = json!(id);
                                r
                            })
                            .collect::<Vec<_>>(),
                    )
                }
                .await
                .unwrap_or_default();
                (i, rows)
            });
        }
        let mut ordered = Vec::new();
        while let Some(result) = jobs.join_next().await {
            if let Ok(result) = result {
                ordered.push(result);
            }
        }
        ordered.sort_by_key(|(i, _)| *i);
        let mut rows = ordered
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| b.score.total_cmp(&a.score));
        rows.truncate(k);
        Ok(format_results_for_llm(
            &rows,
            self.config.include_scores,
            self.config.max_content_length,
        ))
    }
    pub async fn ainvoke(&self, input: Value, kwargs: Value) -> Result<String> {
        self.arun(invocation(input, kwargs)?).await
    }
    pub fn invoke(&self, input: Value, kwargs: Value) -> Result<String> {
        sync_bridge(self.ainvoke(input, kwargs))
    }
    pub fn call(&self, query: String, kwargs: Value) -> Result<String> {
        self.invoke(json!(query), kwargs)
    }
    pub fn to_openai_function(&self) -> Value {
        openai(&self.name, &self.description, self.args_schema.clone())
    }
    pub fn to_langchain_tool(&self) -> NativeToolDescriptor {
        NativeToolDescriptor {
            definition: json!({"name":self.name,"description":self.description,"args_schema":self.args_schema}),
            execution: NativeToolExecution::Multi(self.clone()),
        }
    }
}
/// Callable slots stay native rather than pretending JSON can serialize Python functions.
#[derive(Clone)]
pub struct NativeToolDescriptor {
    pub definition: Value,
    pub execution: NativeToolExecution,
}
#[derive(Clone)]
pub enum NativeToolExecution {
    Single(KnowledgeBaseTool),
    Multi(MultiKnowledgeBaseTool),
}
impl NativeToolDescriptor {
    pub async fn ainvoke(&self, input: Value, kwargs: Value) -> Result<String> {
        match &self.execution {
            NativeToolExecution::Single(tool) => tool.ainvoke(input, kwargs).await,
            NativeToolExecution::Multi(tool) => tool.ainvoke(input, kwargs).await,
        }
    }
    pub fn invoke(&self, input: Value, kwargs: Value) -> Result<String> {
        sync_bridge(self.ainvoke(input, kwargs))
    }
}
fn sync_bridge<F>(future: F) -> Result<String>
where
    F: std::future::Future<Output = Result<String>> + Send,
{
    fn execute<F: std::future::Future<Output = Result<String>>>(future: F) -> Result<String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| invalid("knowledge tool runtime unavailable"))?
            .block_on(future)
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(move || execute(future))
                .join()
                .unwrap_or_else(|_| Err(invalid("knowledge tool execution failed")))
        })
    } else {
        execute(future)
    }
}
pub fn create_kb_tool(
    service: Arc<RetrievalService>,
    dataset_id: String,
    user: UserContext,
    name: Option<String>,
    description: Option<String>,
    config: KBToolConfig,
) -> Result<KnowledgeBaseTool> {
    KnowledgeBaseTool::new(service, dataset_id, user, name, description, config)
}
pub fn create_multi_kb_tool(
    service: Arc<RetrievalService>,
    dataset_ids: Vec<String>,
    user: UserContext,
    name: String,
    description: Option<String>,
    config: KBToolConfig,
) -> Result<MultiKnowledgeBaseTool> {
    MultiKnowledgeBaseTool::new(service, dataset_ids, user, name, description, config)
}
pub fn create_langchain_kb_tool(
    service: Arc<RetrievalService>,
    dataset_id: String,
    user: UserContext,
    name: Option<String>,
    description: Option<String>,
    config: KBToolConfig,
) -> Result<NativeToolDescriptor> {
    Ok(create_kb_tool(service, dataset_id, user, name, description, config)?.to_langchain_tool())
}
pub fn create_retrieval_tool(
    service: Arc<RetrievalService>,
    dataset_id: String,
    user: UserContext,
    config: KBToolConfig,
) -> Result<NativeRetrievalTool> {
    let retriever = KnowledgeRetriever::new(service, dataset_id, user, config)?;
    Ok(NativeRetrievalTool {
        retriever,
        descriptor: json!({"name":"search"}),
        simple: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_source_bounds_before_retrieval() {
        assert!(query(&json!("  ")).is_err());
        assert_eq!(query(&json!("  问题  ")).unwrap(), "问题");
        assert!(top_k(Some(&json!(true)), 5).is_err());
        assert!(top_k(Some(&json!(21)), 5).is_err());
        assert!(threshold(Some(&json!(false))).is_err());
        assert!(intent(Some(&json!("find_image"))).is_err());
        assert!(datasets(vec!["a".into(), " a ".into()]).is_err());
    }
    #[test]
    fn unicode_truncation_and_numbered_format_match_source() {
        let row = KBSearchResult {
            content: "中文abc".into(),
            score: 0.625,
            segment_id: "s".into(),
            document_id: "d".into(),
            metadata: json!({}),
        };
        assert_eq!(
            format_results_for_llm(&[row.clone()], true, 2),
            "[1] (score: 0.625)\n中文..."
        );
        assert_eq!(format_results_for_llm(&[row], false, -2), "[1]\n中文a...");
        assert_eq!(
            format_results_for_llm(&[], false, 10),
            "No relevant information found in the knowledge base."
        );
    }
    #[test]
    fn duplicate_invocation_arguments_fail() {
        assert!(invocation(json!({"query":"x"}), json!({"query":"y"})).is_err());
        assert_eq!(
            invocation(json!("x"), json!({"top_k":2})).unwrap(),
            json!({"query":"x","top_k":2})
        );
    }
    #[test]
    fn text_intent_not_forwarded_by_plain_adapter() {
        let request = request(
            &KBToolConfig::default(),
            &json!({"query":" x ","intent":"find_document","score_threshold":0.25}),
            true,
        )
        .unwrap();
        assert_eq!(request.query, "x");
        assert_eq!(request.score_threshold, Some(0.25));
        assert!(!request.extra.contains_key("intent"));
    }
}
