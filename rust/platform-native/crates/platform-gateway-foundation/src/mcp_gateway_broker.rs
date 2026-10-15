//! Gateway-owned, tenant-authorized discovery with reusable native MCP clients.
//!
//! The repository reauthorizes the exact connection before *every* cache access;
//! no cache entry grants authority. Secret references are resolved only while
//! creating an initialized generation. No secret, reference or remote error is
//! logged or exposed in Debug. All constructors and unpolled futures are inert.
//!
//! Cache identity follows the Python source, including its asymmetry: discovery
//! SELECT omits credential/server revision columns, so absent revisions are
//! `none`/empty and token changes cannot invalidate sooner than TTL unless the
//! host explicitly invalidates. TTL is lazy, not a background timer.
//!
//! A cancelled waiter does not cancel shared initialization. Invalidation aborts
//! and joins initializers, prevents a stale initializer from repopulating cache,
//! and permits existing leases to finish before closing their client. close()
//! additionally drains all active leases. Rust Arc ownership fixes the source's
//! removed-entry early-close race without changing successful route results.
//! Host shutdown must await close(); Drop can abort but cannot await OS DNS work.

use crate::mcp_http::{
    MCP_PROTOCOL_VERSION, McpClientConfig, McpClientFactory, McpProtocolClient, broker_error,
    record_integer,
};
use crate::mcp_registry_store::{McpContext, McpError, McpFuture, McpRepository};
use crate::service_registry::python_truthy;
use crate::usage_parser::python_value_to_string;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

