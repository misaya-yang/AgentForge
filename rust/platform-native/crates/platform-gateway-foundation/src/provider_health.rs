//! Process-local connection-test receipts from `src/services/llm/provider_health.py`.
//!
//! One store belongs to one application state, shared across that application's
//! requests (not one store per request, tenant, provider, or global singleton).
//! Callers supply exclusive access just as synchronous Python functions cannot
//! interleave on its event loop. Construction is inert; there are no routes,
//! timers, provider calls, automatic invalidation, or background expiration.
//!
//! The typed boundary replaces arbitrary mutable Python state/dictionaries with
//! private receipts. Result objects remain JSON maps: only literal `true` counts
//! as success. Missing requests are no-ops except configured status reads, which
//! return a typed error rather than Python's AttributeError. Datetime awareness
//! mismatches remain errors, not healthy/unhealthy receipts. ProtocolTimestamp
//! preserves Python datetime precision, original offset, and ISO formatting.
use crate::protocol::ProtocolTimestamp;
use chrono::{TimeDelta, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{collections::VecDeque, fmt};

pub const PROBE_TTL_SECONDS: i64 = 300;
pub const MAX_PROBE_RECEIPTS: usize = 256;

pub trait ProviderProbeRuntime {
    fn probe_id(&self) -> String;
    fn now(&self) -> ProtocolTimestamp;
}

pub struct SystemProviderProbeRuntime;
impl ProviderProbeRuntime for SystemProviderProbeRuntime {
    fn probe_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn now(&self) -> ProtocolTimestamp {
        Utc::now().fixed_offset().into()
    }
}

#[derive(Debug)]
struct Receipt {
    tenant_id: String,
    provider_id: String,
    probe_id: String,
    checked: Option<ProtocolTimestamp>,
    success: bool,
}

/// Bounded insertion order; recording and reading never promote an entry.
#[derive(Debug, Default)]
pub struct ProviderProbeStore {
    receipts: VecDeque<Receipt>,
}
impl ProviderProbeStore {
    pub fn new() -> Self {
        Self::default()
    }
    fn position(&self, tenant_id: &str, provider_id: &str) -> Option<usize> {
        self.receipts
            .iter()
            .position(|r| r.tenant_id == tenant_id && r.provider_id == provider_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProbeState {
    NotConfigured,
    Unverified,
    Stale,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderProbeStatus {
    pub status: ProviderProbeState,
    pub last_check: Option<String>,
    pub probe_source: Option<&'static str>,
}
impl ProviderProbeStatus {
    fn empty(status: ProviderProbeState) -> Self {
        Self {
            status,
            last_check: None,
            probe_source: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProbeError {
    MissingRequest,
    MixedDatetimeAwareness,
}
impl fmt::Display for ProviderProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingRequest => "provider_probe_missing_request",
            Self::MixedDatetimeAwareness => "provider_probe_mixed_datetime_awareness",
        })
    }
}
impl std::error::Error for ProviderProbeError {}

pub fn begin_provider_probe(
    request: Option<&mut ProviderProbeStore>,
    tenant_id: &str,
    provider_id: &str,
    runtime: &dyn ProviderProbeRuntime,
) -> Option<String> {
    let store = request?;
    let probe_id = runtime.probe_id();
    if let Some(index) = store.position(tenant_id, provider_id) {
        store.receipts.remove(index);
    }
    store.receipts.push_back(Receipt {
        tenant_id: tenant_id.to_owned(),
        provider_id: provider_id.to_owned(),
        probe_id: probe_id.clone(),
        checked: None,
        success: false,
    });
    while store.receipts.len() > MAX_PROBE_RECEIPTS {
        store.receipts.pop_front();
    }
    Some(probe_id)
}

pub fn record_provider_probe(
    request: Option<&mut ProviderProbeStore>,
    tenant_id: &str,
    provider_id: &str,
    result: &Map<String, Value>,
    probe_id: Option<&str>,
    now: Option<ProtocolTimestamp>,
    runtime: &dyn ProviderProbeRuntime,
) {
    let (Some(store), Some(probe_id)) = (request, probe_id) else {
        return;
    };
    let Some(index) = store.position(tenant_id, provider_id) else {
        return;
    };
    let receipt = &mut store.receipts[index];
    if receipt.probe_id != probe_id {
        return;
    }
    receipt.checked = Some(now.unwrap_or_else(|| runtime.now()));
    receipt.success = result.get("success") == Some(&Value::Bool(true));
}

pub fn invalidate_provider_probe(
    request: Option<&mut ProviderProbeStore>,
    tenant_id: &str,
    provider_id: &str,
) {
    let Some(store) = request else {
        return;
    };
    if let Some(index) = store.position(tenant_id, provider_id) {
        store.receipts.remove(index);
    }
}

pub fn provider_probe_status(
    request: Option<&ProviderProbeStore>,
    tenant_id: &str,
    provider_id: &str,
    configured: bool,
    now: Option<ProtocolTimestamp>,
    runtime: &dyn ProviderProbeRuntime,
) -> Result<ProviderProbeStatus, ProviderProbeError> {
    if !configured {
        return Ok(ProviderProbeStatus::empty(
            ProviderProbeState::NotConfigured,
        ));
    }
    let store = request.ok_or(ProviderProbeError::MissingRequest)?;
    let Some(receipt) = store
        .position(tenant_id, provider_id)
        .map(|index| &store.receipts[index])
    else {
        return Ok(ProviderProbeStatus::empty(ProviderProbeState::Unverified));
    };
    let Some(checked) = receipt.checked else {
        return Ok(ProviderProbeStatus::empty(ProviderProbeState::Unverified));
    };
    let current = now.unwrap_or_else(|| runtime.now());
    if matches!(checked, ProtocolTimestamp::Naive(_))
        != matches!(current, ProtocolTimestamp::Naive(_))
    {
        return Err(ProviderProbeError::MixedDatetimeAwareness);
    }
    let age = current
        .naive_utc()
        .signed_duration_since(checked.naive_utc());
    let status = if age < TimeDelta::zero() || age > TimeDelta::seconds(PROBE_TTL_SECONDS) {
        ProviderProbeState::Stale
    } else if receipt.success {
        ProviderProbeState::Healthy
    } else {
        ProviderProbeState::Unhealthy
    };
    Ok(ProviderProbeStatus {
        status,
        last_check: Some(checked.to_isoformat()),
        probe_source: Some("connection_test"),
    })
}
