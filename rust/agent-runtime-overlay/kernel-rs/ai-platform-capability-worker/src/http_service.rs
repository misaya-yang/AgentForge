use std::collections::BTreeMap;
use std::sync::Arc;

use ai_platform_capability_contract::{
    CapabilityDescriptorV2, CapabilityExecutionV2, CapabilityScopeV2,
};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify, Semaphore};
use uuid::Uuid;

use crate::{
    DynStore, MAX_EVENT_PAYLOAD_BYTES, MAX_REQUEST_BYTES, RuntimeCapabilityBinding, StoreError,
    attachment_capabilities::AttachmentCapabilityBroker,
    external_write_capabilities::ExternalWriteExecutor, local_node_broker::LocalNodeBroker,
    office_capabilities::OfficeCapabilityExecutor,
    platform_catalog::worker_capability_catalog_with_writes,
    python_artifact_broker::PythonArtifactStore, python_code_execution::LocalPythonSandboxBroker,
    quiz_capabilities::QuizPersistenceAdapter, read_capabilities::ReadCapabilityExecutor,
    write_capabilities::WriteCapabilityExecutor,
};

mod catalog;
mod events;
mod execution;
mod execution_run;
mod recovery;

use self::catalog::catalog;
use self::events::get_events;
use self::execution::cancel_execution;
use self::execution::create_execution;
use self::execution::get_execution;
use self::recovery::recover_executions_loop;

const INTERNAL_TOKEN_HEADER: &str = "x-ai-platform-internal-token";
const TENANT_HEADER: &str = "x-ai-tenant-id";
const USER_HEADER: &str = "x-ai-user-id";
const SESSION_HEADER: &str = "x-ai-session-id";

#[derive(Clone)]
pub struct WorkerState {
    pub store: DynStore,
    pub internal_token: Arc<str>,
    pub lease_secret: Arc<[u8]>,
    pub execution_slots: Arc<Semaphore>,
    pub read_executor: Option<Arc<ReadCapabilityExecutor>>,
    pub write_executor: Option<Arc<WriteCapabilityExecutor>>,
    pub quiz_executor: Option<Arc<QuizPersistenceAdapter>>,
    pub external_write_executor: Option<Arc<ExternalWriteExecutor>>,
    pub office_executor: Option<Arc<OfficeCapabilityExecutor>>,
    pub python_executor: Option<Arc<LocalPythonSandboxBroker>>,
    pub python_artifact_store: Option<Arc<dyn PythonArtifactStore>>,
    pub attachment_executor: Option<Arc<AttachmentCapabilityBroker>>,
    pub local_node_broker: Option<Arc<LocalNodeBroker>>,
    /// Immutable descriptor registry.  It is constructed once at startup and
    /// shared by catalog, create, descriptor policy, and timeout paths.
    pub capability_registry:
        Arc<BTreeMap<String, ai_platform_capability_contract::CapabilityDescriptorV2>>,
    /// Exact, immutable deployment generation. Never selected by a request.
    pub plugin_activation: Option<Arc<crate::plugin_packs::ActivePacks>>,
    cancellation: Arc<Mutex<BTreeMap<String, Arc<Notify>>>>,
}

impl WorkerState {
    pub fn try_new(
        store: DynStore,
        internal_token: String,
        lease_secret: Vec<u8>,
        fixtures_enabled: bool,
    ) -> Result<Self, crate::platform_catalog::PlatformCatalogError> {
        Self::try_new_with_writes(store, internal_token, lease_secret, fixtures_enabled, false)
    }

