//! Concrete ports of `src/adapters`: protocol-specific payloads, real I/O and
//! caller-owned cancellation. Dify intentionally inherits generic REST in source.
//! Python dynamic in-process graphs cannot be imported into a native process;
//! those configurations fail construction rather than fabricating a response.
mod comfyui;
mod dify;
mod langgraph;
mod media;
mod native_sdk;
mod sdk_usage_audit;
pub mod sdk_session_cleanup;
mod openai;
mod transport;
use crate::{
    dispatcher::{AdapterFailure, DispatchError, DispatchFuture, DispatchProducer},
    protocol::{
        ContentData, ContentItem, ContentType, ServiceType, StreamChunk, StreamEventType,
        TransportType, UnifiedRequest, UnifiedResponse,
    },
    service_ownership::ServiceHandle,
    service_registry::{
        AdapterError, AdapterFactory, AdapterFuture, ProtocolAdapter, RegistryError,
        ServiceRegistry, python_truthy,
    },
};
pub use langgraph::{
    LangGraphAdapterFactory, LangGraphAdapterOptions, LangGraphPayloadPreparation,
};
pub use media::{safe_fetch_with_policy, validated_destination};
use serde_json::{Map, Value, json};
use std::sync::Arc;
pub use transport::{HttpResponse, HttpTransport, Lines};

pub fn register_adapters(registry: &ServiceRegistry) {
    use crate::generic_rest_adapter::GenericRestAdapterFactory;
    for framework in ["openai_agents", "claude_agent_sdk"] {
        registry.register_adapter(framework, Arc::new(native_sdk::NativeSdkFactory { session_pool:None, capabilities:registry.sdk_capabilities(), read_grants:registry.sdk_read_grants(), framework, encryption_key:registry.sdk_agent_key_handle(), worker_config:None }));
    }
    registry.register_adapter("generic_rest", Arc::new(GenericRestAdapterFactory));
    registry.register_adapter("dify", Arc::new(GenericRestAdapterFactory));
    for kind in [
        Kind::DifyChat,
        Kind::OpenAi,
        Kind::Tts,
        Kind::Whisper,
        Kind::StableDiffusion,
        Kind::ComfyUi,
        Kind::LangGraph,
    ] {
        registry.register_adapter(kind.name(), Arc::new(ConcreteFactory(kind)));
    }
}
/// Native host supplies the canonical database and explicit operator environment;
/// neither SDK service metadata nor ambient process state chooses the worker target.
pub fn register_sdk_session_adapters(
    registry: &ServiceRegistry,
    pool: &sqlx::PgPool,
    worker_config: Option<sdk_session_cleanup::SdkAgentWorkerConfig>,
) {
    for framework in ["openai_agents", "claude_agent_sdk"] {
        registry.register_adapter(framework, Arc::new(native_sdk::NativeSdkFactory {
            framework, capabilities:registry.sdk_capabilities(), session_pool: Some(pool.clone()), encryption_key: registry.sdk_agent_key_handle(),
            read_grants: registry.sdk_read_grants(),
            worker_config: worker_config.clone(),
        }));
    }
}
pub fn register_adapters_with_langgraph(
    registry: &ServiceRegistry,
    options: LangGraphAdapterOptions,
) {
    register_adapters(registry);
    registry.register_adapter("langgraph", Arc::new(LangGraphAdapterFactory { options }));
}
#[derive(Clone, Copy)]
enum Kind {
    DifyChat,
    OpenAi,
    Tts,
    Whisper,
    StableDiffusion,
    ComfyUi,
    LangGraph,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::DifyChat => "dify_chat",
            Self::OpenAi => "openai",
            Self::Tts => "tts",
            Self::Whisper => "whisper",
            Self::StableDiffusion => "stable_diffusion",
            Self::ComfyUi => "comfyui",
            Self::LangGraph => "langgraph",
        }
    }
}
struct ConcreteFactory(Kind);
impl AdapterFactory for ConcreteFactory {
    fn create(&self, service: ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> {
        match self.0 {
            Kind::DifyChat => Ok(Arc::new(dify::DifyChatAdapter {
                http: HttpTransport::new(service)?,
            })),
            Kind::ComfyUi => Ok(Arc::new(comfyui::ComfyUiAdapter::new(service)?)),
            Kind::OpenAi => Ok(Arc::new(openai::OpenAiAdapter {
                http: HttpTransport::new(service.clone())?,
                service,
            })),
            Kind::LangGraph => Ok(Arc::new(langgraph::LangGraphAdapter::new(service)?)),
            kind => Ok(Arc::new(media::MediaAdapter {
                http: HttpTransport::new(service.clone())?,
                service,
                kind,
            })),
        }
    }
}
pub(super) fn validation(message: &'static str) -> AdapterError {
    DispatchError::AdapterValidationFailed(message).into()
}
pub(super) fn dispatch(error: AdapterError) -> DispatchError {
    match error {
        AdapterError::Dispatch(e) => e,
        AdapterError::UnsupportedOperation => DispatchError::AdapterOperationUnsupported,
        AdapterError::BulkheadExhausted { service_id } => {
            DispatchError::AdapterBulkheadExhausted { service_id }
        }
    }
}
pub(super) fn content(kind: ContentType, value: Value) -> ContentItem {
    ContentItem {
        content_type: kind,
        data: match value {
            Value::Null => None,
            Value::String(v) => Some(ContentData::Text(v)),
            v => Some(ContentData::Json(v)),
        },
        url: None,
        mime_type: None,
        metadata: None,
    }
}
pub(super) fn binary(kind: ContentType, bytes: Vec<u8>, mime: &str) -> ContentItem {
    ContentItem {
        content_type: kind,
        data: Some(ContentData::Bytes(bytes)),
        url: None,
        mime_type: Some(mime.into()),
        metadata: None,
    }
}
pub(super) fn data_value(item: &ContentItem) -> Value {
    match &item.data {
        Some(ContentData::Text(v)) => json!(v),
        Some(ContentData::Json(v)) => v.clone(),
        Some(ContentData::Bytes(v)) => json!(v),
        None => Value::Null,
    }
}
/// Python str for the JSON input subset, including None and boolean spelling.
pub(super) fn pystr(v: &Value) -> String {
    match v {
        Value::String(v) => v.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Array(a) => format!("[{}]", a.iter().map(pyrepr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", pyrepr(&json!(k)), pyrepr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        v => v.to_string(),
    }
}
fn pyrepr(v: &Value) -> String {
    match v {
        Value::String(s) => format!(
            "'{}'",
            s.replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        ),
        v => pystr(v),
    }
}
pub(super) fn first_text(
    request: &UnifiedRequest,
    message: &'static str,
) -> Result<String, AdapterError> {
    let item = request
        .inputs
        .iter()
        .find(|i| i.content_type == ContentType::Text)
        .ok_or_else(|| validation(message))?;
    let value = data_value(item);
    Ok(if python_truthy(&value) {
        pystr(&value)
    } else {
        String::new()
    })
}
pub(super) fn chunk(
    request_id: &str,
    index: i64,
    text: Value,
    event: StreamEventType,
    final_chunk: bool,
) -> StreamChunk {
    StreamChunk {
        request_id: request_id.into(),
        chunk_index: index,
        content: content(ContentType::Text, text),
        is_final: final_chunk,
        event_type: event,
        tool_call: None,
        metadata: None,
    }
}
