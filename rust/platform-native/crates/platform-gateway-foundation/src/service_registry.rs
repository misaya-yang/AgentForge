//! Service registry / adapter factory / bulkhead port from
//! `src/services/registry/service_registry.py` and `src/adapters/base.py`.
//!
//! Storage is injected; the memory implementation preserves insertion order.
//! Register saves first, then updates the service cache and invalidates its adapter.
//! List merges storage with cache (cache wins), but a storage exception still
//! propagates: the Python comment does not implement an exception fallback.
//! Capability search reads storage only and does not filter inactive services.
//! Factory replacement does not retroactively evict already cached adapters.
//!
//! Config input is supplied JSON text or already-decoded JSON. YAML decoding,
//! file/directory discovery and connector construction remain external seams.
//! PostgreSQL storage is supplied by registry_storage::PgRegistryStorage.
//! No environment reads or dynamic imports occur here.
//! Caller-supplied ADAPTER_BULKHEAD_LIMITS text retains the source parser behavior.
//! Explicit outer bool/int/float conversions mirror Python. Nested declared typed
//! config/scalar fields reject annotation-violating JSON instead of carrying
//! arbitrary Python objects. Nonfinite floats and out-of-range integers are
//! explicit errors. Live handles preserve service/config/connector object identity.
//!
//! Bulkhead errors retain their service identity and are not classified as a
//! retryable transport failure. Async adapter operations remain on the caller
//! task; drop cancels work and releases its semaphore permit. A stream owns its
//! permit until producer close completes; dropped stream cleanup uses its saved
//! runtime handle. Runtime shutdown can prevent asynchronous producer close.
use crate::{
    dispatcher::{AdapterFailure, DispatchError, DispatchFuture, DispatchProducer},
    protocol::{
        ContentType, ServiceDefinition, ServiceType, StreamChunk, UnifiedRequest, UnifiedResponse,
    },
    service_ownership::ServiceHandle,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    ValidationFailed(&'static str),
    InvalidConfig(&'static str),
    InvalidJson,
    AdapterNotFound(String),
    StorageUnavailable,
    AlreadyExists,
    AtomicInsertUnsupported,
    AdapterConstructionFailed,
}
impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed(message) | Self::InvalidConfig(message) => f.write_str(message),
            Self::InvalidJson => f.write_str("invalid_service_json"),
            Self::AdapterNotFound(adapter) => write!(f, "Adapter not found: {adapter}"),
            Self::AlreadyExists => f.write_str("service_id_conflict"),
            Self::AtomicInsertUnsupported => f.write_str("atomic_service_registration_unsupported"),
            Self::StorageUnavailable => f.write_str("service_registry_storage_unavailable"),
            Self::AdapterConstructionFailed => f.write_str("service_adapter_construction_failed"),
        }
    }
}
impl std::error::Error for RegistryError {}
pub type RegistryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RegistryError>> + Send + 'a>>;