    pub fn try_new_with_writes(
        store: DynStore,
        internal_token: String,
        lease_secret: Vec<u8>,
        fixtures_enabled: bool,
        writes_enabled: bool,
    ) -> Result<Self, crate::platform_catalog::PlatformCatalogError> {
        Ok(Self {
            store,
            internal_token: Arc::from(internal_token),
            lease_secret: Arc::from(lease_secret),
            execution_slots: Arc::new(Semaphore::new(1)),
            read_executor: None,
            write_executor: None,
            quiz_executor: None,
            external_write_executor: None,
            office_executor: None,
            python_executor: None,
            python_artifact_store: None,
            attachment_executor: None,
            local_node_broker: None,
            capability_registry: Arc::new({
                let mut registry = worker_capability_catalog_with_writes(writes_enabled)?;
                if fixtures_enabled {
                    for descriptor in crate::fixture_catalog() {
                        if registry.insert(descriptor.id.clone(), descriptor).is_some() {
                            return Err(crate::platform_catalog::PlatformCatalogError::Duplicate(
                                "projected id",
                                "fixture collision".to_string(),
                            ));
                        }
                    }
                }
                registry
            }),
            plugin_activation: None,
            cancellation: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn with_read_executor(mut self, executor: Arc<ReadCapabilityExecutor>) -> Self {
        self.read_executor = Some(executor);
        self
    }

    pub fn with_write_executor(mut self, executor: Arc<WriteCapabilityExecutor>) -> Self {
        self.write_executor = Some(executor);
        self
    }

    pub fn with_quiz_executor(mut self, executor: Arc<QuizPersistenceAdapter>) -> Self {
        self.quiz_executor = Some(executor);
        self
    }

    pub fn with_external_write_executor(mut self, executor: Arc<ExternalWriteExecutor>) -> Self {
        self.external_write_executor = Some(executor);
        self
    }

    pub fn with_office_executor(mut self, executor: Arc<OfficeCapabilityExecutor>) -> Self {
        self.office_executor = Some(executor);
        self
    }

    pub fn with_python_executor(mut self, executor: Arc<LocalPythonSandboxBroker>) -> Self {
        self.python_executor = Some(executor);
        self
    }

    pub fn with_python_artifact_store(mut self, store: Arc<dyn PythonArtifactStore>) -> Self {
        self.python_artifact_store = Some(store);
        self
    }

    pub fn with_attachment_executor(mut self, executor: Arc<AttachmentCapabilityBroker>) -> Self {
        self.attachment_executor = Some(executor);
        self
    }

    pub fn with_local_node_broker(mut self, broker: Arc<LocalNodeBroker>) -> Self {
        self.local_node_broker = Some(broker);
        self
    }

    /// Install a verified deployment snapshot after all executors are wired.
    /// A manifest cannot turn an absent/disabled implementation into readiness.
    pub fn with_plugin_activation(
        mut self,
        active: crate::plugin_packs::ActivePacks,
    ) -> Result<Self, crate::plugin_packs::PackError> {
        for id in active
            .registry
            .keys()
            .filter(|id| active.managed.contains(*id))
        {
            if !self.plugin_executor_configured(id) {
                return Err(crate::plugin_packs::PackError::Incompatible);
            }
        }
        self.capability_registry = Arc::new(active.registry.clone());
        self.plugin_activation = Some(Arc::new(active));
        Ok(self)
    }

    fn plugin_executor_configured(&self, id: &str) -> bool {
        match id {
            "confluence_read"
            | "search_knowledge_base"
            | "web_fetch"
            | "search_web"
            | "read_tool_artifact"
            | "todo_read" => self.read_executor.is_some(),
            "read_attachment" => self.attachment_executor.is_some(),
            "todo_write" | "update_user_memory" => self.write_executor.is_some(),
            "generate_quiz" => self.quiz_executor.is_some(),
            "confluence_write" | "generate_image" => self.external_write_executor.is_some(),
            "mcp_docgen__generate_document" => self.office_executor.is_some(),
            "execute_python_code" => {
                self.python_executor.is_some() && self.python_artifact_store.is_some()
            }
            // The current default LocalNode transport is explicitly unavailable;
            // a future real transport must supply an authoritative readiness seam.
            _ => false,
        }
    }

    /// Dynamic governed MCP keeps its existing authority path. Pack-tagged or
    /// managed IDs must match this exact activation, even after deactivation.
    fn admits_plugin_descriptor(&self, descriptor: &CapabilityDescriptorV2) -> bool {
        let tagged = descriptor.tags.iter().any(|tag| tag.starts_with("plugin-"));
        match &self.plugin_activation {
            Some(active) if tagged || active.managed.contains(&descriptor.id) => {
                self.capability_registry.get(&descriptor.id) == Some(descriptor)
            }
            _ => !tagged,
        }
    }

    async fn register_cancellation(&self, execution_id: &str) -> (Arc<Notify>, bool) {
        let mut cancellation = self.cancellation.lock().await;
        if let Some(token) = cancellation.get(execution_id) {
            return (token.clone(), false);
        }
        let token = Arc::new(Notify::new());
        cancellation.insert(execution_id.to_string(), token.clone());
        (token, true)
    }

    async fn request_cancel(&self, execution_id: &str) {
        let cancellation = self.cancellation.lock().await.get(execution_id).cloned();
        if let Some(cancellation) = cancellation {
            cancellation.notify_one();
        }
    }

    async fn forget_cancellation(&self, execution_id: &str) {
        self.cancellation.lock().await.remove(execution_id);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    #[serde(default)]
    after_sequence: u64,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: &'static str,
    message: &'static str,
}

type HttpError = (StatusCode, Json<ErrorBody>);

fn error(status: StatusCode, code: &'static str, message: &'static str) -> HttpError {
    (
        status,
        Json(ErrorBody {
            error: code,
            message,
        }),
    )
}

fn store_error(value: StoreError) -> HttpError {
    match value {
        StoreError::NotFound => error(StatusCode::NOT_FOUND, "not_found", "execution not found"),
        StoreError::ScopeMismatch => {
            error(StatusCode::NOT_FOUND, "not_found", "execution not found")
        }
        StoreError::IdempotencyConflict => error(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "execution identity conflicts with an existing request",
        ),
        StoreError::DispatchFenceMismatch => error(
            StatusCode::CONFLICT,
            "dispatch_fence_conflict",
            "execution already has another dispatch fence",
        ),
        StoreError::ApprovalRequired => error(
            StatusCode::FORBIDDEN,
            "approval_required",
            "a bound one-time approval is required",
        ),
        StoreError::TerminalImmutable => error(
            StatusCode::CONFLICT,
            "terminal_immutable",
            "terminal execution cannot be mutated",
        ),
        StoreError::InvalidEvent => error(
            StatusCode::BAD_REQUEST,
            "event_invalid",
            "execution event is invalid",
        ),
        StoreError::RuntimeBindingUnauthorized => error(
            StatusCode::FORBIDDEN,
            "runtime_binding_unauthorized",
            "runtime capability binding is not active",
        ),
        StoreError::Internal => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "store_unavailable",
            "capability execution store is unavailable",
        ),
    }
}

fn header(headers: &HeaderMap, name: &'static str) -> Result<String, HttpError> {
    let value = headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| error(StatusCode::FORBIDDEN, "scope_invalid", "scope is missing"))?;
    if value.len() > 255 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "scope_invalid",
            "scope is invalid",
        ));
    }
    Ok(value.to_string())
}