pub trait McpDiscovery: Send + Sync {
    fn repository_matches(&self, _repository: &Arc<dyn McpRepository>) -> bool {
        true
    }
    fn discover<'a>(
        &'a self,
        context: &'a McpContext,
        server_id: &'a str,
        connection_id: &'a str,
        principal_type: &'a str,
    ) -> McpFuture<'a, Value>;
}
pub trait McpSecretResolver: Send + Sync {
    fn resolve<'a>(&'a self, secret_ref: &'a str) -> McpFuture<'a, String>;
}
/// Injectable environment lookup: construction never reads process environment.
pub trait McpEnvironment: Send + Sync {
    fn read(&self, name: &str) -> Option<String>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessMcpEnvironment;
impl McpEnvironment for ProcessMcpEnvironment {
    fn read(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}
pub struct ConfiguredEnvironmentSecretResolver {
    references: HashMap<String, String>,
    environment: Arc<dyn McpEnvironment>,
}
impl fmt::Debug for ConfiguredEnvironmentSecretResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfiguredEnvironmentSecretResolver")
            .field("refs", &self.references.len())
            .finish()
    }
}
impl ConfiguredEnvironmentSecretResolver {
    pub fn new(references: HashMap<String, String>, environment: Arc<dyn McpEnvironment>) -> Self {
        let valid = |name: &str| {
            name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        };
        Self {
            references: references
                .into_iter()
                .filter(|(_, name)| valid(name))
                .collect(),
            environment,
        }
    }
    /// Host supplies MCP_SECRET_REF_MAP text. Malformed input yields no refs.
    pub fn from_json(text: &str, environment: Arc<dyn McpEnvironment>) -> Self {
        let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let references = parsed
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Self::new(references, environment)
    }
    pub fn ready(&self) -> bool {
        self.references
            .values()
            .any(|name| self.environment.read(name).is_some_and(|v| !v.is_empty()))
    }
}
impl McpSecretResolver for ConfiguredEnvironmentSecretResolver {
    fn resolve<'a>(&'a self, secret_ref: &'a str) -> McpFuture<'a, String> {
        Box::pin(async move {
            self.references
                .get(secret_ref)
                .and_then(|name| self.environment.read(name))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| broker_error("MCP_SECRET_UNAVAILABLE"))
        })
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct McpClientCacheKey {
    pub tenant_id: String,
    pub connection_id: String,
    pub credential_revision: String,
    pub protocol_version: String,
    pub config_fingerprint: String,
}
fn value_or<'a>(value: &'a Value, key: &str, fallback: &'a Value) -> &'a Value {
    value
        .get(key)
        .filter(|v| python_truthy(v))
        .unwrap_or(fallback)
}
/// Python json.dumps(sort_keys=True, separators=(",",":"), ensure_ascii=True)
/// on configuration values. No credential value or opaque reference participates.
fn canonical_config(value: &Value, output: &mut String) {
    match value {
        Value::Object(map) => {
            output.push('{');
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    output.push(',');
                }
                canonical_config(&json!(key), output);
                output.push(':');
                canonical_config(&map[key], output);
            }
            output.push('}');
        }
        Value::Array(values) => {
            output.push('[');
            for (i, value) in values.iter().enumerate() {
                if i > 0 {
                    output.push(',');
                }
                canonical_config(value, output);
            }
            output.push(']');
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
            for c in rendered.chars() {
                if c < '\u{007f}' {
                    output.push(c);
                } else {
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        output.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
        }
    }
}
pub fn mcp_client_cache_key(item: &Value, tenant_id: &str) -> Result<McpClientCacheKey, McpError> {
    let empty = json!("");
    let none = json!("none");
    let endpoint = json!("/mcp");
    let revision = python_value_to_string(value_or(
        item,
        "credential_revision",
        value_or(item, "credential_updated_at", &none),
    ));
    let server_revision = python_value_to_string(value_or(
        item,
        "server_updated_at",
        value_or(item, "updated_at", &empty),
    ));
    let mut origins = item
        .get("allowed_origins")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(python_value_to_string).collect::<Vec<_>>())
        .unwrap_or_default();
    origins.sort();
    let values = json!({"server_revision":server_revision,
        "base_url":python_value_to_string(value_or(item,"base_url",&empty)),
        "endpoint_path":python_value_to_string(value_or(item,"endpoint_path",&endpoint)),
        "timeout_ms":record_integer(item,"timeout_ms",30_000)?,"max_concurrency":record_integer(item,"max_concurrency",5)?,
        "response_limit_bytes":record_integer(item,"response_limit_bytes",1_048_576)?,
        "auth_method":python_value_to_string(value_or(item,"auth_method",&none)),
        "oauth_resource":item.get("oauth_resource").unwrap_or(&Value::Null),
        "oauth_audience":item.get("oauth_audience").unwrap_or(&Value::Null),
        "audience":item.get("audience").unwrap_or(&Value::Null),"allowed_origins":origins});
    let mut serialized = String::new();
    canonical_config(&values, &mut serialized);
    let digest = Sha256::digest(serialized.as_bytes());
    let fingerprint = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(McpClientCacheKey {
        tenant_id: tenant_id.into(),
        connection_id: item
            .get("connection_id")
            .map(python_value_to_string)
            .ok_or(McpError::Unhandled)?,
        credential_revision: revision,
        protocol_version: MCP_PROTOCOL_VERSION.into(),
        config_fingerprint: fingerprint,
    })
}