pub fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn number_text(value: &str) -> Result<String, RegistryError> {
    let text = value.trim();
    let bytes = text.as_bytes();
    if bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'_'
            && (index == 0
                || !bytes[index - 1].is_ascii_digit()
                || !bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
    }) {
        return Err(RegistryError::InvalidConfig("invalid_numeric_config"));
    }
    Ok(text.replace('_', ""))
}
fn integer(value: &Value) -> Result<i64, RegistryError> {
    match value {
        Value::Bool(value) => Ok(i64::from(*value)),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                return Ok(value);
            }
            let value = value
                .as_f64()
                .ok_or(RegistryError::InvalidConfig("invalid_integer_config"))?
                .trunc();
            if !value.is_finite() || value < i64::MIN as f64 || value >= -(i64::MIN as f64) {
                return Err(RegistryError::InvalidConfig("integer_config_overflow"));
            }
            Ok(value as i64)
        }
        Value::String(value) => number_text(value)?
            .parse()
            .map_err(|_| RegistryError::InvalidConfig("invalid_integer_config")),
        _ => Err(RegistryError::InvalidConfig("invalid_integer_config")),
    }
}
fn float(value: &Value) -> Result<f64, RegistryError> {
    let value = match value {
        Value::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(value) => value
            .as_f64()
            .ok_or(RegistryError::InvalidConfig("invalid_float_config"))?,
        Value::String(value) => number_text(value)?
            .parse()
            .map_err(|_| RegistryError::InvalidConfig("invalid_float_config"))?,
        _ => return Err(RegistryError::InvalidConfig("invalid_float_config")),
    };
    if !value.is_finite() {
        return Err(RegistryError::InvalidConfig("nonfinite_float_config"));
    }
    Ok(value)
}
/// Exact configuration loader defaults/coercions for supported JSON domain data.
/// Null enum members default individually; invalid non-null enum names fail.
pub fn service_from_value(data: &Value) -> Result<ServiceDefinition, RegistryError> {
    let mut fields = data
        .as_object()
        .cloned()
        .ok_or(RegistryError::InvalidConfig(
            "service_config_must_be_object",
        ))?;
    let service_id = fields
        .get("service_id")
        .cloned()
        .ok_or(RegistryError::ValidationFailed("service_id is required"))?;
    fields.entry("name").or_insert(service_id);
    for (key, fallback) in [("service_type", "custom"), ("connector_type", "http")] {
        if fields.get(key).is_none_or(Value::is_null) {
            fields.insert(key.into(), fallback.into());
        }
    }
    for (key, fallback, default) in [
        ("supported_modes", "sync", json!(["sync"])),
        ("accepted_content_types", "text", json!([])),
        ("output_content_types", "text", json!([])),
    ] {
        let value = fields.entry(key).or_insert(default);
        let items = value.as_array_mut().ok_or(RegistryError::InvalidConfig(
            "service_enum_list_must_be_array",
        ))?;
        for item in items {
            if item.is_null() {
                *item = fallback.into();
            }
        }
    }
    for (key, default) in [
        ("session_enabled", false),
        ("circuit_breaker_enabled", true),
    ] {
        let value = fields.get(key).map(python_truthy).unwrap_or(default);
        fields.insert(key.into(), value.into());
    }
    for (key, default) in [
        ("timeout", 60),
        ("max_retries", 3),
        ("failure_threshold", 5),
        ("recovery_timeout", 30),
    ] {
        let value = fields.get(key).map(integer).transpose()?.unwrap_or(default);
        fields.insert(key.into(), value.into());
    }
    let delay = fields
        .get("retry_delay")
        .map(float)
        .transpose()?
        .unwrap_or(1.0);
    fields.insert("retry_delay".into(), json!(delay));
    for key in [
        "connector_config",
        "input_schema",
        "output_schema",
        "metadata",
    ] {
        if fields.get(key).is_none_or(|value| !python_truthy(value)) {
            fields.insert(key.into(), json!({}));
        }
    }
    for key in ["connector_config", "metadata"] {
        if !fields.get(key).is_some_and(Value::is_object) {
            return Err(RegistryError::InvalidConfig("invalid_service_object_field"));
        }
    }
    if fields.get("tags").is_none_or(|value| !python_truthy(value)) {
        fields.insert("tags".into(), json!([]));
    }
    if fields
        .get("service_config")
        .is_none_or(|value| !python_truthy(value))
    {
        fields.insert("service_config".into(), Value::Null);
    }
    serde_json::from_value(Value::Object(fields))
        .map_err(|_| RegistryError::InvalidConfig("invalid_service_field_type_or_enum"))
}