fn authorize(headers: &HeaderMap, state: &WorkerState) -> Result<CapabilityScopeV2, HttpError> {
    let provided = headers
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if provided.len() != state.internal_token.len()
        || !constant_time_eq::constant_time_eq(provided.as_bytes(), state.internal_token.as_bytes())
    {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "internal authorization failed",
        ));
    }
    Ok(CapabilityScopeV2 {
        tenant_id: header(headers, TENANT_HEADER)?,
        user_id: header(headers, USER_HEADER)?,
        session_id: header(headers, SESSION_HEADER)?,
    })
}

fn scope_matches(scope: &CapabilityScopeV2, expected: &CapabilityScopeV2) -> bool {
    scope == expected
}

fn resource_binding_value(
    binding: &RuntimeCapabilityBinding,
    scope: &CapabilityScopeV2,
    run_id: &str,
    descriptor: &CapabilityDescriptorV2,
) -> Value {
    serde_json::json!({
        "tenant_id": scope.tenant_id,
        "user_id": scope.user_id,
        "session_id": scope.session_id,
        "run_id": run_id,
        "snapshot_id": binding.snapshot_id,
        "capability_revision": binding.capability_revision,
        "type": binding.capability_type,
        "name": binding.name,
        "capability_id": binding.capability_id,
        "capability_version": binding.capability_version,
        "schema_hash": binding.schema_hash,
        "bound_dataset_ids": binding.bound_dataset_ids.iter().cloned().collect::<Vec<_>>(),
        "connector_binding": binding.connector_binding.clone(),
        "device_id": binding.connector_binding.as_ref().and_then(|value| value.connection_id.clone()),
        "memory_policy": binding.memory_policy.clone(),
        "descriptor": descriptor,
    })
}

