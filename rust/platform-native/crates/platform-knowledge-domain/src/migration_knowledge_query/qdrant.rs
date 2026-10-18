use super::{
    config::RetrieveRequest,
    lexical,
    types::{Dataset, QueryError, Result},
};
use reqwest::{Client, Method, Url};
#[path = "qdrant_grpc.rs"]
mod grpc;
#[path = "qdrant_grpc_config.rs"]
mod grpc_config;
#[path = "qdrant_grpc_values.rs"]
mod grpc_values;
#[path = "qdrant_resilience.rs"]
mod resilience;
use resilience::{Failure, Policy, Resilience};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QdrantRole {
    ApiReadOnly,
    WorkerWrite,
}

#[derive(Clone)]
pub struct QdrantStore {
    client: Client,
    role: QdrantRole,
    custom_ca: bool,
    grpc: Option<Arc<grpc::GrpcTransport>>,
    origin: Url,
    api_key: Option<String>,
    pub timeout: Duration,
    pub max_retries: usize,
    pub retry_base_delay: Duration,
    pub interactive_max_retries: usize,
    pub interactive_deadline: Duration,
    pub health_receipt_ttl: Duration,
    resilience: Resilience,
    pub bm25_v2_enabled: bool,
    pub bm25_v2_capability_ttl: Duration,
    capability_receipts: Arc<tokio::sync::Mutex<BTreeMap<String, Instant>>>,
    shadow_failures: Arc<AtomicU64>,
    shadow_failed_points: Arc<AtomicU64>,
}
#[derive(Clone, Debug)]
pub struct VectorHit {
    pub point_id: String,
    pub score: f64,
    pub payload: Value,
    pub vector: Option<Vec<f64>>,
}
#[path = "qdrant_security.rs"]
mod security;
impl QdrantStore {
    /// Clone the dedicated TLS client for service-owned health probes. No API key is installed in its defaults.
    pub fn health_http_client(&self) -> Client {
        self.client.clone()
    }

    /// Select the source client transport explicitly. No HTTP fallback on gRPC failure.
    pub fn set_prefer_grpc(&mut self, enabled: bool) -> Result<()> {
        if enabled && self.custom_ca {
            return Err(QueryError::Validation("custom Qdrant CA requires explicit REST transport; this gRPC client cannot install custom roots".into()));
        }
        self.grpc = if enabled {
            Some(Arc::new(grpc::GrpcTransport::new(
                &self.origin,
                self.api_key.clone(),
                self.timeout,
            )?))
        } else {
            None
        };
        Ok(())
    }

