//! Canonical request/response/service domain records from `src/models/`.
//!
//! Enums are copied from ai_gateway_core/enums/_core.py, not invented API names.
//! Content/auth primitives are shared with request_validation. Domain serialization
//! is distinct from public output projection: to_wire_value() base64-encodes bytes
//! exactly as api/schemas/response.py. No authenticated identity is inferred here.
//! Registry config coercion lives in service_registry::service_from_value.
pub use crate::request_validation::{ContentData, ContentType, ServiceAuthConfig};
use crate::{
    dispatcher::{DispatchPolicy, DispatchService, effective_method},
    dispatcher_rate_limiter::{RateLimit, ServiceRateLimits},
    request_validation::{self, ValidationRequest, ValidationService},
};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, time::Duration};
macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Hash)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name { pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } } }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                // Python's string-valued enums reject externally tagged maps.
                let value = String::deserialize(deserializer)?;
                match value.as_str() {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(serde::de::Error::unknown_variant(&value, &[$($wire),+])),
                }
            }
        }
    };
}
wire_enum!(ServiceType {
    Conversational => "conversational",
    Generative => "generative",
    Processing => "processing",
    Embedding => "embedding",
    Classification => "classification",
    Langgraph => "langgraph",
    Custom => "custom"
});
wire_enum!(InvocationMode {
    Sync => "sync",
    Async => "async",
    Stream => "stream",
    Webhook => "webhook"
});
wire_enum!(TransportType {
    Http => "http",
    Grpc => "grpc",
    Websocket => "websocket",
    MessageQueue => "mq",
    InProcess => "in_process"
});
wire_enum!(StreamEventType {
    TextDelta => "text_delta",
    ToolCallStart => "tool_call_start",
    ToolCallDelta => "tool_call_delta",
    ToolCallEnd => "tool_call_end",
    ToolResult => "tool_result",
    Thinking => "thinking",
    Final => "final",
    StreamEnd => "stream_end",
    RunStarted => "run_started",
    RunFinished => "run_finished",
    RunError => "run_error",
    StepStarted => "step_started",
    StepFinished => "step_finished",
    TextMessageStart => "text_message_start",
    TextMessageContent => "text_message_content",
    TextMessageEnd => "text_message_end",
    ToolCallArgs => "tool_call_args",
    ToolCallResult => "tool_call_result",
    StateSnapshot => "state_snapshot",
    StateDelta => "state_delta",
    MessagesSnapshot => "messages_snapshot",
    RawEvent => "raw_event",
    CustomEvent => "custom_event",
    ArtifactCreated => "artifact_created",
    FileCreating => "file_creating",
    FileCreated => "file_created",
    SearchStarted => "search_started",
    SearchProgress => "search_progress",
    SearchCompleted => "search_completed",
    OutlineReady => "outline_ready",
    DocumentGenerationStart => "document_generation_start",
    DocumentGenerationResult => "document_generation_result",
    CodeExecutionStart => "code_execution_start",
    CodeExecutionResult => "code_execution_result",
    ImageGenerationStart => "image_generation_start",
    ImageGenerationResult => "image_generation_result",
    ToolError => "tool_error",
    ThinkingStart => "thinking_start",
    ThinkingDelta => "thinking_delta",
    ThinkingEnd => "thinking_end",
    ThinkingError => "thinking_error",
    Status => "status",
    PhaseStarted => "phase_started",
    PhaseCompleted => "phase_completed",
    PhaseProgress => "phase_progress",
    Error => "error",
    BudgetWarning => "budget_warning",
    BudgetExceeded => "budget_exceeded",
    Cancelled => "cancelled",
    ContextRetrieved => "context_retrieved",
    WebSearchResults => "web_search_results",
    RagEvaluation => "rag_evaluation",
    ContextBudget => "context_budget",
    ContextCompacted => "context_compacted",
    ContextDetail => "context_detail",
    MemoryRetrieved => "memory_retrieved",
    MemoryReflectionScheduled => "memory_reflection_scheduled",
    QueueState => "queue_state",
    QueueSteered => "queue_steered",
    ApprovalRequired => "approval_required",
    ApprovalResult => "approval_result",
    GatewayDecision => "gateway_decision",
    SandboxDecision => "sandbox_decision",
    SkillSelected => "skill_selected",
    SkillLoaded => "skill_loaded",
    SkillCreatePendingApproval => "skill_create_pending_approval",
    MemoryLoaded => "memory_loaded",
    Done => "done"
});
/// Compatibility alias retained by released SDK/plugins.
pub type ConnectorType = TransportType;
// The wire macro also defines enums without a canonical default.
#[allow(clippy::derivable_impls)]
impl Default for ServiceType {
    fn default() -> Self {
        Self::Custom
    }
}
// The wire macro also defines enums without a canonical default.
#[allow(clippy::derivable_impls)]
impl Default for TransportType {
    fn default() -> Self {
        Self::Http
    }
}
// The wire macro also defines enums without a canonical default.
#[allow(clippy::derivable_impls)]
impl Default for StreamEventType {
    fn default() -> Self {
        Self::TextDelta
    }
}
fn default_modes() -> Vec<InvocationMode> {
    vec![InvocationMode::Sync]
}
fn empty_object() -> Value {
    json!({})
}
/// Python datetime can be naive UTC (the default) or offset-aware when supplied
/// by clients / task envelopes. Neither the offset nor awareness is discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolTimestamp {
    Naive(NaiveDateTime),
    Offset(DateTime<FixedOffset>),
}
impl ProtocolTimestamp {
    pub fn now() -> Self {
        Utc::now().naive_utc().into()
    }
    pub fn parse_isoformat(value: &str) -> Result<Self, &'static str> {
        if let Ok(value) = DateTime::parse_from_rfc3339(value) {
            return Ok(value.into());
        }
        for format in [
            "%Y-%m-%dT%H:%M:%S%.f%::z",
            "%Y-%m-%d %H:%M:%S%.f%::z",
            "%Y-%m-%dT%H:%M:%S%.f%z",
            "%Y-%m-%d %H:%M:%S%.f%z",
        ] {
            if let Ok(value) = DateTime::parse_from_str(value, format) {
                return Ok(value.into());
            }
        }
        for format in [
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M",
            "%Y-%m-%d %H:%M",
            "%Y-%m-%dT%H",
        ] {
            if let Ok(value) = NaiveDateTime::parse_from_str(value, format) {
                return Ok(value.into());
            }
        }
        if let Ok(value) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            return Ok(value.and_hms_opt(0, 0, 0).expect("midnight").into());
        }
        Err("invalid_protocol_timestamp")
    }
    pub fn to_isoformat(self) -> String {
        let (local, offset) = match self {
            Self::Naive(value) => (value, None),
            Self::Offset(value) => (value.naive_local(), Some(value.offset().local_minus_utc())),
        };
        let mut text = if local.nanosecond() == 0 {
            local.format("%Y-%m-%dT%H:%M:%S").to_string()
        } else {
            local.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
        };
        if let Some(offset) = offset {
            let absolute = offset.unsigned_abs();
            text.push_str(&format!(
                "{}{:02}:{:02}",
                if offset < 0 { '-' } else { '+' },
                absolute / 3600,
                absolute % 3600 / 60
            ));
            if absolute % 60 != 0 {
                text.push_str(&format!(":{:02}", absolute % 60));
            }
        }
        text
    }
    pub fn naive_utc(self) -> NaiveDateTime {
        match self {
            Self::Naive(value) => value,
            Self::Offset(value) => value.naive_utc(),
        }
    }
}
impl From<NaiveDateTime> for ProtocolTimestamp {
    fn from(value: NaiveDateTime) -> Self {
        Self::Naive(
            value
                .with_nanosecond(value.nanosecond() / 1000 * 1000)
                .expect("microsecond timestamp"),
        )
    }
}
impl From<DateTime<FixedOffset>> for ProtocolTimestamp {
    fn from(value: DateTime<FixedOffset>) -> Self {
        Self::Offset(
            value
                .with_nanosecond(value.nanosecond() / 1000 * 1000)
                .expect("microsecond timestamp"),
        )
    }
}
impl Serialize for ProtocolTimestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_isoformat())
    }
}
impl<'de> Deserialize<'de> for ProtocolTimestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse_isoformat(&value).map_err(serde::de::Error::custom)
    }
}
fn now() -> ProtocolTimestamp {
    ProtocolTimestamp::now()
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub content_type: ContentType,
    #[serde(default)]
    pub data: Option<ContentData>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Map<String, Value>>,
}
impl ContentItem {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content_type: ContentType::Text,
            data: Some(ContentData::Text(text.into())),
            url: None,
            mime_type: None,
            metadata: None,
        }
    }
    pub fn validation_view(&self) -> request_validation::ContentItem {
        request_validation::ContentItem {
            content_type: self.content_type,
            data: self.data.clone(),
            url: self.url.clone(),
        }
    }
    pub fn to_wire_value(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("content item is JSON serializable");
        if let Some(ContentData::Bytes(bytes)) = &self.data {
            value["data"] = base64_bytes(bytes).into();
        }
        value
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UnifiedRequest {
    pub request_id: String,
    pub service_id: String,
    pub inputs: Vec<ContentItem>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub context: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub parameters: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub callback_url: Option<String>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default = "now")]
    pub timestamp: ProtocolTimestamp,
}
impl UnifiedRequest {
    pub fn new(
        request_id: impl Into<String>,
        service_id: impl Into<String>,
        inputs: Vec<ContentItem>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            service_id: service_id.into(),
            inputs,
            session_id: None,
            context: None,
            parameters: None,
            callback_url: None,
            priority: 0,
            user_id: String::new(),
            tenant_id: String::new(),
            timestamp: now(),
        }
    }
    pub fn validation_view(&self) -> ValidationRequest {
        ValidationRequest {
            service_id: self.service_id.clone(),
            inputs: self
                .inputs
                .iter()
                .map(ContentItem::validation_view)
                .collect(),
            parameters: self
                .parameters
                .clone()
                .map(Value::Object)
                .unwrap_or(Value::Null),
            user_id: Some(self.user_id.clone()),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UnifiedResponse {
    pub request_id: String,
    pub status: String,
    pub outputs: Vec<ContentItem>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub usage: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub error: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub metadata: Option<serde_json::Map<String, Value>>,
    #[serde(default = "now")]
    pub timestamp: ProtocolTimestamp,
}
impl UnifiedResponse {
    pub fn new(
        request_id: impl Into<String>,
        status: impl Into<String>,
        outputs: Vec<ContentItem>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            status: status.into(),
            outputs,
            session_id: None,
            task_id: None,
            usage: None,
            error: None,
            metadata: None,
            timestamp: now(),
        }
    }
    pub fn to_wire_value(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("response is JSON serializable");
        value["outputs"] = self
            .outputs
            .iter()
            .map(ContentItem::to_wire_value)
            .collect();
        value
    }
}
fn pending() -> String {
    "pending".into()
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: String,
    #[serde(default = "pending")]
    pub status: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StreamChunk {
    pub request_id: String,
    pub chunk_index: i64,
    pub content: ContentItem,
    #[serde(default)]
    pub is_final: bool,
    #[serde(default)]
    pub event_type: StreamEventType,
    #[serde(default)]
    pub tool_call: Option<ToolCall>,
    #[serde(default)]
    pub metadata: Option<serde_json::Map<String, Value>>,
}
impl StreamChunk {
    pub fn to_wire_value(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("chunk is JSON serializable");
        value["content"] = self.content.to_wire_value();
        value
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceRateLimitConfig {
    pub enabled: bool,
    pub requests: i64,
    pub window: i64,
    pub burst: i64,
    pub strategy: String,
}
impl Default for ServiceRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            requests: 100,
            window: 60,
            burst: 0,
            strategy: "sliding_window".into(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceCacheConfig {
    pub enabled: bool,
    pub ttl: i64,
    pub semantic_cache: bool,
}
impl Default for ServiceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl: 300,
            semantic_cache: false,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServicePriorityConfig {
    pub priority: i64,
    pub weight: i64,
    pub max_queue_size: i64,
}
impl Default for ServicePriorityConfig {
    fn default() -> Self {
        Self {
            priority: 5,
            weight: 1,
            max_queue_size: 100,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceCapacityConfig {
    pub upstream_group: Option<String>,
    pub concurrency_limit: Option<i64>,
    pub queue_max: i64,
    pub queue_timeout_ms: i64,
    pub enforced: bool,
}
impl Default for ServiceCapacityConfig {
    fn default() -> Self {
        Self {
            upstream_group: None,
            concurrency_limit: None,
            queue_max: 16,
            queue_timeout_ms: 3000,
            enforced: true,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceConfig {
    pub rate_limit: ServiceRateLimitConfig,
    pub auth: ServiceAuthConfig,
    pub cache: ServiceCacheConfig,
    pub priority: ServicePriorityConfig,
    pub capacity: ServiceCapacityConfig,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ServiceDefinition {
    pub service_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub service_type: ServiceType,
    pub supported_modes: Vec<InvocationMode>,
    pub connector_type: TransportType,
    pub connector_config: Value,
    pub input_schema: Value,
    pub output_schema: Value,
    pub accepted_content_types: Vec<ContentType>,
    pub output_content_types: Vec<ContentType>,
    pub session_enabled: bool,
    pub session_adapter: Option<String>,
    pub timeout: i64,
    pub max_retries: i64,
    pub retry_delay: f64,
    pub circuit_breaker_enabled: bool,
    pub failure_threshold: i64,
    pub recovery_timeout: i64,
    pub rate_limit: Option<Value>,
    pub concurrency_limit: Option<i64>,
    pub status: String,
    pub tags: Vec<String>,
    pub metadata: Value,
    pub async_config: Option<Value>,
    pub service_config: Option<ServiceConfig>,
}
impl fmt::Debug for ServiceDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceDefinition")
            .field("service_id", &self.service_id)
            .field("service_type", &self.service_type)
            .finish_non_exhaustive()
    }
}
impl Default for ServiceDefinition {
    fn default() -> Self {
        Self {
            service_id: String::new(),
            name: String::new(),
            description: String::new(),
            version: "1.0.0".into(),
            service_type: ServiceType::Custom,
            supported_modes: default_modes(),
            connector_type: TransportType::Http,
            connector_config: empty_object(),
            input_schema: empty_object(),
            output_schema: empty_object(),
            accepted_content_types: Vec::new(),
            output_content_types: Vec::new(),
            session_enabled: false,
            session_adapter: None,
            timeout: 60,
            max_retries: 3,
            retry_delay: 1.0,
            circuit_breaker_enabled: true,
            failure_threshold: 5,
            recovery_timeout: 30,
            rate_limit: None,
            concurrency_limit: None,
            status: "active".into(),
            tags: Vec::new(),
            metadata: empty_object(),
            async_config: None,
            service_config: None,
        }
    }
}
impl ServiceDefinition {
    pub fn new(service_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            name: name.into(),
            ..Self::default()
        }
    }
    /// Matches the dataclass's lazy materialization of default service config.
    pub fn get_service_config(&mut self) -> &mut ServiceConfig {
        self.service_config.get_or_insert_default()
    }
    pub fn validation_view(&self) -> ValidationService {
        ValidationService {
            service_id: self.service_id.clone(),
            accepted_content_types: self.accepted_content_types.clone(),
            input_schema: self.input_schema.clone(),
            auth: self
                .service_config
                .as_ref()
                .map(|config| config.auth.clone())
                .unwrap_or_default(),
        }
    }
    pub fn rate_limits_view(&self) -> Result<ServiceRateLimits, &'static str> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyLimit {
            requests: i64,
            window: i64,
            #[serde(default)]
            burst: i64,
            #[serde(default = "sliding_window")]
            strategy: String,
        }
        fn sliding_window() -> String {
            "sliding_window".into()
        }
        let configured = self.service_config.as_ref().and_then(|config| {
            config.rate_limit.enabled.then(|| RateLimit {
                requests: config.rate_limit.requests,
                window: config.rate_limit.window,
                burst: config.rate_limit.burst,
                strategy: config.rate_limit.strategy.clone(),
            })
        });
        let legacy = if configured.is_none() {
            self.rate_limit
                .as_ref()
                .filter(|value| crate::service_registry::python_truthy(value))
                .map(|value| {
                    serde_json::from_value::<LegacyLimit>(value.clone()).map(|config| RateLimit {
                        requests: config.requests,
                        window: config.window,
                        burst: config.burst,
                        strategy: config.strategy,
                    })
                })
                .transpose()
                .map_err(|_| "invalid_legacy_rate_limit")?
        } else {
            None
        };
        Ok(ServiceRateLimits {
            service_id: self.service_id.clone(),
            configured,
            legacy,
        })
    }
    pub fn dispatch_view(&self) -> Result<DispatchService, &'static str> {
        let connector = self
            .connector_config
            .as_object()
            .ok_or("invalid_connector_config")?;
        let mapping = connector
            .get("request_mapping")
            .filter(|value| crate::service_registry::python_truthy(value));
        let mapping = mapping
            .map(|value| value.as_object().ok_or("invalid_request_mapping"))
            .transpose()?;
        let method = |value: Option<&Value>| -> Result<Option<String>, &'static str> {
            match value.filter(|value| crate::service_registry::python_truthy(value)) {
                None => Ok(None),
                Some(Value::String(value)) => Ok(Some(value.clone())),
                Some(Value::Bool(value)) => Ok(Some(if *value { "True" } else { "False" }.into())),
                Some(Value::Number(value)) => Ok(Some(value.to_string())),
                _ => Err("invalid_request_method"),
            }
        };
        let mapping_method = method(mapping.and_then(|mapping| mapping.get("method")))?;
        let connector_method = if mapping_method.is_none() {
            method(connector.get("method"))?
        } else {
            None
        };
        let policy = DispatchPolicy::new(
            &effective_method(mapping_method.as_deref(), connector_method.as_deref()),
            self.max_retries,
            Duration::from_secs(self.timeout.max(0) as u64),
        );
        Ok(DispatchService {
            policy,
            ..self.stream_dispatch_view()
        })
    }
    /// Streaming never evaluates invocation method/retry/timeout configuration.
    /// A custom streaming adapter may legitimately ignore request_mapping.
    pub fn stream_dispatch_view(&self) -> DispatchService {
        DispatchService {
            service_id: self.service_id.clone(),
            policy: DispatchPolicy::default(),
            circuit_breaker_enabled: self.circuit_breaker_enabled,
            failure_threshold: self.failure_threshold,
            recovery_timeout: Duration::from_secs(self.recovery_timeout.max(0) as u64),
            capacity_config: json!({"service_config": self.service_config, "metadata": self.metadata, "concurrency_limit": self.concurrency_limit, "rate_limit": self.rate_limit}),
        }
    }
}
fn base64_bytes(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[((bits >> 18) & 63) as usize] as char);
        encoded.push(ALPHABET[((bits >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}