fn descriptor_type(descriptor: &CapabilityDescriptorV2) -> Result<&'static str, HttpError> {
    if descriptor
        .tags
        .iter()
        .any(|tag| tag == "kind:connector" || tag == "binding-type:connector")
    {
        Ok("connector")
    } else if descriptor.tags.iter().any(|tag| tag == "kind:knowledge") {
        Ok("knowledge")
    } else if descriptor.tags.iter().any(|tag| tag == "kind:mcp") {
        Ok("mcp")
    } else if descriptor.tags.iter().any(|tag| tag == "kind:tool")
        || descriptor.tags.iter().any(|tag| tag == "fixture")
    {
        Ok("tool")
    } else {
        Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "capability_descriptor_invalid",
            "capability descriptor kind is invalid",
        ))
    }
}

fn verify_descriptor_against_runtime_binding(
    descriptor: &CapabilityDescriptorV2,
    binding: &RuntimeCapabilityBinding,
    requested_connector_binding: Option<&Value>,
) -> Result<(), StoreError> {
    if binding.capability_id != descriptor.id
        || binding.capability_version != descriptor.version
        || binding.schema_hash != descriptor.schema_hash
        || binding.effect != descriptor.effect
        || binding.approval_policy != descriptor.approval_policy
    {
        return Err(StoreError::RuntimeBindingUnauthorized);
    }
    let stored_connector_binding = binding
        .connector_binding
        .as_ref()
        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null));
    if stored_connector_binding.as_ref() != requested_connector_binding
        || descriptor.connector_binding.as_ref() != requested_connector_binding
    {
        return Err(StoreError::RuntimeBindingUnauthorized);
    }
    Ok(())
}

pub fn router(state: WorkerState) -> Router {
    tokio::spawn(recover_executions_loop(state.clone()));
    Router::new()
        .route("/health/ready", get(health))
        .route("/internal/v2/capabilities/catalog", post(catalog))
        .route(
            "/internal/v2/capabilities/executions",
            post(create_execution),
        )
        .route(
            "/internal/v2/capabilities/executions/{execution_id}",
            get(get_execution).post(cancel_execution),
        )
        .route(
            "/internal/v2/capabilities/executions/{execution_id}/events",
            get(get_events),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state)
}

#[derive(Serialize)]
struct WorkerHealthResponse {
    status: &'static str,
    core_ready: bool,
    degraded: bool,
    core: BTreeMap<&'static str, &'static str>,
    capabilities: BTreeMap<&'static str, &'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_activation: Option<crate::plugin_packs::ActivationReceipt>,
}