pub trait RegistryStorage: Send + Sync {
    /// Atomic absence test and insertion. Never implement this as get + save.
    /// Legacy stores fail closed until they explicitly provide this guarantee.
    fn insert_if_absent(&self, _service: ServiceHandle) -> RegistryFuture<'_, bool> {
        Box::pin(async { Err(RegistryError::AtomicInsertUnsupported) })
    }
    fn save(&self, service: ServiceHandle) -> RegistryFuture<'_, ()>;
    fn get<'a>(&'a self, service_id: &'a str) -> RegistryFuture<'a, Option<ServiceHandle>>;
    fn list(&self) -> RegistryFuture<'_, Vec<ServiceHandle>>;
    fn delete<'a>(&'a self, service_id: &'a str) -> RegistryFuture<'a, bool>;
}
#[derive(Default)]
struct OrderedServices {
    order: Vec<String>,
    rows: HashMap<String, ServiceHandle>,
}
impl OrderedServices {
    fn insert(&mut self, service: ServiceHandle) {
        let id = service.service_id();
        if !self.rows.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.rows.insert(id, service);
    }
    fn delete(&mut self, service_id: &str) -> bool {
        let removed = self.rows.remove(service_id).is_some();
        if removed {
            self.order.retain(|id| id != service_id);
        }
        removed
    }
    fn list(&self) -> Vec<ServiceHandle> {
        self.order
            .iter()
            .filter_map(|id| self.rows.get(id).cloned())
            .collect()
    }
}
#[derive(Default)]
pub struct MemoryRegistryStorage {
    services: Mutex<OrderedServices>,
}
impl MemoryRegistryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}
impl RegistryStorage for MemoryRegistryStorage {
    fn insert_if_absent(&self, service: ServiceHandle) -> RegistryFuture<'_, bool> {
        Box::pin(async move {
            let mut rows = self.services.lock().expect("registry memory poisoned");
            if rows.rows.contains_key(&service.service_id()) { return Ok(false); }
            rows.insert(service);
            Ok(true)
        })
    }
    fn save(&self, service: ServiceHandle) -> RegistryFuture<'_, ()> {
        Box::pin(async move {
            self.services
                .lock()
                .expect("registry memory poisoned")
                .insert(service);
            Ok(())
        })
    }
    fn get<'a>(&'a self, service_id: &'a str) -> RegistryFuture<'a, Option<ServiceHandle>> {
        Box::pin(async move {
            Ok(self
                .services
                .lock()
                .expect("registry memory poisoned")
                .rows
                .get(service_id)
                .cloned())
        })
    }
    fn list(&self) -> RegistryFuture<'_, Vec<ServiceHandle>> {
        Box::pin(async move {
            Ok(self
                .services
                .lock()
                .expect("registry memory poisoned")
                .list())
        })
    }
    fn delete<'a>(&'a self, service_id: &'a str) -> RegistryFuture<'a, bool> {
        Box::pin(async move {
            Ok(self
                .services
                .lock()
                .expect("registry memory poisoned")
                .delete(service_id))
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AdapterError {
    Dispatch(DispatchError),
    BulkheadExhausted { service_id: String },
    UnsupportedOperation,
}
impl From<DispatchError> for AdapterError {
    fn from(error: DispatchError) -> Self {
        Self::Dispatch(error)
    }
}
impl From<AdapterFailure> for AdapterError {
    fn from(error: AdapterFailure) -> Self {
        Self::Dispatch(DispatchError::Adapter(error))
    }
}
impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dispatch(error) => error.fmt(f),
            Self::BulkheadExhausted { service_id } => {
                write!(f, "Adapter bulkhead exhausted for {service_id}")
            }
            Self::UnsupportedOperation => f.write_str("adapter_operation_not_implemented"),
        }
    }
}
impl std::error::Error for AdapterError {}
pub type AdapterFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AdapterError>> + Send + 'a>>;
/// Protocol transformations and connector creation belong to the injected adapter.
/// Stream creation must return an owned producer, never an untracked background task.
pub trait ProtocolAdapter: Send + Sync {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse>;
    fn stream(
        &self,
        _request: UnifiedRequest,
    ) -> AdapterFuture<'_, Box<dyn DispatchProducer<StreamChunk>>> {
        Box::pin(async { Err(AdapterError::UnsupportedOperation) })
    }
    fn submit<'a>(&'a self, _request: &'a UnifiedRequest) -> AdapterFuture<'a, String> {
        Box::pin(async { Err(AdapterError::UnsupportedOperation) })
    }
    fn get_result<'a>(&'a self, _task_id: &'a str) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async { Err(AdapterError::UnsupportedOperation) })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool>;
}
pub trait AdapterFactory: Send + Sync {
    /// Synchronous construction only. Factories must not call back into registry.
    fn create(&self, service: ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError>;
}
impl<F> AdapterFactory for F
where
    F: Fn(ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> + Send + Sync,
{
    fn create(&self, service: ServiceHandle) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> {
        self(service)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct BulkheadPolicy {
    pub concurrency_limit: usize,
    pub queue_timeout: Duration,
}
pub fn adapter_bulkhead_limit_from_text(adapter_type: &str, raw: &str) -> Option<i64> {
    for item in raw.trim().split(',') {
        let Some((name, value)) = item.split_once(':') else {
            continue;
        };
        if name.trim() != adapter_type {
            continue;
        }
        let parsed = integer(&Value::String(value.trim().into())).ok()?;
        return (parsed > 0).then_some(parsed);
    }
    None
}
pub fn bulkhead_policy(
    service: &ServiceDefinition,
    adapter_type: &str,
    environment_text: &str,
) -> Result<Option<BulkheadPolicy>, RegistryError> {
    let bulkhead = service.metadata.get("bulkhead").and_then(Value::as_object);
    let limit = bulkhead
        .and_then(|bulkhead| bulkhead.get("concurrency_limit"))
        .filter(|value| !value.is_null());
    let limit = match limit {
        Some(limit) => Some(integer(limit)?),
        None => adapter_bulkhead_limit_from_text(adapter_type, environment_text),
    };
    let Some(limit) = limit else {
        return Ok(None);
    };
    let queue_timeout = bulkhead
        .and_then(|bulkhead| bulkhead.get("queue_timeout_seconds"))
        .map(float)
        .transpose()?
        .unwrap_or(1.0)
        .max(0.001);
    let concurrency_limit = usize::try_from(limit.max(1))
        .map_err(|_| RegistryError::InvalidConfig("bulkhead_limit_overflow"))?;
    if concurrency_limit > Semaphore::MAX_PERMITS {
        return Err(RegistryError::InvalidConfig("bulkhead_limit_overflow"));
    }
    let queue_timeout = Duration::try_from_secs_f64(queue_timeout)
        .map_err(|_| RegistryError::InvalidConfig("bulkhead_timeout_overflow"))?;
    Ok(Some(BulkheadPolicy {
        concurrency_limit,
        queue_timeout,
    }))
}

pub struct ServiceRegistry {
    pub storage: Arc<dyn RegistryStorage>,
    cache: Mutex<OrderedServices>,
    factories: Mutex<HashMap<String, Arc<dyn AdapterFactory>>>,
    adapter_cache: Mutex<HashMap<String, Arc<dyn ProtocolAdapter>>>,
    bulkhead_environment: String,
    sdk_read_grants: Arc<crate::sdk_read_grants::SdkReadGrants>,
    sdk_capabilities: Arc<std::sync::OnceLock<Arc<dyn crate::sdk_capabilities::SdkCapabilityAuthority>>>,
    sdk_agent_key: Arc<std::sync::OnceLock<Arc<str>>>,
}
impl ServiceRegistry {
    pub fn new(storage: Arc<dyn RegistryStorage>) -> Self {
        Self::with_bulkhead_limits(storage, "")
    }
    pub fn with_bulkhead_limits(
        storage: Arc<dyn RegistryStorage>,
        limits: impl Into<String>,
    ) -> Self {
        Self {
            storage,
            cache: Mutex::new(OrderedServices::default()),
            factories: Mutex::new(HashMap::new()),
            adapter_cache: Mutex::new(HashMap::new()),
            bulkhead_environment: limits.into(),
            sdk_read_grants: Arc::new(crate::sdk_read_grants::SdkReadGrants::default()),
            sdk_capabilities: Arc::new(std::sync::OnceLock::new()),
            sdk_agent_key: Arc::new(std::sync::OnceLock::new()),
        }
    }
    /// Called once by the host with its existing provider encryption configuration.
    pub fn configure_sdk_agent_key(&self, key: String) -> Result<(), RegistryError> {
        if key.is_empty() { return Ok(()); }
        self.sdk_agent_key.set(Arc::from(key)).map_err(|_|RegistryError::ValidationFailed("sdk_encryption_already_configured"))
    }
    pub fn sdk_read_grants(&self)->Arc<crate::sdk_read_grants::SdkReadGrants>{self.sdk_read_grants.clone()}
    pub fn sdk_capabilities(&self)->Arc<std::sync::OnceLock<Arc<dyn crate::sdk_capabilities::SdkCapabilityAuthority>>>{self.sdk_capabilities.clone()}
    pub fn configure_sdk_capabilities(&self,authority:Arc<dyn crate::sdk_capabilities::SdkCapabilityAuthority>)->Result<(), &'static str>{
        self.sdk_capabilities.set(authority).map_err(|_|"sdk_capabilities_already_configured")
    }
    pub fn sdk_agent_key_handle(&self) -> Arc<std::sync::OnceLock<Arc<str>>> { self.sdk_agent_key.clone() }
    pub async fn register(&self, service: ServiceDefinition) -> Result<(), RegistryError> {
        self.register_handle(ServiceHandle::new(service)).await
    }
    pub async fn register_handle(&self, service: ServiceHandle) -> Result<(), RegistryError> {
        self.persist_handle(service, false).await
    }
    pub async fn create_handle(&self, service: ServiceHandle) -> Result<(), RegistryError> {
        self.persist_handle(service, true).await
    }
    async fn persist_handle(&self, service: ServiceHandle, create_only: bool) -> Result<(), RegistryError> {
        let snapshot = service.snapshot();
        let service = if crate::sdk_agent_config::is_sdk(&snapshot) || snapshot.connector_config.get("sdk_provider_api_key").is_some() || snapshot.connector_config.get("sdk_provider_api_key_encrypted").is_some() {
            let mut snapshot=service.snapshot();
            crate::sdk_agent_config::validate_and_seal(&mut snapshot,self.sdk_agent_key.get().map(|v|v.as_ref()).unwrap_or(""))
                .map_err(RegistryError::ValidationFailed)?;
            ServiceHandle::new(snapshot)
        } else { service };
        let id = service.service_id();
        if id.is_empty() {
            return Err(RegistryError::ValidationFailed("service_id is required"));
        }
        // The typed TransportType cannot contain the source's missing/empty connector.
        if create_only {
            if !self.storage.insert_if_absent(service.clone()).await? {
                return Err(RegistryError::AlreadyExists);
            }
        } else {
            self.storage.save(service.clone()).await?;
        }
        let mut adapters = self.adapter_cache.lock().expect("adapter cache poisoned");
        self.cache
            .lock()
            .expect("registry cache poisoned")
            .insert(service.clone());
        adapters.remove(&id);
        Ok(())
    }
    /// Already-decoded YAML may enter here; this method does not decode YAML itself.
    pub async fn register_from_value(&self, data: &Value) -> Result<(), RegistryError> {
        if !python_truthy(data) {
            return Ok(());
        }
        self.register(service_from_value(data)?).await
    }
    pub async fn register_from_json(&self, text: &str) -> Result<(), RegistryError> {
        let data: Value = serde_json::from_str(text).map_err(|_| RegistryError::InvalidJson)?;
        self.register_from_value(&data).await
    }
    pub async fn get(&self, service_id: &str) -> Result<Option<ServiceHandle>, RegistryError> {
        if let Some(service) = self
            .cache
            .lock()
            .expect("registry cache poisoned")
            .rows
            .get(service_id)
            .cloned()
        {
            return Ok(Some(service));
        }
        let service = self.storage.get(service_id).await?;
        if let Some(service) = &service {
            self.cache
                .lock()
                .expect("registry cache poisoned")
                .insert(service.clone());
        }
        Ok(service)
    }
    pub async fn list(
        &self,
        service_type: Option<ServiceType>,
        tags: &[String],
        status: Option<&str>,
    ) -> Result<Vec<ServiceHandle>, RegistryError> {
        let mut merged = OrderedServices::default();
        for service in self.storage.list().await? {
            merged.insert(service);
        }
        for service in self.cache.lock().expect("registry cache poisoned").list() {
            merged.insert(service);
        }
        let status = status.unwrap_or("active");
        Ok(merged
            .list()
            .into_iter()
            .filter(|service| {
                let service = service.snapshot();
                (status.is_empty() || service.status == status)
                    && service_type.is_none_or(|kind| kind == service.service_type)
                    && tags.iter().all(|tag| service.tags.contains(tag))
            })
            .collect())
    }
    pub async fn find_by_capability(
        &self,
        content_types: &[ContentType],
    ) -> Result<Vec<ServiceHandle>, RegistryError> {
        Ok(self
            .storage
            .list()
            .await?
            .into_iter()
            .filter(|service| {
                let service = service.snapshot();
                content_types
                    .iter()
                    .all(|kind| service.accepted_content_types.contains(kind))
            })
            .collect())
    }
    /// Publish the captured object without persistence or adapter-cache eviction.
    /// Replacing an existing ID preserves insertion order, as Python dict mutation.
    /// Old holders observe edits to this object, but never switch to another object.
    pub fn replace_service_cache(&self, service: ServiceHandle) {
        self.cache
            .lock()
            .expect("registry cache poisoned")
            .insert(service);
    }
    /// Observe cache only, without invoking storage or materializing defaults.
    pub fn cached_service(&self, service_id: &str) -> Option<ServiceHandle> {
        self.cache
            .lock()
            .expect("registry cache poisoned")
            .rows
            .get(service_id)
            .cloned()
    }
    /// Storage.delete does not implicitly clear this cache in Python. Routes that
    /// delete must explicitly invalidate; register already does this atomically
    /// after a successful save for both service and adapter cache entries.
    pub fn invalidate_service_cache(&self, service_id: &str) {
        self.cache
            .lock()
            .expect("registry cache poisoned")
            .delete(service_id);
    }
    pub fn invalidate_adapter_cache(&self, service_id: &str) {
        self.adapter_cache
            .lock()
            .expect("adapter cache poisoned")
            .remove(service_id);
    }
    pub fn register_adapter(
        &self,
        adapter_type: impl Into<String>,
        factory: Arc<dyn AdapterFactory>,
    ) {
        self.factories
            .lock()
            .expect("adapter factories poisoned")
            .insert(adapter_type.into(), factory);
    }
    pub fn get_adapter(
        &self,
        service: ServiceHandle,
    ) -> Result<Arc<dyn ProtocolAdapter>, RegistryError> {
        let snapshot = service.snapshot();
        let mut cached = self.adapter_cache.lock().expect("adapter cache poisoned");
        if let Some(adapter) = cached.get(&snapshot.service_id) {
            return Ok(adapter.clone());
        }
        let adapter_type = match snapshot.metadata.get("adapter_type") {
            None => "generic_rest",
            Some(Value::String(value)) => value,
            _ => return Err(RegistryError::AdapterNotFound("None".into())),
        };
        let factory = self
            .factories
            .lock()
            .expect("adapter factories poisoned")
            .get(adapter_type)
            .cloned()
            .ok_or_else(|| RegistryError::AdapterNotFound(adapter_type.into()))?;
        let adapter = factory.create(service.clone())?;
        let adapter: Arc<dyn ProtocolAdapter> =
            match bulkhead_policy(&snapshot, adapter_type, &self.bulkhead_environment)? {
                Some(policy) => {
                    Arc::new(BulkheadAdapter::new(&snapshot.service_id, adapter, policy))
                }
                None => adapter,
            };
        cached.insert(snapshot.service_id.clone(), adapter.clone());
        Ok(adapter)
    }
}

pub struct BulkheadAdapter {
    service_id: String,
    inner: Arc<dyn ProtocolAdapter>,
    slots: Arc<Semaphore>,
    queue_timeout: Duration,
}
impl BulkheadAdapter {
    pub fn new(
        service_id: impl Into<String>,
        inner: Arc<dyn ProtocolAdapter>,
        policy: BulkheadPolicy,
    ) -> Self {
        Self {
            service_id: service_id.into(),
            inner,
            slots: Arc::new(Semaphore::new(policy.concurrency_limit.max(1))),
            queue_timeout: policy.queue_timeout.max(Duration::from_millis(1)),
        }
    }
    async fn acquire(&self) -> Result<OwnedSemaphorePermit, AdapterError> {
        tokio::time::timeout(self.queue_timeout, self.slots.clone().acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| AdapterError::BulkheadExhausted {
                service_id: self.service_id.clone(),
            })
    }
}
impl ProtocolAdapter for BulkheadAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let _permit = self.acquire().await?;
            self.inner.invoke(request).await
        })
    }
    fn stream(
        &self,
        request: UnifiedRequest,
    ) -> AdapterFuture<'_, Box<dyn DispatchProducer<StreamChunk>>> {
        Box::pin(async move {
            let permit = self.acquire().await?;
            let producer = self.inner.stream(request).await?;
            Ok(Box::new(BulkheadStream {
                inner: Some(producer),
                permit: Some(permit),
                cleanup: None,
                runtime: Handle::current(),
            }) as Box<dyn DispatchProducer<StreamChunk>>)
        })
    }
    fn submit<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, String> {
        Box::pin(async move {
            let _permit = self.acquire().await?;
            self.inner.submit(request).await
        })
    }
    fn get_result<'a>(&'a self, task_id: &'a str) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move {
            let _permit = self.acquire().await?;
            self.inner.get_result(task_id).await
        })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        self.inner.health_check()
    }
}
struct BulkheadStream {
    inner: Option<Box<dyn DispatchProducer<StreamChunk>>>,
    permit: Option<OwnedSemaphorePermit>,
    cleanup: Option<JoinHandle<Result<(), DispatchError>>>,
    runtime: Handle,
}
impl BulkheadStream {
    fn start_cleanup(&mut self) {
        if let Some(inner) = self.inner.take() {
            let permit = self.permit.take();
            self.cleanup = Some(self.runtime.spawn(async move {
                let result = inner.close().await;
                drop(permit);
                result
            }));
        }
    }
    async fn finish(&mut self) -> Result<(), DispatchError> {
        self.start_cleanup();
        let result = match self.cleanup.as_mut() {
            Some(task) => task.await.unwrap_or(Err(DispatchError::CleanupFailed)),
            None => return Ok(()),
        };
        self.cleanup = None;
        result
    }
}
impl DispatchProducer<StreamChunk> for BulkheadStream {
    fn next(&mut self) -> DispatchFuture<'_, Option<StreamChunk>> {
        Box::pin(async move {
            let result = match self.inner.as_mut() {
                Some(inner) => inner.next().await,
                None => {
                    self.finish().await?;
                    return Ok(None);
                }
            };
            if !matches!(result, Ok(Some(_))) {
                self.finish().await?;
            }
            result
        })
    }
    fn close(mut self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move { self.finish().await })
    }
}
impl Drop for BulkheadStream {
    fn drop(&mut self) {
        self.start_cleanup();
    }
}