    /// Exact special-generation cleanup. Caller must hold the dataset publication lease.
    pub async fn delete_document_points_by_ids(
        &self,
        collection: &str,
        ids: &[String],
        dataset: &Dataset,
        document_id: &str,
        lifecycle_lease_held: bool,
    ) -> Result<()> {
        let ids = ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect::<BTreeSet<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        if !lifecycle_lease_held
            || dataset.tenant_id.trim().is_empty()
            || dataset.dataset_id.trim().is_empty()
            || document_id.trim().is_empty()
        {
            return Err(QueryError::Validation(
                "special point cleanup requires its dataset lease and owner".into(),
            ));
        }
        if !self.collection_exists(collection).await? {
            return Ok(());
        }
        let info = self.collection_info(collection).await?;
        let metadata = &info["config"]["metadata"];
        if let Some(scope) = metadata.get("knowledge_scope") {
            if !self.owns_scope(dataset, scope)? {
                return Err(QueryError::CollectionAuthority(
                    "special collection owner differs".into(),
                ));
            }
        }
        if let Some(profile) = metadata.get("knowledge_lexical") {
            super::lexical_v2::validate_collection_profile(profile)?;
            self.invalidate_receipt(collection, metadata, "special_generation_cleanup")
                .await?;
        }
        let filter =
            json!({"must":[Self::deletion_filter(dataset, Some(document_id))?,{"has_id":ids}]});
        let result = self
            .call(
                Method::POST,
                collection,
                &["points", "delete"],
                Some(&json!({"filter":filter})),
                true,
            )
            .await?;
        Self::completed(&result)?;
        if self.count_points(collection, Some(&filter)).await? != 0 {
            return Err(QueryError::VectorStore);
        }
        Ok(())
    }
    pub fn new(client: Client, origin: Url, api_key: Option<String>) -> Result<Self> {
        if !["http", "https"].contains(&origin.scheme())
            || origin.host_str().is_none()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(QueryError::Validation("invalid Qdrant origin".into()));
        }
        Ok(Self {
            client,
            role: QdrantRole::WorkerWrite,
            custom_ca: false,
            grpc: None,
            origin,
            api_key,
            timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            interactive_max_retries: 2,
            interactive_deadline: Duration::from_secs(3),
            health_receipt_ttl: Duration::from_secs(2),
            resilience: Resilience::default(),
            bm25_v2_enabled: false,
            bm25_v2_capability_ttl: Duration::from_secs(300),
            capability_receipts: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            shadow_failures: Arc::new(AtomicU64::new(0)),
            shadow_failed_points: Arc::new(AtomicU64::new(0)),
        })
    }
    fn url(&self, collection: &str, suffix: &[&str]) -> Result<Url> {
        if collection.trim().is_empty() {
            return Err(QueryError::VectorStore);
        }
        let mut url = self.origin.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| QueryError::VectorStore)?;
            path.pop_if_empty().push("collections").push(collection);
            for s in suffix {
                path.push(s);
            }
        }
        Ok(url)
    }
    fn policy(&self) -> Policy {
        Policy {
            max_retries: self.max_retries,
            retry_base_delay: self.retry_base_delay,
            interactive_max_retries: self.interactive_max_retries,
            interactive_deadline: self.interactive_deadline,
            health_receipt_ttl: self.health_receipt_ttl,
        }
    }
    /// Scope is restored on completion, error and cancellation. Nested entrypoints
    /// inherit the earlier absolute deadline, just like the source ContextVar.
    pub(super) async fn with_interactive_budget<F: std::future::Future>(
        &self,
        future: F,
    ) -> F::Output {
        resilience::with_budget(self.interactive_deadline, future).await
    }
    pub(super) fn publication_deadline(&self, fallback: Duration) -> Instant {
        let local = Instant::now() + fallback;
        resilience::current_deadline().map_or(local, |deadline| local.min(deadline.into_std()))
    }
    fn transport_failure(error: reqwest::Error) -> Failure<QueryError> {
        if error.is_timeout()
            || error.is_connect()
            || error.is_body()
            || (error.is_request() && !error.is_builder())
        {
            Failure::Transient(QueryError::VectorStore)
        } else {
            Failure::Permanent(QueryError::VectorStore)
        }
    }
    fn transport_error_class(error: &reqwest::Error) -> &'static str {
        if error.is_timeout() {
            "timeout"
        } else if error.is_connect() {
            "connect"
        } else if error.is_body() {
            "response_body"
        } else if error.is_builder() {
            "request_builder"
        } else {
            "request"
        }
    }
    fn resilience_failure_class<E>(failure: &Failure<E>) -> &'static str {
        match failure {
            Failure::Transient(_) => "transient_exhausted",
            Failure::Permanent(_) => "permanent",
            Failure::Timeout => "timeout",
            Failure::Deadline => "interactive_deadline",
            Failure::CircuitOpen => "circuit_open",
        }
    }
    fn qdrant_error_class(value: &Value) -> &'static str {
        let message = value
            .pointer("/status/error")
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
            .or_else(|| {
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .filter(|status| !status.eq_ignore_ascii_case("ok"))
            })
            .unwrap_or_default()
            .to_ascii_lowercase();
        if message.contains("dimension") {
            "vector_dimension_mismatch"
        } else if message.contains("collection")
            && (message.contains("not found") || message.contains("does not exist"))
        {
            "collection_not_found"
        } else if (message.contains("vector") || message.contains("field"))
            && (message.contains("not found") || message.contains("missing"))
        {
            "vector_field_missing"
        } else if message.contains("unauthorized")
            || message.contains("forbidden")
            || message.contains("api key")
        {
            "authorization"
        } else if message.contains("timeout") || message.contains("timed out") {
            "timeout"
        } else if message.contains("wrong input")
            || message.contains("invalid")
            || message.contains("parse")
        {
            "invalid_request"
        } else if message.is_empty() {
            "http_error"
        } else {
            "upstream_response_error"
        }
    }
    fn operation_name(method: &Method, suffix: &[&str]) -> &'static str {
        match (method.as_str(), suffix) {
            ("GET", ["collections"]) => "collections_list",
            ("GET", []) => "collection_info",
            ("PUT", []) => "collection_create",
            ("PATCH", []) => "collection_update",
            ("DELETE", []) => "collection_delete",
            ("POST", ["points", "query"]) => "points_query",
            ("POST", ["points", "query", "batch"]) => "points_query_batch",
            ("POST", ["points", "scroll"]) => "points_scroll",
            ("POST", ["points"]) => "points_get",
            _ => "other",
        }
    }
    fn dense_query_dimension(body: &Value) -> Option<usize> {
        body.get("query")
            .and_then(Value::as_array)
            .or_else(|| {
                body.get("prefetch")
                    .and_then(Value::as_array)
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("query"))
                    .and_then(Value::as_array)
            })
            .map(Vec::len)
    }
    fn collection_vector_dimension(info: &Value) -> Option<usize> {
        let vectors = info.pointer("/config/params/vectors")?;
        vectors
            .get("size")
            .and_then(Value::as_u64)
            .map(|size| size as usize)
            .or_else(|| {
                vectors
                    .as_object()?
                    .values()
                    .find_map(|vector| vector.get("size").and_then(Value::as_u64))
                    .map(|size| size as usize)
            })
    }
    fn status_failure(status: reqwest::StatusCode) -> Failure<QueryError> {
        if status.as_u16() == 429 || status.is_server_error() {
            Failure::Transient(QueryError::VectorStore)
        } else {
            Failure::Permanent(QueryError::VectorStore)
        }
    }
    async fn request_json(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
        operation: &'static str,
        interactive: bool,
    ) -> Result<Value> {
        match self
            .resilience
            .execute(&self.policy(), self.timeout, interactive, |timeout| {
                let mut request = self
                    .client
                    .request(method.clone(), url.clone())
                    .timeout(timeout);
                if let Some(key) = &self.api_key {
                    request = request.header("api-key", key);
                }
                if let Some(body) = body {
                    request = request.json(body);
                }
                async move {
                    let response = request.send().await.map_err(|error| {
                        tracing::warn!(
                            target: "ai_platform_knowledge::qdrant",
                            operation,
                            error_class = Self::transport_error_class(&error),
                            "Qdrant request transport failed"
                        );
                        Self::transport_failure(error)
                    })?;
                    if !response.status().is_success() {
                        let status = response.status();
                        let error_body = response.json::<Value>().await.unwrap_or(Value::Null);
                        tracing::warn!(
                            target: "ai_platform_knowledge::qdrant",
                            operation,
                            http_status = status.as_u16(),
                            error_class = Self::qdrant_error_class(&error_body),
                            "Qdrant request returned an error status"
                        );
                        return Err(Self::status_failure(status));
                    }
                    let body: Value = response.json().await.map_err(|error| {
                        tracing::warn!(
                            target: "ai_platform_knowledge::qdrant",
                            operation,
                            error_class = "invalid_response_json",
                            "Qdrant response body was not valid JSON"
                        );
                        Self::transport_failure(error)
                    })?;
                    if body["status"].as_str().is_some_and(|status| status != "ok") {
                        tracing::warn!(
                            target: "ai_platform_knowledge::qdrant",
                            operation,
                            http_status = 200,
                            error_class = Self::qdrant_error_class(&body),
                            "Qdrant response reported an error"
                        );
                        return Err(Failure::Permanent(QueryError::VectorStore));
                    }
                    Ok(body["result"].clone())
                }
            })
            .await
        {
            Ok(value) => Ok(value),
            Err(failure) => {
                tracing::warn!(
                    target: "ai_platform_knowledge::qdrant",
                    operation,
                    failure_class = Self::resilience_failure_class(&failure),
                    "Qdrant request was not completed"
                );
                Err(QueryError::VectorStore)
            }
        }
    }
    async fn call(
        &self,
        method: Method,
        collection: &str,
        suffix: &[&str],
        body: Option<&Value>,
        write: bool,
    ) -> Result<Value> {
        self.call_mode(
            method,
            collection,
            suffix,
            body,
            write,
            resilience::interactive_calls(),
        )
        .await
    }
    async fn call_interactive(
        &self,
        method: Method,
        collection: &str,
        suffix: &[&str],
        body: Option<&Value>,
        write: bool,
    ) -> Result<Value> {
        self.call_mode(method, collection, suffix, body, write, true)
            .await
    }
    async fn call_mode(
        &self,
        method: Method,
        collection: &str,
        suffix: &[&str],
        body: Option<&Value>,
        write: bool,
        interactive: bool,
    ) -> Result<Value> {
        let read_operation = method == Method::GET
            || (method == Method::POST
                && matches!(
                    suffix,
                    ["points"]
                        | ["points", "query"]
                        | ["points", "query", "batch"]
                        | ["points", "scroll"]
                        | ["points", "count"]
                        | ["points", "search"]
                        | ["points", "search", "batch"]
                ));
        if !read_operation && self.is_read_only() {
            return Err(QueryError::Validation(
                "Qdrant mutation requires the durable Knowledge worker".into(),
            ));
        }
        if let Some(grpc) = &self.grpc {
            let method = &method;
            let result = self
                .resilience
                .execute(
                    &self.policy(),
                    self.timeout,
                    interactive,
                    |timeout| async move {
                        tokio::time::timeout(timeout, grpc.call(method, collection, suffix, body))
                            .await
                            .unwrap_or(Err(Failure::Timeout))
                    },
                )
                .await;
            return match result {
                Ok(value) => Ok(value),
                Err(failure) => {
                    tracing::warn!(
                        target: "ai_platform_knowledge::qdrant",
                        operation = Self::operation_name(method, suffix),
                        failure_class = Self::resilience_failure_class(&failure),
                        "Qdrant gRPC request was not completed"
                    );
                    Err(QueryError::VectorStore)
                }
            };
        }
        let mut url = self.url(collection, suffix)?;
        if write {
            url.query_pairs_mut().append_pair("wait", "true");
        }
        self.request_json(
            method.clone(),
            url,
            body,
            Self::operation_name(&method, suffix),
            interactive,
        )
        .await
    }
    pub(super) async fn delete_collection_scoped(&self, dataset: &Dataset) -> Result<()> {
        if self.collection_info(dataset.collection()?).await?["config"]["metadata"]["knowledge_scope"]
            ["schema_version"]
            == 2
        {
            let filter = Self::deletion_filter(dataset, None)?;
            let result = self
                .call(
                    Method::POST,
                    dataset.collection()?,
                    &["points", "delete"],
                    Some(&json!({"filter":filter})),
                    true,
                )
                .await?;
            return Self::completed(&result);
        }

        let deleted = self
            .call(Method::DELETE, dataset.collection()?, &[], None, false)
            .await?;
        if deleted == Value::Bool(true) {
            Ok(())
        } else {
            Err(QueryError::VectorStore)
        }
    }
    pub async fn collection_info(&self, name: &str) -> Result<Value> {
        self.call(Method::GET, name, &[], None, false).await
    }
    pub async fn collection_exists(&self, name: &str) -> Result<bool> {
        if name.is_empty() {
            return Ok(false);
        }
        if let Some(grpc) = &self.grpc {
            return self
                .resilience
                .execute(&self.policy(), self.timeout, false, |timeout| async move {
                    tokio::time::timeout(timeout, grpc.exists(name))
                        .await
                        .unwrap_or(Err(Failure::Timeout))
                })
                .await
                .map_err(|_| QueryError::VectorStore);
        }
        let url = self.url(name, &[])?;
        self.resilience
            .execute(&self.policy(), self.timeout, false, |timeout| {
                let mut request = self.client.get(url.clone()).timeout(timeout);
                if let Some(key) = &self.api_key {
                    request = request.header("api-key", key);
                }
                async move {
                    let response = request.send().await.map_err(Self::transport_failure)?;
                    if response.status().as_u16() == 404 {
                        Ok(false)
                    } else if response.status().is_success() {
                        Ok(true)
                    } else {
                        Err(Self::status_failure(response.status()))
                    }
                }
            })
            .await
            .map_err(|_| QueryError::VectorStore)
    }
    pub async fn require_collection_readable(&self, dataset: &Dataset) -> Result<Value> {
        let name = dataset.collection()?;
        let info = self
            .call_interactive(Method::GET, name, &[], None, false)
            .await?;
        let metadata = &info["config"]["metadata"];
        if let Some(profile) =
            super::serving_profile::require(dataset)?.filter(|p| p.index.is_some())
        {
            profile.verify_layout(&info)?;
            if metadata["knowledge_serving"]["index_fingerprint"]
                != super::lexical_v2::fingerprint(
                    &serde_json::to_value(&profile.index).map_err(|_| QueryError::VectorStore)?,
                )
            {
                return Err(QueryError::CollectionAuthority(
                    "optimized index profile requires a measured blue-green collection migration"
                        .into(),
                ));
            }
        } else if metadata["knowledge_serving"]["index_fingerprint"].is_string() {
            return Err(QueryError::CollectionAuthority(
                "optimized index cannot be silently adopted by a legacy profile".into(),
            ));
        }
        if let Some(scope) = metadata.get("knowledge_scope") {
            if !self.owns_scope(dataset, scope)? {
                return Err(QueryError::CollectionAuthority(
                    "collection immutable scope differs".into(),
                ));
            }
        } else if dataset.tenant_id.trim().is_empty() || dataset.dataset_id.trim().is_empty() {
            return Err(QueryError::CollectionAuthority(
                "legacy collection requires explicit dataset and tenant scope".into(),
            ));
        }
        if let Some(profile) = metadata.get("knowledge_lexical") {
            super::lexical_v2::validate_collection_profile(profile)?;
        }
        if dataset.index_config["retrieval"]["lexical"]["active_version"].as_str()
            == Some("bm25_v2")
        {
            if !self.bm25_v2_enabled {
                return Err(QueryError::Validation(
                    "bm25_v2 is disabled by the service kill switch".into(),
                ));
            }
            if metadata["knowledge_lexical"]["active_version"] != "bm25_v2" {
                return Err(QueryError::CollectionAuthority(
                    "collection is not cut over to bm25_v2".into(),
                ));
            }
        }
        Ok(info)
    }
    pub async fn ensure_collection(&self, dataset: &Dataset, dimension: usize) -> Result<String> {
        self.ensure_collection_options(dataset, dimension, true, false)
            .await
    }
    pub(super) async fn ensure_collection_policy(
        &self,
        dataset: &Dataset,
        dimension: usize,
        allow_existing: bool,
    ) -> Result<String> {
        self.ensure_collection_options(dataset, dimension, allow_existing, false)
            .await
    }
    /// Administrative dataset-patch path. The caller owns the dataset mutation
    /// lease and SQL compare-and-swap; active selection is lifecycle-only.
    pub async fn reconcile_lexical_collection(
        &self,
        dataset: &Dataset,
        dimension: usize,
    ) -> Result<String> {
        self.ensure_collection_options(dataset, dimension, true, true)
            .await
    }
    async fn ensure_collection_options(
        &self,
        dataset: &Dataset,
        dimension: usize,
        allow_existing: bool,
        lexical_transition: bool,
    ) -> Result<String> {
        if dataset.dataset_id.trim().is_empty()
            || dataset.tenant_id.trim().is_empty()
            || dimension == 0
        {
            return Err(QueryError::Validation(
                "collection ownership and dimension are required".into(),
            ));
        }
        if let Some(profile) = super::serving_profile::require(dataset)? {
            if let Some(shared) = &profile.shared {
                let requested = dataset
                    .collection_name
                    .as_deref()
                    .filter(|name| {
                        *name == shared.collection
                            || *name == format!("{}_sections", shared.collection)
                            || *name == format!("{}_summary", shared.collection)
                    })
                    .unwrap_or(&shared.collection);
                let mut scoped = dataset.clone();
                scoped.collection_name = Some(requested.into());
                let info = self.require_collection_readable(&scoped).await?;
                if info["config"]["params"]["vectors"]["size"].as_u64() != Some(dimension as u64) {
                    return Err(QueryError::CollectionAuthority(
                        "shared group model dimension differs".into(),
                    ));
                }
                return Ok(requested.into());
            }
        }
        let sanitize = super::migration::sanitize;
        let desired = sanitize(dataset.collection_name.as_deref().unwrap_or(&format!(
            "kb_{}_{}",
            sanitize(&dataset.dataset_id),
            dimension
        )));
        let mut actual = desired.clone();
        if self.collection_exists(&desired).await? {
            if !allow_existing {
                return Err(QueryError::Validation("collection already exists".into()));
            }
            let info = self.collection_info(&desired).await?;
            if info["config"]["params"]["vectors"]["size"].as_u64() == Some(dimension as u64) {
                self.finish_existing_collection(dataset, &desired, dimension, lexical_transition)
                    .await?;
                return Ok(desired);
            }
            actual = sanitize(&format!("{desired}_d{dimension}"));
        }
        let lexical = super::lexical_v2::LexicalProfile::from_dataset(dataset)?;
        if lexical.active {
            return Err(QueryError::Validation(
                "bm25_v2 active mode requires a completed shadow backfill receipt".into(),
            ));
        }
        let mut sparse = json!({"bm25":{"modifier":"idf"}});
        let mut metadata = json!({"knowledge_scope":{"schema_version":1,"dataset_id":dataset.dataset_id,"tenant_id":dataset.tenant_id}});
        if lexical.shadow {
            self.ensure_bm25_v2_capability(&lexical).await?;
            sparse["bm25_v2"] = json!({"modifier":"idf"});
            metadata["knowledge_lexical"] = lexical.collection_profile();
        }
        let mut body = json!({"vectors":{"size":dimension,"distance":"Cosine"},"sparse_vectors":sparse,"metadata":metadata,"hnsw_config":{"m":16,"ef_construct":200,"full_scan_threshold":10000},"optimizers_config":{"indexing_threshold":20000}});
        if let Some(profile) = super::serving_profile::require(dataset)? {
            profile.collection_config(&mut body);
            if profile.index.is_some() {
                body["metadata"]["knowledge_serving"] = json!({"schema":"knowledge-serving/v1","fingerprint":profile.fingerprint(dataset),"index_fingerprint":super::lexical_v2::fingerprint(&serde_json::to_value(&profile.index).map_err(|_|QueryError::VectorStore)?)});
            }
        }
        let created = self
            .call(Method::PUT, &actual, &[], Some(&body), false)
            .await?;
        if created != true {
            if allow_existing {
                self.finish_existing_collection(dataset, &actual, dimension, lexical_transition)
                    .await?;
                return Ok(actual);
            }
            return Err(QueryError::CollectionAuthority(
                "collection could not be claimed".into(),
            ));
        }
        let post_create: Result<()> = async {
            for field in [
                "tenant_id",
                "dataset_id",
                "document_id",
                "segment_id",
                "source_type",
                "language",
                "madhab",
                "authority_rank",
                "section_title",
                "metadata.topic",
                "metadata.keywords",
                "enabled",
                "level",
                "content_type",
                "_rollback_backup",
                "source_version",
                "metadata.source_path",
            ] {
                let schema = if field == "tenant_id" {
                    json!({"type":"keyword","is_tenant":true})
                } else if field == "metadata.source_path"
                    && dataset.index_config["serving_v2"]["index"].is_object()
                {
                    json!({"type":"keyword","prefix":{}})
                } else if ["enabled", "_rollback_backup"].contains(&field) {
                    json!("bool")
                } else if ["level", "source_version"].contains(&field) {
                    json!("integer")
                } else if field == "authority_rank" {
                    json!("integer")
                } else {
                    json!("keyword")
                };
                let installed = self
                    .call(
                        Method::PUT,
                        &actual,
                        &["index"],
                        Some(&json!({"field_name":field,"field_schema":schema})),
                        true,
                    )
                    .await;
                if dataset.index_config["serving_v2"]["index"].is_object()
                    && !matches!(installed, Ok(Value::Bool(true)))
                {
                    return Err(QueryError::CollectionAuthority(
                        "optimized collection payload index installation failed".into(),
                    ));
                }
            }
            if lexical.shadow {
                self.ensure_filtering_profile(&actual, &lexical, true, Some(false), true)
                    .await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = post_create {
            let _ = self.call(Method::DELETE, &actual, &[], None, false).await;
            return Err(error);
        }
        Ok(actual)
    }
    async fn finish_existing_collection(
        &self,
        dataset: &Dataset,
        name: &str,
        dimension: usize,
        lexical_transition: bool,
    ) -> Result<()> {
        let mut scoped = dataset.clone();
        scoped.collection_name = Some(name.into());
        let info = self.collection_info(name).await?;
        if info["config"]["params"]["vectors"]["size"].as_u64() != Some(dimension as u64) {
            return Err(QueryError::CollectionAuthority(
                "collection dimension mismatch".into(),
            ));
        }
        let info = self.ensure_immutable_scope(&scoped, info).await?;
        if super::serving_profile::Profile::from_dataset(dataset)?
            .is_some_and(|p| p.index.is_some())
        {
            self.require_collection_readable(&scoped).await?;
        }
        // Legacy sparse installation is best effort in the Python adapter.
        if info["config"]["params"]["sparse_vectors"]
            .get("bm25")
            .is_none()
        {
            let _ = self
                .call(
                    Method::PATCH,
                    name,
                    &[],
                    Some(&json!({"sparse_vectors":{"bm25":{"modifier":"idf"}}})),
                    false,
                )
                .await;
        }
        let requested = super::lexical_v2::LexicalProfile::from_dataset(dataset)?;
        self.ensure_lexical_selection(
            &scoped,
            &requested,
            !dataset.index_config["retrieval"]["lexical"].is_null(),
            lexical_transition,
            false,
        )
        .await?;
        Ok(())
    }
    fn owns_scope(&self, dataset: &Dataset, scope: &Value) -> Result<bool> {
        match scope["schema_version"].as_u64() {
            Some(1) => {
                if scope["tenant_id"].as_str().is_none_or(|s| s.is_empty())
                    || scope["dataset_id"].as_str().is_none_or(|s| s.is_empty())
                {
                    return Err(QueryError::CollectionAuthority(
                        "malformed immutable collection scope".into(),
                    ));
                }
                Ok(scope["tenant_id"] == dataset.tenant_id
                    && scope["dataset_id"] == dataset.dataset_id)
            }
            Some(2) => {
                if scope["group"]
                    .as_str()
                    .is_none_or(|s| !s.starts_with("kbs_"))
                    || !scope["layout"].is_string()
                {
                    return Err(QueryError::CollectionAuthority(
                        "malformed shared collection scope".into(),
                    ));
                }
                Ok(super::serving_profile::require(dataset)?
                    .and_then(|p| p.shared_scope(dataset))
                    .as_ref()
                    == Some(scope))
            }
            _ => Err(QueryError::CollectionAuthority(
                "unknown immutable collection scope".into(),
            )),
        }
    }
    fn require_write_scope(&self, dataset: &Dataset, info: &Value) -> Result<()> {
        if !self.owns_scope(dataset, &info["config"]["metadata"]["knowledge_scope"])? {
            return Err(QueryError::CollectionAuthority(
                "collection immutable scope differs".into(),
            ));
        }
        Ok(())
    }
    fn completed(result: &Value) -> Result<()> {
        if result["status"].as_str() == Some("completed") {
            Ok(())
        } else {
            Err(QueryError::VectorStore)
        }
    }
    pub fn filter(dataset: &Dataset, request: Option<&RetrieveRequest>, enabled: bool) -> Value {
        let mut must = vec![
            json!({"key":"tenant_id","match":{"value":dataset.tenant_id}}),
            json!({"key":"dataset_id","match":{"value":dataset.dataset_id}}),
        ];
        if enabled {
            must.push(json!({"should":[{"key":"enabled","match":{"value":true}},{"is_empty":{"key":"enabled"}}]}))
        }
        if let Some(r) = request {
            if dataset.index_config["serving_v2"]["index"].is_object() {
                if let Some(prefix) = r
                    .extra
                    .get("source_path_prefix")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 1024)
                {
                    must.push(json!({"key":"metadata.source_path","match":{"prefix":prefix}}));
                }
            }
            for (key, value) in [
                ("document_id", r.document_id.as_deref()),
                ("source_type", r.source_type_filter.as_deref()),
                ("language", r.language_filter.as_deref()),
            ] {
                if let Some(v) = value.filter(|v| !v.is_empty()) {
                    must.push(json!({"key":key,"match":{"value":v}}))
                }
            }
            if let Some(m) = r.metadata_filter.as_ref().and_then(Value::as_object) {
                for (k, v) in m {
                    if v.is_string() || v.is_i64() || v.is_u64() || v.is_boolean() {
                        must.push(json!({"key":format!("metadata.{k}"),"match":{"value":v}}))
                    }
                }
            }
        }
        json!({"must":must})
    }
    pub async fn search(
        &self,
        dataset: &Dataset,
        vector: &[f64],
        top_k: usize,
        request: &RetrieveRequest,
    ) -> Result<Vec<VectorHit>> {
        let info = self.require_collection_readable(dataset).await?;
        let collection_dimension = Self::collection_vector_dimension(&info);
        let body = json!({"query":vector,"limit":top_k,"filter":Self::filter(dataset,Some(request),true),"with_payload":true,"with_vector":false});
        self.query(dataset, &body, collection_dimension).await
    }
    async fn query(
        &self,
        dataset: &Dataset,
        body: &Value,
        collection_dimension: Option<usize>,
    ) -> Result<Vec<VectorHit>> {
        let mut body = body.clone();
        if let Some(profile) = super::serving_profile::require(dataset)? {
            if let Some(shard) = profile.shard_key() {
                body["shard_key"] = shard;
            }
            let params = profile.search_params(dataset);
            if !params.is_null() {
                body["params"] = params.clone();
                if let Some(prefetch) = body["prefetch"].as_array_mut() {
                    for q in prefetch {
                        q["params"] = params.clone();
                    }
                }
            }
        }
        let body = &body;
        let query_dimension = Self::dense_query_dimension(body);
        let result = self
            .call_interactive(
                Method::POST,
                dataset.collection()?,
                &["points", "query"],
                Some(body),
                false,
            )
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: "ai_platform_knowledge::qdrant",
                    operation = "points_query",
                    query_vector_dimension = ?query_dimension,
                    dataset_embedding_dimension = ?dataset.embedding_dimension,
                    collection_vector_dimension = ?collection_dimension,
                    failure_class = "request_failed",
                    "Qdrant vector query failed"
                );
                error
            })?;
        Self::hits(&result["points"]).map_err(|error| {
            tracing::warn!(
                target: "ai_platform_knowledge::qdrant",
                operation = "points_query",
                query_vector_dimension = ?query_dimension,
                dataset_embedding_dimension = ?dataset.embedding_dimension,
                collection_vector_dimension = ?collection_dimension,
                failure_class = "invalid_points_result",
                "Qdrant vector query response was not a valid points result"
            );
            error
        })
    }
    fn hits(v: &Value) -> Result<Vec<VectorHit>> {
        v.as_array()
            .ok_or(QueryError::VectorStore)?
            .iter()
            .map(|p| {
                Ok(VectorHit {
                    point_id: id_string(&p["id"]).ok_or(QueryError::VectorStore)?,
                    score: p["score"]
                        .as_f64()
                        .filter(|n| n.is_finite())
                        .ok_or(QueryError::VectorStore)?,
                    payload: p["payload"].clone(),
                    vector: vector_values(&p["vector"]),
                })
            })
            .collect()
    }
    pub async fn sparse_search(
        &self,
        dataset: &Dataset,
        query: &str,
        top_k: usize,
        request: &RetrieveRequest,
    ) -> Result<Vec<VectorHit>> {
        let info = self.require_collection_readable(dataset).await?;
        let collection_dimension = Self::collection_vector_dimension(&info);
        let profile = super::lexical_v2::LexicalProfile::from_dataset(dataset)?;
        let (query, using) = if profile.active {
            self.verify_bm25_v2_readiness(dataset, &profile).await?;
            (profile.document(query), "bm25_v2")
        } else {
            let (i, v) = lexical::sparse_vector(query, true);
            if i.is_empty() {
                return Ok(vec![]);
            }
            (json!({"indices":i,"values":v}), "bm25")
        };
        self.query(
            dataset,
            &json!({"query":query,"using":using,"limit":top_k,"filter":Self::filter(dataset,Some(request),true),"with_payload":true}),
            collection_dimension,
        )
        .await
    }
    pub async fn hybrid_search_native(
        &self,
        dataset: &Dataset,
        query: &str,
        vector: &[f64],
        top_k: usize,
        dense_k: usize,
        sparse_k: usize,
        rrf_k: usize,
        weights: Option<(f64, f64)>,
        request: &RetrieveRequest,
    ) -> Result<Vec<VectorHit>> {
        let info = self.require_collection_readable(dataset).await?;
        let collection_dimension = Self::collection_vector_dimension(&info);
        let profile = super::lexical_v2::LexicalProfile::from_dataset(dataset)?;
        let (sparse, using) = if profile.active {
            self.verify_bm25_v2_readiness(dataset, &profile).await?;
            (profile.document(query), "bm25_v2")
        } else {
            let (i, v) = lexical::sparse_vector(query, true);
            (json!({"indices":i,"values":v}), "bm25")
        };
        let filter = Self::filter(dataset, Some(request), true);
        let mut rrf = json!({"k":rrf_k});
        if let Some((d, b)) = weights {
            rrf["weights"] = json!([d, b]);
        }
        self.query(
            dataset,
            &json!({"prefetch":[{"query":vector,"limit":dense_k,"filter":filter},{"query":sparse,"using":using,"limit":sparse_k,"filter":filter}],"query":{"rrf":rrf},"limit":top_k,"with_payload":true}),
            collection_dimension,
        )
        .await
    }
    pub async fn snapshot_points(&self, dataset: &Dataset, ids: &[String]) -> Result<Vec<Value>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        self.require_collection_readable(dataset).await?;
        let value = self
            .call(
                Method::POST,
                dataset.collection()?,
                &["points"],
                Some(&json!({"ids":ids,"with_payload":true,"with_vector":true})),
                false,
            )
            .await?;
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for p in value.as_array().ok_or(QueryError::VectorStore)? {
            let mut p = p.clone();
            let id = id_string(&p["id"]).ok_or(QueryError::VectorStore)?;
            if !ids.contains(&id) || !seen.insert(id) || p["vector"].is_null() {
                return Err(QueryError::VectorStore);
            }
            for (key, expected) in [
                ("tenant_id", &dataset.tenant_id),
                ("dataset_id", &dataset.dataset_id),
            ] {
                if super::serving_profile::Profile::from_dataset(dataset)?
                    .is_some_and(|p| p.shared.is_some())
                    && p["payload"][key].as_str() != Some(expected.as_str())
                {
                    return Err(QueryError::CollectionAuthority(
                        "shared snapshot lacks exact owner".into(),
                    ));
                }
                if p["payload"][key]
                    .as_str()
                    .is_some_and(|v| !v.is_empty() && v != expected)
                {
                    return Err(QueryError::CollectionAuthority(
                        "snapshot point escaped dataset/tenant scope".into(),
                    ));
                }
                p["payload"][key] = json!(expected)
            }
            out.push(p)
        }
        Ok(out)
    }
    pub async fn retrieve_vectors(
        &self,
        dataset: &Dataset,
        ids: &[String],
    ) -> Result<BTreeMap<String, Vec<f64>>> {
        resilience::with_interactive_calls(async {
            let mut result = BTreeMap::new();
            for p in self.snapshot_points(dataset, ids).await? {
                if let Some(v) = vector_values(&p["vector"]) {
                    result.insert(id_string(&p["id"]).ok_or(QueryError::VectorStore)?, v);
                }
            }
            Ok(result)
        })
        .await
    }
    pub async fn upsert_points(&self, dataset: &Dataset, points: &[Value]) -> Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        // A fresh collection profile is authoritative over stale caller config.
        let info = self.collection_info(dataset.collection()?).await?;
        self.require_write_scope(dataset, &info)?;
        let metadata = &info["config"]["metadata"];
        let raw = metadata.get("knowledge_lexical");
        let profile = if let Some(raw) = raw {
            super::lexical_v2::validate_collection_profile(raw)?;
            super::lexical_v2::LexicalProfile::from_collection(raw)?
        } else {
            let mut blank = dataset.clone();
            blank.index_config = json!({});
            super::lexical_v2::LexicalProfile::from_dataset(&blank)?
        };
        let eligible = super::lexical_v2::eligible;
        let shadow_only = profile.shadow && !profile.active;
        let mut invalidation_failed = false;
        if raw.is_some() && points.iter().any(|p| eligible(&p["payload"])) {
            if let Err(error) = self
                .invalidate_receipt(dataset.collection()?, metadata, "runtime_upsert")
                .await
            {
                if profile.active {
                    return Err(error);
                }
                invalidation_failed = true;
            }
        }
        if profile.active && !self.bm25_v2_enabled {
            return Err(QueryError::Validation("bm25_v2 is disabled".into()));
        }
        let preflight: Result<()> = async {
            if !profile.shadow {
                return Ok(());
            }
            for field in ["bm25", "bm25_v2"] {
                if info["config"]["params"]["sparse_vectors"][field]["modifier"] != "idf" {
                    return Err(QueryError::CollectionAuthority(
                        "bm25_v2 writes require both sparse fields with IDF".into(),
                    ));
                }
            }
            self.ensure_bm25_v2_capability(&profile).await?;
            self.ensure_filtering_profile(
                dataset.collection()?,
                &profile,
                false,
                Some(profile.active),
                true,
            )
            .await?;
            Ok(())
        }
        .await;
        if profile.active {
            preflight.as_ref().map_err(Clone::clone)?;
        }
        let preflight_failed = preflight.is_err();
        let mut prepared = Vec::new();
        let mut updates = Vec::new();
        let mut invalid_shadow = 0u64;
        for point in points {
            let mut p = point.clone();
            let text = p["payload"]["text"].as_str().unwrap_or("").to_owned();
            let is_eligible = eligible(&p["payload"]);
            if p["id"].is_null() {
                return Err(QueryError::VectorStore);
            }
            for (key, value) in [
                ("tenant_id", &dataset.tenant_id),
                ("dataset_id", &dataset.dataset_id),
            ] {
                if p["payload"][key]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.trim() != value)
                {
                    return Err(QueryError::CollectionAuthority(
                        "point escaped immutable collection scope".into(),
                    ));
                }
                p["payload"][key] = json!(value)
            }
            if p["vector"].is_array() {
                let dense = p["vector"].clone();
                p["vector"] = json!({"":dense});
            }
            if !p["vector"].is_object() {
                if profile.active {
                    return Err(QueryError::VectorStore);
                }
                invalid_shadow += u64::from(shadow_only);
                prepared.push(p);
                continue;
            }
            p["vector"]
                .as_object_mut()
                .ok_or(QueryError::VectorStore)?
                .remove("bm25_v2");
            if info["config"]["params"]["sparse_vectors"]
                .get("bm25")
                .is_some()
                && p["vector"].get("bm25").is_none()
            {
                let (i, v) = lexical::sparse_vector(&text, false);
                let (i, v) = if i.is_empty() {
                    (vec![0], vec![1.0])
                } else {
                    (i, v)
                };
                p["vector"]["bm25"] = json!({"indices":i,"values":v});
            }
            let mut marker = if p["payload"]["_lexical"].is_object() {
                p["payload"]["_lexical"].clone()
            } else {
                json!({})
            };
            let mut versions = marker["versions"].as_array().cloned().unwrap_or_default();
            for version in [json!("lexical_v1"), json!("bm25_v2")] {
                if !versions.contains(&version) {
                    versions.push(version)
                }
            }
            if raw.is_some() {
                if let Some(payload) = p["payload"].as_object_mut() {
                    payload.remove("_lexical");
                }
            }
            for (k, v) in profile
                .marker(&text)
                .as_object()
                .ok_or(QueryError::VectorStore)?
            {
                marker[k] = v.clone();
            }
            marker["versions"] = json!(versions);
            if profile.active && is_eligible {
                if text.trim().is_empty() {
                    return Err(QueryError::VectorStore);
                }
                p["vector"]["bm25_v2"] = profile.document(&text);
                p["payload"]["_lexical"] = marker;
            } else if shadow_only && !preflight_failed && is_eligible {
                if text.trim().is_empty() {
                    invalid_shadow += 1;
                } else {
                    updates.push((p["id"].clone(), text, marker));
                }
            }
            prepared.push(p);
        }
        let dimension = prepared
            .first()
            .and_then(|p| p["vector"].get("").or_else(|| p["vector"].get("dense")))
            .and_then(Value::as_array)
            .map(Vec::len);
        if dimension
            .is_some_and(|n| info["config"]["params"]["vectors"]["size"].as_u64() != Some(n as u64))
        {
            return Err(QueryError::CollectionAuthority(
                "point dimension differs from collection".into(),
            ));
        }
        let batch_size = match prepared.len() {
            0..=50 => 32,
            51..=200 => 16,
            201..=500 => 8,
            _ => 4,
        };
        for batch in prepared.chunks(batch_size) {
            let mut batch_body = json!({"points":batch});
            if let Some(key) = super::serving_profile::require(dataset)?.and_then(|p| p.shard_key())
            {
                batch_body["shard_key"] = key;
            }
            let result = self
                .call(
                    Method::PUT,
                    dataset.collection()?,
                    &["points"],
                    Some(&batch_body),
                    true,
                )
                .await?;
            Self::completed(&result)?;
        }
        if shadow_only {
            let mut failed = invalid_shadow;
            let mut failure = preflight_failed || invalidation_failed;
            if !preflight_failed && !updates.is_empty() {
                let update:Result<()>=async{
                let vectors:Vec<_>=updates.iter().map(|(id,text,_)|json!({"id":id,"vector":{"bm25_v2":profile.document(text)}})).collect();
                let result=self.call(Method::PUT,dataset.collection()?,&["points","vectors"],Some(&json!({"points":vectors})),true).await?;Self::completed(&result)?;
                for(id,_,marker)in &updates{let result=self.call(Method::POST,dataset.collection()?,&["points","payload"],Some(&json!({"points":[id],"payload":{"_lexical":marker}})),true).await?;Self::completed(&result)?;}Ok(())
            }.await;
                if update.is_err() {
                    failure = true;
                    failed += updates.len() as u64;
                }
            }
            if failure || failed > 0 {
                self.shadow_failures.fetch_add(1, Ordering::Relaxed);
                self.shadow_failed_points.fetch_add(
                    failed.max(if failure && failed == 0 {
                        points.len() as u64
                    } else {
                        0
                    }),
                    Ordering::Relaxed,
                );
            }
        }
        Ok(())
    }
    pub fn bm25_v2_shadow_write_stats(&self) -> Value {
        json!({"failures":self.shadow_failures.load(Ordering::Relaxed),"failed_points":self.shadow_failed_points.load(Ordering::Relaxed)})
    }
    pub async fn delete_points(&self, dataset: &Dataset, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.require_collection_readable(dataset).await?;
        let mut filter = Self::filter(dataset, None, false);
        filter["must"]
            .as_array_mut()
            .ok_or(QueryError::VectorStore)?
            .push(json!({"has_id":ids}));
        let r = self
            .call(
                Method::POST,
                dataset.collection()?,
                &["points", "delete"],
                Some(&json!({"filter":filter})),
                true,
            )
            .await?;
        Self::completed(&r)
    }
    pub async fn scroll(
        &self,
        dataset: &Dataset,
        filter: &Value,
        with_vectors: bool,
    ) -> Result<Vec<Value>> {
        self.require_collection_readable(dataset).await?;
        let mut offset = Value::Null;
        let mut out = Vec::new();
        let mut offsets = BTreeSet::new();
        loop {
            let scoped_filter =
                if super::serving_profile::require(dataset)?.is_some_and(|p| p.shared.is_some()) {
                    json!({"must":[filter,Self::filter(dataset,None,false)]})
                } else {
                    filter.clone()
                };
            let body = json!({"filter":scoped_filter,"limit":256,"offset":offset,"with_payload":true,"with_vector":with_vectors});
            let result = self
                .call(
                    Method::POST,
                    dataset.collection()?,
                    &["points", "scroll"],
                    Some(&body),
                    false,
                )
                .await?;
            out.extend(
                result["points"]
                    .as_array()
                    .ok_or(QueryError::VectorStore)?
                    .iter()
                    .cloned(),
            );
            offset = result["next_page_offset"].clone();
            if offset.is_null() {
                break;
            }
            if !offsets.insert(offset.to_string()) {
                return Err(QueryError::VectorStore);
            }
        }
        Ok(out)
    }
    pub async fn verify_bm25_v2_readiness(
        &self,
        dataset: &Dataset,
        profile: &super::lexical_v2::LexicalProfile,
    ) -> Result<Value> {
        let result = resilience::with_interactive_calls(async {
            if !self.bm25_v2_enabled {
                return Err(QueryError::Validation("bm25_v2 is disabled".into()));
            }
            let info = self.require_collection_readable(dataset).await?;
            super::lexical_v2::verify_active(self, dataset, profile, &info).await
        })
        .await;
        super::pipeline_metrics::bm25_readiness(if result.is_ok() { "miss" } else { "failure" });
        result
    }
}
pub fn id_string(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
}
fn vector_values(v: &Value) -> Option<Vec<f64>> {
    let v = if v.is_object() { v.get("")? } else { v };
    v.as_array()?.iter().map(Value::as_f64).collect()
}