async fn health(State(state): State<WorkerState>) -> impl IntoResponse {
    let store_ready = state.store.ready().await.is_ok();
    let capabilities = worker_capability_health(&state);
    let degraded = capabilities.values().any(|value| *value == "unavailable");
    let response = WorkerHealthResponse {
        status: if store_ready { "ready" } else { "not_ready" },
        core_ready: store_ready,
        degraded,
        core: BTreeMap::from([(
            "execution_store",
            if store_ready {
                "healthy"
            } else {
                "unavailable"
            },
        )]),
        capabilities,
        plugin_activation: state
            .plugin_activation
            .as_ref()
            .map(|active| active.receipt.clone()),
    };
    (
        if store_ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(response),
    )
}

fn worker_capability_health(state: &WorkerState) -> BTreeMap<&'static str, &'static str> {
    fn advertised(state: &WorkerState, ids: &[&str], configured: bool) -> &'static str {
        if !ids
            .iter()
            .any(|capability_id| state.capability_registry.contains_key(*capability_id))
        {
            "not_configured"
        } else if configured {
            "healthy"
        } else {
            "unavailable"
        }
    }

    BTreeMap::from([
        ("catalog", "healthy"),
        (
            "read",
            advertised(
                state,
                &[
                    "search_knowledge_base",
                    "confluence_read",
                    "read_tool_artifact",
                    "todo_read",
                    "web_fetch",
                    "search_web",
                ],
                state.read_executor.is_some(),
            ),
        ),
        (
            "write",
            advertised(
                state,
                &["todo_write", "update_user_memory"],
                state.write_executor.is_some(),
            ),
        ),
        (
            "external_write",
            advertised(
                state,
                &["confluence_write", "generate_image"],
                state.external_write_executor.is_some(),
            ),
        ),
        (
            "quiz",
            advertised(state, &["generate_quiz"], state.quiz_executor.is_some()),
        ),
        (
            "office",
            advertised(
                state,
                &[
                    "mcp_docgen__generate_document",
                    "mcp_docgen__modify_document",
                    "mcp_docgen__preview_document",
                ],
                state.office_executor.is_some(),
            ),
        ),
        (
            "python",
            advertised(
                state,
                &["execute_python_code"],
                state.python_executor.is_some(),
            ),
        ),
        (
            "attachment",
            advertised(
                state,
                &["read_attachment"],
                state.attachment_executor.is_some(),
            ),
        ),
        (
            "local_node",
            advertised(
                state,
                &[
                    "local_node_catalog",
                    "local_node_describe",
                    "local_node_action",
                ],
                state.local_node_broker.is_some(),
            ),
        ),
    ])
}

fn validate_execution_id(value: &str) -> Result<(), HttpError> {
    Uuid::parse_str(value).map(|_| ()).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "execution_id_invalid",
            "execution id is invalid",
        )
    })
}

fn event_id(execution_id: &str, ordinal: u8) -> Result<String, HttpError> {
    validate_execution_id(execution_id)?;
    Ok(event_id_unchecked(execution_id, ordinal))
}

fn event_id_unchecked(execution_id: &str, ordinal: u8) -> String {
    let mut bytes = *Uuid::parse_str(execution_id)
        .expect("validated execution id")
        .as_bytes();
    bytes[15] ^= ordinal;
    Uuid::from_bytes(bytes).to_string()
}

pub fn payload_within_limit(payload: &BTreeMap<String, Value>) -> bool {
    serde_json::to_vec(payload).is_ok_and(|encoded| encoded.len() <= MAX_EVENT_PAYLOAD_BYTES)
}

trait ExecutionScope {
    fn scope(&self) -> CapabilityScopeV2;
}