pub trait McpClock: Send + Sync {
    fn now(&self) -> Instant;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicMcpClock;
impl McpClock for MonotonicMcpClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
struct Entry {
    client: Arc<dyn McpProtocolClient>,
    invalidated: AtomicBool,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.client.close();
    }
}
struct Cached {
    entry: Arc<Entry>,
    expires_at: Instant,
    last_used_at: Instant,
    insertion_order: u64,
}
#[derive(Clone)]
enum FlightState {
    Waiting,
    Complete(Result<Arc<Entry>, McpError>),
}
struct Flight {
    result: watch::Sender<FlightState>,
    task: Mutex<Option<JoinHandle<()>>>,
}
impl Flight {
    fn cancel(&self) {
        self.result
            .send_replace(FlightState::Complete(Err(broker_error(
                "MCP_CLIENT_INVALIDATED",
            ))));
        if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            task.abort();
        }
    }
    async fn join(&self) {
        let task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
#[derive(Default)]
struct CacheState {
    cache: HashMap<McpClientCacheKey, Cached>,
    inflight: HashMap<McpClientCacheKey, Arc<Flight>>,
    closed: bool,
    next_order: u64,
}
struct Inner {
    repository: Arc<dyn McpRepository>,
    secrets: Arc<dyn McpSecretResolver>,
    factory: Arc<dyn McpClientFactory>,
    clock: Arc<dyn McpClock>,
    ttl: Duration,
    max_entries: usize,
    state: Mutex<CacheState>,
    active: AtomicUsize,
    drained: Notify,
}
impl Drop for Inner {
    fn drop(&mut self) {
        for flight in self
            .state
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .inflight
            .values()
        {
            flight.cancel();
        }
    }
}
struct Lease {
    entry: Option<Arc<Entry>>,
    owner: Arc<Inner>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        // close() must see the client released before observing zero leases.
        drop(self.entry.take());
        if self.owner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.owner.drained.notify_waiters();
        }
    }
}
struct FlightGuard {
    owner: Weak<Inner>,
    key: McpClientCacheKey,
    flight: Arc<Flight>,
    finished: bool,
}
impl Drop for FlightGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.flight
            .result
            .send_replace(FlightState::Complete(Err(McpError::Cancelled)));
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .inflight
                .get(&self.key)
                .is_some_and(|v| Arc::ptr_eq(v, &self.flight))
            {
                state.inflight.remove(&self.key);
            }
        }
    }
}