impl QdrantStore {
    async fn list_collection_names(&self) -> Result<Vec<String>> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| QueryError::VectorStore)?
            .pop_if_empty()
            .push("collections");
        let data = if let Some(grpc) = &self.grpc {
            self.resilience
                .execute(&self.policy(), self.timeout, false, |timeout| async move {
                    tokio::time::timeout(timeout, grpc.list())
                        .await
                        .unwrap_or(Err(Failure::Timeout))
                })
                .await
                .map_err(|_| QueryError::VectorStore)?
        } else {
            self.request_json(
                Method::GET,
                url,
                None,
                Self::operation_name(&Method::GET, &["collections"]),
                false,
            )
            .await?
        };
        data["collections"]
            .as_array()
            .ok_or(QueryError::VectorStore)?
            .iter()
            .map(|c| {
                c["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .ok_or(QueryError::VectorStore)
            })
            .collect()
    }
    async fn count_points(&self, name: &str, filter: Option<&Value>) -> Result<u64> {
        let mut body = json!({"exact":true});
        if let Some(filter) = filter {
            body["filter"] = filter.clone()
        }
        self.call(Method::POST, name, &["points", "count"], Some(&body), false)
            .await?["count"]
            .as_u64()
            .ok_or(QueryError::VectorStore)
    }
    async fn invalidate_receipt(&self, name: &str, metadata: &Value, reason: &str) -> Result<()> {
        if metadata["knowledge_lexical"].is_null() {
            return Ok(());
        }
        let mut metadata = metadata.clone();
        metadata["knowledge_bm25_v2_backfill"] = json!({"status":"invalidated","reason":reason});
        let result = self
            .call(
                Method::PATCH,
                name,
                &[],
                Some(&json!({"metadata":metadata})),
                false,
            )
            .await?;
        if result == Value::Bool(true) {
            Ok(())
        } else {
            Err(QueryError::VectorStore)
        }
    }
    fn deletion_filter(dataset: &Dataset, document: Option<&str>) -> Result<Value> {
        if dataset.tenant_id.trim().is_empty() || dataset.dataset_id.trim().is_empty() {
            return Err(QueryError::Validation(
                "tenant_id and dataset_id are required for deletion".into(),
            ));
        }
        let mut must = vec![
            json!({"key":"dataset_id","match":{"value":dataset.dataset_id}}),
            json!({"should":[{"key":"tenant_id","match":{"value":dataset.tenant_id}},{"is_empty":{"key":"tenant_id"}}]}),
        ];
        if super::serving_profile::require(dataset)?.is_some_and(|p| p.shared.is_some()) {
            must[1] = json!({"key":"tenant_id","match":{"value":dataset.tenant_id}});
        }
        if let Some(document) = document {
            if document.trim().is_empty() {
                return Err(QueryError::Validation(
                    "document_id is required for deletion".into(),
                ));
            }
            must.push(json!({"key":"document_id","match":{"value":document}}))
        }
        Ok(json!({"must":must}))
    }
    /// Caller must hold the ingestion dataset-exclusive lease and persisted fence.
    pub async fn delete_document_points(&self, dataset: &Dataset, document_id: &str) -> Result<()> {
        let filter = Self::deletion_filter(dataset, Some(document_id))?;
        let owned = self.owned_mutation_collections(dataset, &filter).await?;
        for (name, metadata) in owned {
            self.invalidate_receipt(&name, &metadata, "runtime_document_delete")
                .await?;
            let result = self
                .call(
                    Method::POST,
                    &name,
                    &["points", "delete"],
                    Some(&json!({"filter":filter})),
                    true,
                )
                .await?;
            Self::completed(&result)?;
        }
        Ok(())
    }
    /// Complete ownership is proven before deleting a collection. Mixed legacy
    /// collections lose only exact authorized dataset points.
    pub async fn delete_dataset_collections(&self, dataset: &Dataset) -> Result<()> {
        let names = dataset.collection_name.iter().cloned().collect::<Vec<_>>();
        self.delete_dataset_collections_with_names(dataset, &names)
            .await
    }
    pub async fn delete_dataset_collections_with_names(
        &self,
        dataset: &Dataset,
        authoritative_names: &[String],
    ) -> Result<()> {
        let filter = Self::deletion_filter(dataset, None)?;
        let mut actions = Vec::new();
        for name in self.list_collection_names().await? {
            let info = self.collection_info(&name).await?;
            let metadata = info["config"]["metadata"].clone();
            let authoritative = authoritative_names.contains(&name);
            if let Some(scope) = metadata.get("knowledge_scope") {
                if scope["schema_version"] == 2 {
                    if self.owns_scope(dataset, scope)? {
                        actions.push((name, false, metadata));
                    } else if authoritative {
                        return Err(QueryError::CollectionAuthority(
                            "shared collection scope differs".into(),
                        ));
                    }
                    continue;
                }

                if scope["schema_version"] != 1
                    || scope["tenant_id"].as_str().is_none_or(|s| s.is_empty())
                    || scope["dataset_id"].as_str().is_none_or(|s| s.is_empty())
                {
                    return Err(QueryError::CollectionAuthority(
                        "malformed immutable collection scope".into(),
                    ));
                }
                if scope["tenant_id"] == dataset.tenant_id
                    && scope["dataset_id"] == dataset.dataset_id
                {
                    actions.push((name, true, metadata))
                } else if authoritative {
                    return Err(QueryError::CollectionAuthority(
                        "authoritative collection scope mismatch".into(),
                    ));
                }
                continue;
            }
            let total = self.count_points(&name, None).await?;
            let matching = self.count_points(&name, Some(&filter)).await?;
            if total == 0 {
                if authoritative {
                    actions.push((name, true, metadata))
                }
            } else if matching == total {
                actions.push((name, true, metadata))
            } else if matching > 0 {
                actions.push((name, false, metadata))
            } else if authoritative {
                return Err(QueryError::CollectionAuthority(
                    "authoritative collection contains no matching dataset points".into(),
                ));
            }
        }
        for (name, whole, metadata) in actions {
            if whole {
                if !self.collection_exists(&name).await? {
                    continue;
                }
                let r = self.call(Method::DELETE, &name, &[], None, false).await?;
                if r != Value::Bool(true) {
                    return Err(QueryError::VectorStore);
                }
            } else {
                self.invalidate_receipt(&name, &metadata, "runtime_dataset_delete")
                    .await?;
                let r = self
                    .call(
                        Method::POST,
                        &name,
                        &["points", "delete"],
                        Some(&json!({"filter":filter})),
                        true,
                    )
                    .await?;
                Self::completed(&r)?
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "qdrant_error_tests.rs"]
mod qdrant_error_class_tests;
impl QdrantStore {
    pub async fn document_point_ids_by_collection(
        &self,
        dataset: &Dataset,
        document_id: &str,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let filter = Self::deletion_filter(dataset, Some(document_id))?;
        let mut result = BTreeMap::new();
        for name in self.list_collection_names().await? {
            let info = self.collection_info(&name).await?;
            if let Some(scope) = info["config"]["metadata"].get("knowledge_scope") {
                if scope["schema_version"] == 2 {
                    if !self.owns_scope(dataset, scope)? {
                        continue;
                    }
                } else {
                    if scope["schema_version"] != 1 {
                        return Err(QueryError::CollectionAuthority(
                            "malformed immutable collection scope".into(),
                        ));
                    }
                    if scope["tenant_id"] != dataset.tenant_id
                        || scope["dataset_id"] != dataset.dataset_id
                    {
                        continue;
                    }
                }
            } else if self.count_points(&name, Some(&filter)).await? == 0 {
                continue;
            }
            let mut d = dataset.clone();
            d.collection_name = Some(name.clone());
            let points = self.scroll(&d, &filter, false).await?;
            let ids = points
                .iter()
                .map(|p| id_string(&p["id"]).ok_or(QueryError::VectorStore))
                .collect::<Result<Vec<_>>>()?;
            result.insert(name, ids);
        }
        Ok(result)
    }
    pub(super) async fn scroll_all(&self, dataset: &Dataset) -> Result<Vec<Value>> {
        self.scroll(dataset, &json!({}), false).await
    }
}
impl QdrantStore {
    pub async fn publish_lexical_receipt(&self, dataset: &Dataset, receipt: &Value) -> Result<()> {
        if receipt["status"] != "complete"
            || receipt["collection_name"].as_str() != Some(dataset.collection()?)
            || receipt["dataset_id"] != dataset.dataset_id
            || receipt["tenant_id"] != dataset.tenant_id
        {
            return Err(QueryError::CollectionAuthority(
                "invalid lexical receipt scope".into(),
            ));
        }
        self.require_collection_readable(dataset).await?;
        let result = self
            .call(
                Method::PATCH,
                dataset.collection()?,
                &[],
                Some(&json!({"metadata":{"knowledge_bm25_v2_backfill":receipt}})),
                false,
            )
            .await?;
        if result != true
            || self.collection_info(dataset.collection()?).await?["config"]["metadata"]["knowledge_bm25_v2_backfill"]
                != *receipt
        {
            return Err(QueryError::VectorStore);
        }
        Ok(())
    }
    pub async fn invalidate_lexical_receipt(&self, dataset: &Dataset, reason: &str) -> Result<()> {
        let info = self.collection_info(dataset.collection()?).await?;
        self.require_write_scope(dataset, &info)?;
        self.invalidate_receipt(dataset.collection()?, &info["config"]["metadata"], reason)
            .await
    }
    pub(super) async fn select_lexical_profile(
        &self,
        dataset: &Dataset,
        profile: &super::lexical_v2::LexicalProfile,
    ) -> Result<()> {
        self.ensure_lexical_selection(dataset, profile, true, true, true)
            .await
            .map(|_| ())
    }
}
impl QdrantStore {
    pub async fn search_hierarchy(
        &self,
        dataset: &Dataset,
        vector: &[f64],
        top_k: usize,
        documents: &[String],
        threshold: Option<f64>,
    ) -> Result<Vec<VectorHit>> {
        let info = self.require_collection_readable(dataset).await?;
        let collection_dimension = Self::collection_vector_dimension(&info);
        let mut filter = Self::filter(dataset, None, true);
        if !documents.is_empty() {
            filter["must"]
                .as_array_mut()
                .ok_or(QueryError::VectorStore)?
                .push(json!({"key":"document_id","match":{"any":documents}}))
        }
        let mut body = json!({"query":vector,"limit":top_k,"filter":filter,"with_payload":true});
        if let Some(t) = threshold {
            body["score_threshold"] = json!(t)
        }
        self.query(dataset, &body, collection_dimension).await
    }
}
impl QdrantStore {
    /// One provider batch for all rewritten queries. Each retains its candidate
    /// windows and filter; the serving pipeline globally ranks the union once.
    pub async fn hybrid_search_batch(
        &self,
        d: &Dataset,
        specs: &[(String, Vec<f64>, RetrieveRequest, usize, usize)],
        top_k: usize,
        rrf_k: usize,
        weights: Option<(f64, f64)>,
    ) -> Result<Vec<Vec<VectorHit>>> {
        if specs.is_empty() {
            return Ok(vec![]);
        }
        self.require_collection_readable(d).await?;
        let profile = super::lexical_v2::LexicalProfile::from_dataset(d)?;
        if profile.active {
            self.verify_bm25_v2_readiness(d, &profile).await?;
        }
        let mut searches = Vec::new();
        for (text, vector, request, dense_k, sparse_k) in specs {
            let (sparse, using) = if profile.active {
                (profile.document(text), "bm25_v2")
            } else {
                let (i, v) = lexical::sparse_vector(text, true);
                (json!({"indices":i,"values":v}), "bm25")
            };
            let filter = Self::filter(d, Some(request), true);
            let mut rrf = json!({"k":rrf_k});
            if let Some((dense, sparse)) = weights {
                rrf["weights"] = json!([dense, sparse])
            }
            searches.push(json!({"prefetch":[{"query":vector,"limit":dense_k,"filter":filter},{"query":sparse,"using":using,"limit":sparse_k,"filter":filter}],"query":{"rrf":rrf},"limit":top_k,"with_payload":true}));
        }
        if let Some(profile) = super::serving_profile::require(d)? {
            if let Some(shard) = profile.shard_key() {
                for q in &mut searches {
                    q["shard_key"] = shard.clone();
                }
            }
            let params = profile.search_params(d);
            if !params.is_null() {
                for query in &mut searches {
                    query["params"] = params.clone();
                    if let Some(prefetch) = query["prefetch"].as_array_mut() {
                        for q in prefetch {
                            q["params"] = params.clone();
                        }
                    }
                }
            }
        }
        let result = self
            .call_interactive(
                Method::POST,
                d.collection()?,
                &["points", "query", "batch"],
                Some(&json!({"searches":searches})),
                false,
            )
            .await?;
        let rows = result
            .as_array()
            .filter(|a| a.len() == specs.len())
            .ok_or(QueryError::VectorStore)?;
        rows.iter().map(|r| Self::hits(&r["points"])).collect()
    }
}
impl QdrantStore {
    async fn ensure_immutable_scope(&self, d: &Dataset, info: Value) -> Result<Value> {
        let metadata = &info["config"]["metadata"];
        if metadata.get("knowledge_scope").is_some() {
            self.require_write_scope(d, &info)?;
            return Ok(info);
        }
        let name = d.collection()?;
        let expected =
            json!({"schema_version":1,"dataset_id":d.dataset_id,"tenant_id":d.tenant_id});
        let filter = Self::filter(d, None, false);
        let total = self.count_points(name, None).await?;
        let matching = self.count_points(name, Some(&filter)).await?;
        if total == 0 || total != matching {
            return Err(QueryError::CollectionAuthority(
                "empty or mixed legacy collection cannot be safely adopted".into(),
            ));
        }
        let adopting = json!({"schema_version":0,"status":"adopting","dataset_id":d.dataset_id,"tenant_id":d.tenant_id});
        let updated = self
            .call(
                Method::PATCH,
                name,
                &[],
                Some(&json!({"metadata":{"knowledge_scope":adopting}})),
                false,
            )
            .await?;
        if updated != true
            || self.collection_info(name).await?["config"]["metadata"]["knowledge_scope"]
                != adopting
        {
            return Err(QueryError::CollectionAuthority(
                "collection adoption marker did not converge".into(),
            ));
        }
        let total = self.count_points(name, None).await?;
        let matching = self.count_points(name, Some(&filter)).await?;
        if total != matching {
            let _=self.call(Method::PATCH,name,&[],Some(&json!({"metadata":{"knowledge_scope":{"schema_version":0,"adoption_failed":true}}})),false).await;
            return Err(QueryError::CollectionAuthority(
                "collection changed during ownership adoption".into(),
            ));
        }
        let updated = self
            .call(
                Method::PATCH,
                name,
                &[],
                Some(&json!({"metadata":{"knowledge_scope":expected}})),
                false,
            )
            .await?;
        let info = self.collection_info(name).await?;
        if updated != true || info["config"]["metadata"]["knowledge_scope"] != expected {
            return Err(QueryError::CollectionAuthority(
                "collection adoption finalization did not converge".into(),
            ));
        }
        Ok(info)
    }
}
impl QdrantStore {
    /// Discover every immutable-owned or matching legacy collection before any
    /// mutation. A malformed ownership/profile declaration fails the whole plan.
    async fn owned_mutation_collections(
        &self,
        dataset: &Dataset,
        filter: &Value,
    ) -> Result<Vec<(String, Value)>> {
        let mut owned = Vec::new();
        for name in self.list_collection_names().await? {
            let info = self.collection_info(&name).await?;
            let metadata = info["config"]["metadata"].clone();
            if let Some(scope) = metadata.get("knowledge_scope") {
                if scope["schema_version"] == 2 {
                    if !self.owns_scope(dataset, scope)? {
                        continue;
                    }
                } else {
                    if scope["schema_version"] != 1
                        || scope["tenant_id"].as_str().is_none_or(|s| s.is_empty())
                        || scope["dataset_id"].as_str().is_none_or(|s| s.is_empty())
                    {
                        return Err(QueryError::CollectionAuthority(
                            "malformed immutable collection scope".into(),
                        ));
                    }
                    if scope["tenant_id"] != dataset.tenant_id
                        || scope["dataset_id"] != dataset.dataset_id
                    {
                        continue;
                    }
                }
            } else if self.count_points(&name, Some(filter)).await? == 0 {
                continue;
            }
            if let Some(profile) = metadata.get("knowledge_lexical") {
                super::lexical_v2::validate_collection_profile(profile)?;
            }
            owned.push((name, metadata));
        }
        Ok(owned)
    }
    fn segment_mutation_filter(
        dataset: &Dataset,
        document_id: &str,
        segment_id: &str,
    ) -> Result<Value> {
        if segment_id.trim().is_empty() {
            return Err(QueryError::Validation("segment_id is required".into()));
        }
        let mut filter = Self::deletion_filter(dataset, Some(document_id))?;
        filter["must"].as_array_mut().ok_or(QueryError::VectorStore)?.push(json!({"should":[{"key":"segment_id","match":{"value":segment_id.trim()}},{"has_id":[segment_id.trim()]}]}));
        Ok(filter)
    }
    /// Caller holds the dataset lifecycle lease and persisted deletion fence.
    pub async fn delete_segment_points(
        &self,
        dataset: &Dataset,
        document_id: &str,
        segment_id: &str,
    ) -> Result<()> {
        let filter = Self::segment_mutation_filter(dataset, document_id, segment_id)?;
        for (name, metadata) in self.owned_mutation_collections(dataset, &filter).await? {
            self.invalidate_receipt(&name, &metadata, "runtime_segment_delete")
                .await?;
            let result = self
                .call(
                    Method::POST,
                    &name,
                    &["points", "delete"],
                    Some(&json!({"filter":filter})),
                    true,
                )
                .await?;
            Self::completed(&result)?;
        }
        Ok(())
    }
    /// Caller holds the dataset lifecycle lease; PostgreSQL is final authority.
    pub async fn set_segment_payload_enabled(
        &self,
        dataset: &Dataset,
        document_id: &str,
        segment_id: &str,
        enabled: bool,
    ) -> Result<()> {
        let filter = Self::segment_mutation_filter(dataset, document_id, segment_id)?;
        self.set_scoped_payload_enabled(
            dataset,
            &filter,
            enabled,
            "runtime_segment_visibility_update",
        )
        .await
    }
    pub async fn set_document_payload_enabled(
        &self,
        dataset: &Dataset,
        document_id: &str,
        enabled: bool,
    ) -> Result<()> {
        let filter = Self::deletion_filter(dataset, Some(document_id))?;
        self.set_scoped_payload_enabled(
            dataset,
            &filter,
            enabled,
            "runtime_document_visibility_update",
        )
        .await
    }
    async fn set_scoped_payload_enabled(
        &self,
        dataset: &Dataset,
        filter: &Value,
        enabled: bool,
        reason: &str,
    ) -> Result<()> {
        for (name, metadata) in self.owned_mutation_collections(dataset, filter).await? {
            self.invalidate_receipt(&name, &metadata, reason).await?;
            let result = self
                .call(
                    Method::POST,
                    &name,
                    &["points", "payload"],
                    Some(&json!({"filter":filter,"payload":{"enabled":enabled}})),
                    true,
                )
                .await?;
            Self::completed(&result)?;
        }
        Ok(())
    }
}
impl QdrantStore {
    async fn ensure_lexical_selection(
        &self,
        dataset: &Dataset,
        requested: &super::lexical_v2::LexicalProfile,
        configured: bool,
        allow_transition: bool,
        active_authorized: bool,
    ) -> Result<bool> {
        use super::lexical_v2::{LexicalProfile, validate_collection_profile};
        let name = dataset.collection()?;
        let mut info = self.collection_info(name).await?;
        let raw = info["config"]["metadata"].get("knowledge_lexical");
        let mut stored = if let Some(raw) = raw {
            validate_collection_profile(raw)?;
            Some(LexicalProfile::from_collection(raw)?)
        } else {
            None
        };
        let sparse = info["config"]["params"]["sparse_vectors"].clone();
        let has_v2 = sparse.get("bm25_v2").is_some();
        if stored.is_some() && !has_v2 {
            return Err(QueryError::CollectionAuthority(
                "bm25_v2 metadata is missing its sparse field".into(),
            ));
        }
        let explicit_remove = !configured && allow_transition && stored.is_some();
        let mut selected = stored.clone().unwrap_or_else(|| requested.clone());
        let mut add_field = false;
        if has_v2 {
            if sparse["bm25_v2"]["modifier"] != "idf" {
                return Err(QueryError::CollectionAuthority(
                    "bm25_v2 sparse field requires IDF".into(),
                ));
            }
            if stored.is_none() && configured {
                if !allow_transition {
                    return Err(QueryError::CollectionAuthority(
                        "bm25_v2 schema adoption requires an administrator transition".into(),
                    ));
                }
                if self.count_points(name, None).await? > 0 {
                    return Err(QueryError::CollectionAuthority(
                        "refusing to mix unknown unversioned bm25_v2 encodings".into(),
                    ));
                }
                if requested.active {
                    return Err(QueryError::CollectionAuthority(
                        "bm25_v2 active mode requires a completed shadow phase".into(),
                    ));
                }
                selected = requested.clone();
                selected.runtime_revision = 1;
                stored = Some(selected.clone());
            }
            if let Some(old) = &stored {
                if explicit_remove {
                    selected = old.clone();
                    selected.active = false;
                    selected.shadow = false;
                    selected.runtime_revision += 1;
                } else if !configured {
                    selected = old.clone();
                } else {
                    if requested.schema_fingerprint() != old.schema_fingerprint() {
                        return Err(QueryError::CollectionAuthority("bm25_v2 encoding is immutable; use a new collection for changed parameters".into()));
                    }
                    let changed = requested.active != old.active
                        || requested.shadow != old.shadow
                        || requested.filtering_fingerprint() != old.filtering_fingerprint();
                    selected = if changed && allow_transition {
                        let mut next = requested.clone();
                        next.runtime_revision = old.runtime_revision + 1;
                        next
                    } else {
                        old.clone()
                    };
                }
            }
        } else if configured && requested.shadow {
            if !allow_transition {
                return Err(QueryError::CollectionAuthority(
                    "enabling bm25_v2 shadow writes requires an administrator transition".into(),
                ));
            }
            if requested.active {
                return Err(QueryError::CollectionAuthority(
                    "bm25_v2 active mode requires a completed shadow phase".into(),
                ));
            }
            add_field = true;
            selected = requested.clone();
            selected.runtime_revision = 1;
        }
        if !has_v2 && !add_field {
            return Ok(false);
        }
        if selected.active && !active_authorized {
            return Err(QueryError::CollectionAuthority(
                "bm25_v2 active cutover is lifecycle-only".into(),
            ));
        }
        if !self.bm25_v2_enabled {
            let emergency =
                allow_transition && stored.is_some() && !selected.active && !selected.shadow;
            if !emergency
                && (selected.active
                    || (allow_transition && configured && (requested.shadow || requested.active)))
            {
                return Err(QueryError::Validation(
                    "bm25_v2 is disabled by the service kill switch".into(),
                ));
            }
            if !emergency {
                return Ok(false);
            }
        }
        if info["config"]["metadata"].get("knowledge_scope").is_none() && allow_transition {
            info = self.ensure_immutable_scope(dataset, info).await?;
        }
        self.require_write_scope(dataset, &info)?;
        if selected.shadow || selected.active {
            if sparse["bm25"]["modifier"] != "idf" {
                return Err(QueryError::CollectionAuthority(
                    "bm25_v2 must retain lexical_v1 with IDF".into(),
                ));
            }
            self.ensure_bm25_v2_capability(&selected).await?;
            self.ensure_filtering_profile(name, &selected, true, None, false)
                .await?;
        }
        let expected = selected.collection_profile();
        let changed = info["config"]["metadata"]["knowledge_lexical"] != expected;
        if stored.as_ref().is_some_and(|old| old.active) && !selected.active {
            self.ensure_filtering_profile(name, &selected, true, Some(false), true)
                .await?;
        }
        if add_field || changed {
            let mut body = json!({"metadata":{"knowledge_lexical":expected}});
            if add_field {
                body["sparse_vectors"] = json!({"bm25_v2":{"modifier":"idf"}})
            }
            if self
                .call(Method::PATCH, name, &[], Some(&body), false)
                .await?
                != true
            {
                return Err(QueryError::VectorStore);
            }
        }
        if selected.shadow || stored.is_some() {
            self.ensure_filtering_profile(name, &selected, true, Some(false), true)
                .await?;
        }
        Ok(add_field || changed)
    }
    fn payload_index_ready(info: &Value, field: &str) -> bool {
        let schema = &info["payload_schema"][field];
        let kind = schema["data_type"].as_str().or_else(|| schema.as_str());
        let expected = match field {
            "level" => "integer",
            "enabled" => "bool",
            _ => "keyword",
        };
        kind == Some(expected) && (field != "tenant_id" || schema["params"]["is_tenant"] == true)
    }
    fn strict_filtering_ready(info: &Value) -> bool {
        let strict = &info["config"]["strict_mode_config"];
        strict["enabled"] == true
            && strict["unindexed_filtering_retrieve"] == false
            && strict["unindexed_filtering_update"] == false
    }
    pub(super) async fn ensure_filtering_profile(
        &self,
        name: &str,
        profile: &super::lexical_v2::LexicalProfile,
        allow_mutation: bool,
        enforce_strict: Option<bool>,
        manage_strict: bool,
    ) -> Result<Value> {
        let mut info = self.collection_info(name).await?;
        let strict = profile.filtering["strict_unindexed_filtering"] == true;
        let fields: &[&str] = if strict {
            &[
                "tenant_id",
                "dataset_id",
                "content_type",
                "level",
                "enabled",
                "document_id",
                "source_type",
                "language",
                "_lexical.bm25_v2_schema_fingerprint",
                "_lexical.filtering_profile_fingerprint",
            ]
        } else {
            &["tenant_id", "dataset_id"]
        };
        let missing: Vec<_> = fields
            .iter()
            .copied()
            .filter(|field| !Self::payload_index_ready(&info, field))
            .collect();
        if !missing.is_empty() && !allow_mutation {
            return Err(QueryError::CollectionAuthority(
                "bm25_v2 filtering indexes are unavailable".into(),
            ));
        }
        for field in &missing {
            let schema = match *field {
                "tenant_id" => json!({"type":"keyword","is_tenant":true}),
                "level" => json!("integer"),
                "enabled" => json!("bool"),
                _ => json!("keyword"),
            };
            self.call(
                Method::PUT,
                name,
                &["index"],
                Some(&json!({"field_name":field,"field_schema":schema})),
                true,
            )
            .await?;
        }
        if !missing.is_empty() {
            info = self.collection_info(name).await?;
            if fields.iter().any(|f| !Self::payload_index_ready(&info, f)) {
                return Err(QueryError::CollectionAuthority(
                    "bm25_v2 filtering indexes did not converge".into(),
                ));
            }
        }
        if !manage_strict {
            return Ok(info);
        }
        let required = enforce_strict.unwrap_or(profile.active) && strict;
        let disabled = info["config"]["strict_mode_config"].is_null()
            || info["config"]["strict_mode_config"]["enabled"] == false;
        if (required && !Self::strict_filtering_ready(&info)) || (!required && !disabled) {
            if !allow_mutation {
                return Err(QueryError::CollectionAuthority(
                    "strict filtering mode differs from lexical profile".into(),
                ));
            }
            let config = if required {
                json!({"enabled":true,"unindexed_filtering_retrieve":false,"unindexed_filtering_update":false})
            } else {
                json!({"enabled":false})
            };
            if self
                .call(
                    Method::PATCH,
                    name,
                    &[],
                    Some(&json!({"strict_mode_config":config})),
                    false,
                )
                .await?
                != true
            {
                return Err(QueryError::VectorStore);
            }
            info = self.collection_info(name).await?;
            if (required && !Self::strict_filtering_ready(&info))
                || (!required && info["config"]["strict_mode_config"]["enabled"] != false)
            {
                return Err(QueryError::CollectionAuthority(
                    "strict filtering mode did not converge".into(),
                ));
            }
        }
        Ok(info)
    }
    /// Runtime protocol probe from the canonical adapter, never an assumed
    /// capability receipt. No provider call occurs until this method is invoked.
    pub(super) async fn ensure_bm25_v2_capability(
        &self,
        profile: &super::lexical_v2::LexicalProfile,
    ) -> Result<()> {
        if !self.bm25_v2_enabled {
            return Err(QueryError::Validation(
                "bm25_v2 is disabled by the service kill switch".into(),
            ));
        }
        let key = profile.schema_fingerprint();
        let mut receipts = self.capability_receipts.lock().await;
        if receipts
            .get(&key)
            .is_some_and(|until| *until > Instant::now())
        {
            return Ok(());
        }
        let name = format!("kb_bm25_v2_canary_{}", uuid::Uuid::new_v4().simple());
        let repeated = uuid::Uuid::new_v4().to_string();
        let diluted = uuid::Uuid::new_v4().to_string();
        let unrelated = uuid::Uuid::new_v4().to_string();
        let mut probe = self.clone();
        probe.timeout = probe.timeout.min(Duration::from_secs(15));
        let mut created = false;
        let operation:Result<()>=async{
            if probe.call(Method::PUT,&name,&[],Some(&json!({"vectors":{},"sparse_vectors":{"bm25_v2":{"modifier":"idf"}}})),false).await?!=true{return Err(QueryError::VectorStore)}created=true;
            let points=json!([{"id":repeated,"vector":{"bm25_v2":profile.document("alpha alpha alpha")}}, {"id":diluted,"vector":{"bm25_v2":profile.document("alpha filler filler filler filler filler")}}, {"id":unrelated,"vector":{"bm25_v2":profile.document("beta gamma")}}]);
            probe.call(Method::PUT,&name,&["points"],Some(&json!({"points":points})),true).await?;
            let result=probe.call(Method::POST,&name,&["points","query"],Some(&json!({"query":profile.document("alpha"),"using":"bm25_v2","limit":2,"with_payload":false})),false).await?;
            let hits=result["points"].as_array().ok_or(QueryError::VectorStore)?;
            if hits.len()<2||id_string(&hits[0]["id"]).as_deref()!=Some(repeated.as_str())||hits[0]["score"].as_f64().unwrap_or(0.0)<=0.0||hits[0]["score"].as_f64().unwrap_or(0.0)<=hits[1]["score"].as_f64().unwrap_or(0.0)||hits.iter().filter_map(|h|id_string(&h["id"])).collect::<BTreeSet<_>>()!=BTreeSet::from([repeated.clone(),diluted.clone()]){return Err(QueryError::CollectionAuthority("native BM25 capability probe returned unexpected TF/length ranking".into()))}Ok(())
        }.await;
        let cleanup = if created {
            probe
                .call(Method::DELETE, &name, &[], None, false)
                .await
                .and_then(|v| {
                    if v == true {
                        Ok(())
                    } else {
                        Err(QueryError::VectorStore)
                    }
                })
        } else {
            Ok(())
        };
        if operation.is_err() && cleanup.is_err() {
            return Err(QueryError::CollectionAuthority(
                "native BM25 capability probe and cleanup failed".into(),
            ));
        }
        operation?;
        cleanup?;
        receipts.insert(key, Instant::now() + self.bm25_v2_capability_ttl);
        Ok(())
    }
}

#[cfg(test)]
#[path = "qdrant_role_tls_tests.rs"]
mod role_tls_tests;