impl ExecutionScope for CapabilityExecutionV2 {
    fn scope(&self) -> CapabilityScopeV2 {
        CapabilityScopeV2 {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;
    use ai_platform_capability_contract::CapabilityEffect;
    use ai_platform_capability_contract::canonical_json_hash;

    #[test]
    fn deterministic_event_ids_are_distinct() {
        let execution_id = "00000000-0000-0000-0000-000000000001";
        assert_ne!(
            event_id_unchecked(execution_id, 1),
            event_id_unchecked(execution_id, 2)
        );
    }

    #[test]
    fn event_payload_limit_is_bounded() {
        let mut payload = BTreeMap::new();
        payload.insert("value".to_string(), Value::String("x".repeat(1024)));
        assert!(payload_within_limit(&payload));
        payload.insert(
            "large".to_string(),
            Value::String("x".repeat(MAX_EVENT_PAYLOAD_BYTES)),
        );
        assert!(!payload_within_limit(&payload));
    }

    fn state(fixtures_enabled: bool) -> WorkerState {
        WorkerState::try_new(
            Arc::new(MemoryStore::default()),
            "internal-token".to_string(),
            b"lease-signing-secret-that-is-long-enough".to_vec(),
            fixtures_enabled,
        )
        .expect("checked-in catalog must load")
    }

    #[tokio::test]
    async fn arc05_readiness_uses_store_and_reports_optional_capabilities() {
        let state = state(false);
        let response = health(State(state.clone())).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.store.ready().await.is_ok());
        let capabilities = worker_capability_health(&state);
        assert_eq!(capabilities["catalog"], "healthy");
        assert_eq!(capabilities["read"], "unavailable");
        assert_eq!(capabilities["write"], "not_configured");
    }

    #[test]
    fn default_registry_hides_fixtures_and_unimplemented_capabilities() {
        let state = state(false);
        for capability_id in [
            "confluence_read",
            "read_tool_artifact",
            "search_knowledge_base",
            "todo_read",
            "web_fetch",
            "read_attachment",
        ] {
            assert!(
                state.capability_registry.contains_key(capability_id),
                "{capability_id}"
            );
        }
        assert!(
            !state
                .capability_registry
                .contains_key("execute_python_code")
        );
        assert!(!state.capability_registry.contains_key("local_node_action"));
    }

    #[test]
    fn fixture_flag_adds_only_the_two_contract_fixtures() {
        let state = state(true);
        assert!(state.capability_registry.contains_key("platform.echo"));
        assert!(
            state
                .capability_registry
                .contains_key("platform.read_fixture")
        );
        assert!(state.capability_registry.len() >= 10);
    }

    #[test]
    fn write_flag_exposes_only_implemented_writers() {
        let state = WorkerState::try_new_with_writes(
            Arc::new(MemoryStore::default()),
            "internal-token".to_string(),
            b"lease-signing-secret-that-is-long-enough".to_vec(),
            false,
            true,
        )
        .expect("checked-in catalog must load");
        assert!(state.capability_registry.contains_key("todo_write"));
        assert!(state.capability_registry.contains_key("update_user_memory"));
        assert!(state.capability_registry.contains_key("confluence_write"));
        assert!(state.capability_registry.contains_key("generate_image"));
        assert!(state.capability_registry.contains_key("generate_quiz"));
        assert!(
            state
                .capability_registry
                .contains_key("mcp_docgen__generate_document")
        );
        assert!(
            state
                .capability_registry
                .contains_key("mcp_docgen__modify_document")
        );
        assert!(
            state
                .capability_registry
                .contains_key("mcp_docgen__preview_document")
        );
        assert!(
            state
                .capability_registry
                .contains_key("execute_python_code")
        );
        assert!(state.capability_registry.contains_key("local_node_action"));
        assert!(state.capability_registry.contains_key("search_web"));
        assert_eq!(state.capability_registry.len(), 19);
    }

    #[test]
    fn write_enabled_startup_loads_embedded_local_node_connector_contract() {
        let state = WorkerState::try_new_with_writes(
            Arc::new(MemoryStore::default()),
            "internal-token".to_string(),
            b"lease-signing-secret-that-is-long-enough".to_vec(),
            false,
            true,
        )
        .expect("full embedded worker catalog must load at startup");
        let action = &state.capability_registry["local_node_action"];

        assert_eq!(action.protocol, "local-node-capability/v2");
        assert!(action.tags.iter().any(|tag| tag == "connector:local_node"));
        assert!(
            action
                .tags
                .iter()
                .any(|tag| tag == "binding-type:connector")
        );
    }

    #[test]
    fn cloned_state_shares_the_same_immutable_registry() {
        let state = state(false);
        let clone = state.clone();
        assert!(Arc::ptr_eq(
            &state.capability_registry,
            &clone.capability_registry
        ));
    }

    #[test]
    fn declarative_kind_tag_binds_snapshot_capability_type() {
        let state = state(false);
        assert_eq!(
            descriptor_type(&state.capability_registry["search_knowledge_base"])
                .expect("knowledge kind"),
            "knowledge"
        );
        assert_eq!(
            descriptor_type(&state.capability_registry["web_fetch"]).expect("tool kind"),
            "tool"
        );
    }

    fn dynamic_mcp_descriptor() -> CapabilityDescriptorV2 {
        let input_schema = serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
            "additionalProperties": false
        });
        CapabilityDescriptorV2 {
            schema_version: ai_platform_capability_contract::CAPABILITY_DESCRIPTOR_SCHEMA_VERSION
                .to_string(),
            id: "mcp.docs.search".to_string(),
            name: "search_docs".to_string(),
            version: "2026-08-24".to_string(),
            description: "Search the bound docs MCP server".to_string(),
            schema_hash: canonical_json_hash(&input_schema).expect("schema hash"),
            input_schema,
            output_schema: serde_json::json!({"type": "object"}),
            effect: CapabilityEffect::Read,
            approval_policy: ai_platform_capability_contract::ApprovalPolicy::Never,
            execution_mode: ai_platform_capability_contract::ExecutionMode::Inline,
            timeout_ms: 30_000,
            tags: vec!["kind:mcp".to_string()],
            protocol: "mcp".to_string(),
            connector_binding: None,
        }
    }

    fn dynamic_binding(descriptor: &CapabilityDescriptorV2) -> RuntimeCapabilityBinding {
        RuntimeCapabilityBinding {
            snapshot_id: "snapshot-a".to_string(),
            capability_revision: 7,
            capability_type: "mcp".to_string(),
            name: descriptor.name.clone(),
            capability_id: descriptor.id.clone(),
            capability_version: descriptor.version.clone(),
            schema_hash: descriptor.schema_hash.clone(),
            effect: descriptor.effect,
            approval_policy: descriptor.approval_policy,
            bound_dataset_ids: Default::default(),
            connector_binding: None,
            memory_policy: None,
        }
    }

    #[test]
    fn dynamic_mcp_descriptor_is_accepted_without_static_catalog_entry() {
        let descriptor = dynamic_mcp_descriptor();
        let binding = dynamic_binding(&descriptor);
        assert_eq!(descriptor_type(&descriptor).expect("mcp kind"), "mcp");
        verify_descriptor_against_runtime_binding(&descriptor, &binding, None)
            .expect("snapshot-bound dynamic descriptor should dispatch");
    }

    #[test]
    fn dynamic_descriptor_schema_and_connector_tampering_is_rejected() {
        let descriptor = dynamic_mcp_descriptor();
        let binding = dynamic_binding(&descriptor);
        let mut schema_tamper = descriptor.clone();
        schema_tamper.schema_hash =
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string();
        assert_eq!(
            verify_descriptor_against_runtime_binding(&schema_tamper, &binding, None),
            Err(StoreError::RuntimeBindingUnauthorized)
        );
        assert_eq!(
            verify_descriptor_against_runtime_binding(
                &descriptor,
                &binding,
                Some(&serde_json::json!({"binding_type": "grant"})),
            ),
            Err(StoreError::RuntimeBindingUnauthorized)
        );
    }
}