#[derive(Clone)]
pub struct McpGatewayBroker {
    inner: Arc<Inner>,
}
impl fmt::Debug for McpGatewayBroker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpGatewayBroker(<internal>)")
    }
}
impl McpGatewayBroker {
    pub fn new(
        repository: Arc<dyn McpRepository>,
        secrets: Arc<dyn McpSecretResolver>,
        factory: Arc<dyn McpClientFactory>,
        ttl_seconds: f64,
        max_entries: usize,
    ) -> Self {
        Self::with_clock(
            repository,
            secrets,
            factory,
            ttl_seconds,
            max_entries,
            Arc::new(MonotonicMcpClock),
        )
    }
    pub fn with_clock(
        repository: Arc<dyn McpRepository>,
        secrets: Arc<dyn McpSecretResolver>,
        factory: Arc<dyn McpClientFactory>,
        ttl_seconds: f64,
        max_entries: usize,
        clock: Arc<dyn McpClock>,
    ) -> Self {
        let ttl = if ttl_seconds.is_nan() {
            1.0
        } else {
            ttl_seconds.clamp(1.0, 3600.0)
        };
        Self {
            inner: Arc::new(Inner {
                repository,
                secrets,
                factory,
                clock,
                ttl: Duration::from_secs_f64(ttl),
                max_entries: max_entries.clamp(1, 1000),
                state: Mutex::new(CacheState::default()),
                active: AtomicUsize::new(0),
                drained: Notify::new(),
            }),
        }
    }
    fn start_flight(&self, key: McpClientCacheKey, item: Value) -> Arc<Flight> {
        let (result, _) = watch::channel(FlightState::Waiting);
        let flight = Arc::new(Flight {
            result,
            task: Mutex::new(None),
        });
        let owner = Arc::downgrade(&self.inner);
        let secrets = self.inner.secrets.clone();
        let factory = self.inner.factory.clone();
        let task_flight = flight.clone();
        let task = tokio::spawn(async move {
            let mut guard = FlightGuard {
                owner: owner.clone(),
                key: key.clone(),
                flight: task_flight.clone(),
                finished: false,
            };
            let result = async {
                let credential = if item
                    .get("auth_method")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("none")
                    == "none"
                {
                    None
                } else {
                    let reference = item
                        .get("secret_ref")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| broker_error("MCP_SECRET_UNAVAILABLE"))?;
                    let value = secrets
                        .resolve(reference)
                        .await
                        .map_err(|_| broker_error("MCP_SECRET_UNAVAILABLE"))?;
                    if value.is_empty() {
                        return Err(broker_error("MCP_SECRET_UNAVAILABLE"));
                    }
                    Some(value)
                };
                let config = McpClientConfig::from_record(&item, credential)
                    .map_err(|_| McpError::Unhandled)?;
                let initialized = factory.create(config).await?;
                let entry = Arc::new(Entry {
                    client: initialized.client,
                    invalidated: AtomicBool::new(false),
                });
                if initialized.protocol_version.as_deref() != Some(MCP_PROTOCOL_VERSION) {
                    return Err(broker_error("MCP_PROTOCOL_VERSION_MISMATCH"));
                }
                Ok(entry)
            }
            .await;
            if let Some(owner) = owner.upgrade() {
                owner.finish_flight(&key, &task_flight, result);
            } else {
                task_flight
                    .result
                    .send_replace(FlightState::Complete(Err(McpError::Cancelled)));
            }
            guard.finished = true;
        });
        *flight.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        flight
    }
    async fn acquire(&self, item: Value, tenant_id: &str) -> Result<Lease, McpError> {
        let key = mcp_client_cache_key(&item, tenant_id)?;
        let now = self.inner.clock.now();
        let mut removed = Vec::new();
        let (cached, mut waiting) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return Err(broker_error("MCP_CLIENT_INVALIDATED"));
            }
            let stale: Vec<_> = state
                .cache
                .iter()
                .filter(|(k, v)| {
                    v.expires_at <= now
                        || (k.tenant_id == key.tenant_id
                            && k.connection_id == key.connection_id
                            && *k != &key)
                })
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                if let Some(old) = state.cache.remove(&k) {
                    old.entry.invalidated.store(true, Ordering::Release);
                    removed.push(old);
                }
            }
            if let Some(cached) = state.cache.get_mut(&key) {
                cached.last_used_at = now;
                self.inner.active.fetch_add(1, Ordering::AcqRel);
                (Some(cached.entry.clone()), None)
            } else {
                let flight = state
                    .inflight
                    .entry(key.clone())
                    .or_insert_with(|| self.start_flight(key.clone(), item));
                (None, Some(flight.result.subscribe()))
            }
        };
        drop(removed);
        if let Some(entry) = cached {
            return Ok(Lease {
                entry: Some(entry),
                owner: self.inner.clone(),
            });
        }
        let receiver = waiting
            .as_mut()
            .ok_or_else(|| broker_error("MCP_CLIENT_INVALIDATED"))?;
        let entry = loop {
            let state = receiver.borrow().clone();
            match state {
                FlightState::Complete(result) => break result?,
                FlightState::Waiting => receiver
                    .changed()
                    .await
                    .map_err(|_| broker_error("MCP_CLIENT_INVALIDATED"))?,
            }
        };
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed || entry.invalidated.load(Ordering::Acquire) {
                return Err(broker_error("MCP_CLIENT_INVALIDATED"));
            }
            let cached = state
                .cache
                .get_mut(&key)
                .filter(|v| Arc::ptr_eq(&v.entry, &entry))
                .ok_or_else(|| broker_error("MCP_CLIENT_INVALIDATED"))?;
            cached.last_used_at = self.inner.clock.now();
            self.inner.active.fetch_add(1, Ordering::AcqRel);
        }
        Ok(Lease {
            entry: Some(entry),
            owner: self.inner.clone(),
        })
    }
    pub async fn invalidate_connection(&self, tenant_id: &str, connection_id: &str) {
        let (removed, flights) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<_> = state
                .cache
                .keys()
                .filter(|k| k.tenant_id == tenant_id && k.connection_id == connection_id)
                .cloned()
                .collect();
            let removed: Vec<_> = keys.iter().filter_map(|k| state.cache.remove(k)).collect();
            for entry in &removed {
                entry.entry.invalidated.store(true, Ordering::Release);
            }
            let keys: Vec<_> = state
                .inflight
                .keys()
                .filter(|k| k.tenant_id == tenant_id && k.connection_id == connection_id)
                .cloned()
                .collect();
            let flights: Vec<_> = keys
                .iter()
                .filter_map(|k| state.inflight.remove(k))
                .collect();
            for flight in &flights {
                flight.cancel();
            }
            (removed, flights)
        };
        drop(removed);
        for flight in flights {
            flight.join().await;
        }
    }
    pub async fn close(&self) {
        let (removed, flights) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closed = true;
            let removed = std::mem::take(&mut state.cache);
            for cached in removed.values() {
                cached.entry.invalidated.store(true, Ordering::Release);
            }
            let flights = std::mem::take(&mut state.inflight);
            for flight in flights.values() {
                flight.cancel();
            }
            (removed, flights)
        };
        drop(removed);
        for flight in flights.into_values() {
            flight.join().await;
        }
        loop {
            let changed = self.inner.drained.notified();
            if self.inner.active.load(Ordering::Acquire) == 0 {
                break;
            }
            changed.await;
        }
    }
}
impl Inner {
    fn finish_flight(
        &self,
        key: &McpClientCacheKey,
        flight: &Arc<Flight>,
        result: Result<Arc<Entry>, McpError>,
    ) {
        let mut removed = Vec::new();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed
                || !state
                    .inflight
                    .get(key)
                    .is_some_and(|v| Arc::ptr_eq(v, flight))
            {
                flight
                    .result
                    .send_replace(FlightState::Complete(Err(broker_error(
                        "MCP_CLIENT_INVALIDATED",
                    ))));
                return;
            }
            state.inflight.remove(key);
            if let Ok(entry) = &result {
                let now = self.clock.now();
                let insertion_order = state.next_order;
                state.next_order = state.next_order.wrapping_add(1);
                if let Some(old) = state.cache.insert(
                    key.clone(),
                    Cached {
                        entry: entry.clone(),
                        expires_at: now + self.ttl,
                        last_used_at: now,
                        insertion_order,
                    },
                ) {
                    old.entry.invalidated.store(true, Ordering::Release);
                    removed.push(old);
                }
                while state.cache.len() > self.max_entries {
                    let oldest = state
                        .cache
                        .iter()
                        .min_by_key(|(_, entry)| (entry.last_used_at, entry.insertion_order))
                        .map(|(key, _)| key.clone());
                    if let Some(old) = oldest.and_then(|key| state.cache.remove(&key)) {
                        old.entry.invalidated.store(true, Ordering::Release);
                        removed.push(old);
                    }
                }
            }
            flight.result.send_replace(FlightState::Complete(result));
        }
        drop(removed);
    }
}
impl McpDiscovery for McpGatewayBroker {
    fn repository_matches(&self, repository: &Arc<dyn McpRepository>) -> bool {
        Arc::ptr_eq(&self.inner.repository, repository)
    }
    fn discover<'a>(
        &'a self,
        context: &'a McpContext,
        server_id: &'a str,
        connection_id: &'a str,
        principal_type: &'a str,
    ) -> McpFuture<'a, Value> {
        Box::pin(async move {
            // Canonical repository owns exact tenant/server/connection/principal,
            // enabled/revoked/expiry, delegated owner and OAuth audience checks.
            let item=self.inner.repository.call("resolve_discovery_connection",context,
                json!({"server_id":server_id,"connection_id":connection_id,"principal_type":principal_type})).await?;
            let lease = self.acquire(item, &context.tenant_id).await?;
            let result = lease
                .entry
                .as_ref()
                .ok_or(McpError::Unhandled)?
                .client
                .list_tools()
                .await;
            drop(lease);
            let mut tools = result?;
            // Never accept trusted operation facts from an injected remote client.
            if let Some(tools) = tools.as_array_mut() {
                for tool in tools {
                    if let Some(tool) = tool.as_object_mut() {
                        tool.insert("read_only".into(), json!(false));
                        tool.insert("risk_level".into(), json!("medium"));
                    }
                }
            }
            let mut recorded = self
                .inner
                .repository
                .call(
                    "record_discovery",
                    context,
                    json!({"server_id":server_id,"tools":tools}),
                )
                .await?;
            let record = recorded.as_object_mut().ok_or(McpError::Unhandled)?;
            record.insert("protocol_version".into(), json!(MCP_PROTOCOL_VERSION));
            Ok(recorded)
        })
    }
}
