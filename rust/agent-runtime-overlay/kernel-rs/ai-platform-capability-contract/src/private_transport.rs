//! Stage-D transport wire contract. Pure std: usable at all four service boundaries.
//! Transport budgets attenuate admitted authority; they never grant execution authority.
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

pub const DEADLINE_HEADER: &str = "x-ai-deadline-unix-ms";
pub const BUDGET_HEADER: &str = "x-ai-budget-ms";
pub const HOPS_HEADER: &str = "x-ai-hop-limit";
const MAX_MS: u64 = 3_600_000;
const MAX_HOPS: u8 = 8;
pub type Headers = Vec<(String, String)>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Malformed,
    Duplicate,
    Expired,
    HopLimit,
    InvalidPolicy,
}
fn one<'a>(headers: &'a [(String, String)], name: &str) -> Result<Option<&'a str>, Error> {
    let mut found = None;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(name) {
            if found.is_some() {
                return Err(Error::Duplicate);
            }
            found = Some(v.as_str());
        }
    }
    Ok(found)
}
fn number(value: &str, max: u64) -> Result<u64, Error> {
    if value.is_empty()
        || value.len() > 20
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::Malformed);
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0 && *v <= max)
        .ok_or(Error::Malformed)
}
/// No identity or baggage storage, and no Debug dump of untrusted carrier values.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Trace {
    parent: Option<String>,
    state: Option<String>,
}
impl Trace {
    /// Gateway: pass only the installed ActiveTraceCarrier map, never raw request headers.
    /// Other services: call only after internal auth/scope admission. Invalid correlation
    /// is dropped independently of business auth and cannot fail a valid operation.
    pub fn from_admitted_carrier(headers: &[(String, String)]) -> Self {
        let parent = one(headers, "traceparent")
            .ok()
            .flatten()
            .filter(|v| valid_parent(v))
            .map(str::to_owned);
        let state = parent
            .as_ref()
            .and_then(|_| one(headers, "tracestate").ok().flatten())
            .filter(|v| valid_state(v))
            .map(str::to_owned);
        Self { parent, state }
    }
    pub fn append(&self, headers: &mut Headers) {
        headers.retain(|(k, _)| {
            !k.eq_ignore_ascii_case("traceparent")
                && !k.eq_ignore_ascii_case("tracestate")
                && !k.eq_ignore_ascii_case("baggage")
        });
        if let Some(parent) = &self.parent {
            headers.push(("traceparent".into(), parent.clone()));
            if let Some(state) = &self.state {
                headers.push(("tracestate".into(), state.clone()));
            }
        }
    }
    pub fn trace_id(&self) -> Option<&str> {
        self.parent.as_ref().map(|p| &p[3..35])
    }
}
fn valid_parent(value: &str) -> bool {
    value.is_ascii()
        && value.len() == 55
        && value.starts_with("00-")
        && value.as_bytes()[35] == b'-'
        && value.as_bytes()[52] == b'-'
        && matches!(&value[53..], "00" | "01")
        && value[3..35].bytes().any(|b| b != b'0')
        && value[36..52].bytes().any(|b| b != b'0')
        && value.bytes().enumerate().all(|(i, b)| {
            matches!(i, 2 | 35 | 52) || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
        })
}
fn lower_digit(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}
fn key_tail(b: u8) -> bool {
    lower_digit(b) || matches!(b, b'_' | b'-' | b'*' | b'/')
}
fn valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 256 {
        return false;
    }
    if let Some((tenant, system)) = key.split_once('@') {
        !tenant.is_empty()
            && tenant.len() <= 241
            && tenant.bytes().next().is_some_and(lower_digit)
            && tenant.bytes().all(key_tail)
            && !system.is_empty()
            && system.len() <= 14
            && system
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase())
            && system.bytes().all(key_tail)
    } else {
        key.bytes().next().is_some_and(|b| b.is_ascii_lowercase()) && key.bytes().all(key_tail)
    }
}
/// W3C member grammar; allow OWS around comma-separated entries, not inside keys.
fn valid_state(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 || !value.is_ascii() {
        return false;
    }
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for raw in value.split(',') {
        count += 1;
        if count > 32 {
            return false;
        }
        let member = raw.trim_matches([' ', '\t']);
        let Some((key, val)) = member.split_once('=') else {
            return false;
        };
        if !valid_key(key)
            || !seen.insert(key)
            || val.is_empty()
            || val.len() > 256
            || val.ends_with(' ')
            || !val
                .bytes()
                .all(|b| (0x20..=0x7e).contains(&b) && b != b'=' && b != b',')
        {
            return false;
        }
    }
    true
}

/// A receiver must combine wire hints with this locally admitted ceiling. Never
/// deserialize a Budget from caller JSON or construct it from untrusted HTTP.
#[derive(Clone, Debug)]
pub struct Budget {
    deadline: Instant,
    absolute_ms: u64,
    hops: u8,
}
impl Budget {
    /// wall_ms is trusted process time; absolute_ceiling_ms is an independently
    /// verified lease/route deadline. HTTP fields must never supply that argument.
    pub fn admit(
        local_cap: Duration,
        absolute_ceiling_ms: u64,
        wall_ms: u64,
        now: Instant,
    ) -> Result<Self, Error> {
        let cap = u64::try_from(local_cap.as_millis()).map_err(|_| Error::InvalidPolicy)?;
        if cap == 0 || cap > MAX_MS {
            return Err(Error::InvalidPolicy);
        }
        let absolute_ms =
            absolute_ceiling_ms.min(wall_ms.checked_add(cap).ok_or(Error::InvalidPolicy)?);
        let remaining = absolute_ms
            .checked_sub(wall_ms)
            .filter(|v| *v > 0)
            .ok_or(Error::Expired)?;
        Ok(Self {
            deadline: now
                .checked_add(Duration::from_millis(remaining))
                .ok_or(Error::InvalidPolicy)?,
            absolute_ms,
            hops: MAX_HOPS,
        })
    }
    /// Public ingress explicitly ignores budget-like headers. Its caller must
    /// already have selected a locally configured cap and verified lease ceiling.
    pub fn public_ingress(
        local_cap: Duration,
        absolute_ceiling_ms: u64,
        wall_ms: u64,
        now: Instant,
        _untrusted: &[(String, String)],
    ) -> Result<Self, Error> {
        Self::admit(local_cap, absolute_ceiling_ms, wall_ms, now)
    }
    pub fn remaining(&self, now: Instant) -> Result<Duration, Error> {
        self.deadline
            .checked_duration_since(now)
            .filter(|d| d.as_millis() > 0)
            .ok_or(Error::Expired)
    }
    /// Only authenticated private peers may attenuate with the complete triplet.
    /// Partial, duplicate, zero, overflow and noncanonical numbers fail closed.
    pub fn constrain_authenticated(
        &self,
        headers: &[(String, String)],
        wall_ms: u64,
        now: Instant,
    ) -> Result<Self, Error> {
        self.remaining(now)?;
        let fields = [
            one(headers, DEADLINE_HEADER)?,
            one(headers, BUDGET_HEADER)?,
            one(headers, HOPS_HEADER)?,
        ];
        if fields.iter().all(Option::is_none) {
            return Ok(self.clone());
        }
        let [Some(absolute), Some(relative), Some(hops)] = fields else {
            return Err(Error::Malformed);
        };
        let absolute = number(absolute, u64::MAX)?;
        let relative = number(relative, MAX_MS)?;
        let hops = number(hops, u64::from(MAX_HOPS))? as u8;
        let remote = absolute
            .checked_sub(wall_ms)
            .filter(|v| *v > 0)
            .ok_or(Error::Expired)?;
        let remaining = self
            .remaining(now)?
            .min(Duration::from_millis(relative.min(remote)));
        let absolute_ms = self
            .absolute_ms
            .min(absolute)
            .min(wall_ms.saturating_add(remaining.as_millis() as u64));
        Ok(Self {
            deadline: now.checked_add(remaining).ok_or(Error::InvalidPolicy)?,
            absolute_ms,
            hops: self.hops.min(hops),
        })
    }
    /// Child requests consume a hop and cannot renew the parent deadline. Use
    /// the returned budget for the entire send + body + decode, not only headers.
    pub fn child(&self, policy: EdgePolicy, now: Instant) -> Result<Self, Error> {
        if self.hops <= 1 {
            return Err(Error::HopLimit);
        }
        let remaining = self.remaining(now)?.min(policy.total);
        Ok(Self {
            deadline: now.checked_add(remaining).ok_or(Error::InvalidPolicy)?,
            absolute_ms: self.absolute_ms,
            hops: self.hops - 1,
        })
    }
    pub fn append(&self, headers: &mut Headers, wall_ms: u64, now: Instant) -> Result<(), Error> {
        let remaining =
            u64::try_from(self.remaining(now)?.as_millis()).map_err(|_| Error::InvalidPolicy)?;
        let absolute = self
            .absolute_ms
            .min(wall_ms.checked_add(remaining).ok_or(Error::InvalidPolicy)?);
        if absolute <= wall_ms {
            return Err(Error::Expired);
        }
        headers.retain(|(k, _)| {
            ![DEADLINE_HEADER, BUDGET_HEADER, HOPS_HEADER]
                .iter()
                .any(|name| k.eq_ignore_ascii_case(name))
        });
        headers.extend([
            (DEADLINE_HEADER.into(), absolute.to_string()),
            (BUDGET_HEADER.into(), remaining.to_string()),
            (HOPS_HEADER.into(), self.hops.to_string()),
        ]);
        Ok(())
    }
    pub fn timeout(&self, cap: Duration, now: Instant) -> Result<Duration, Error> {
        Ok(self.remaining(now)?.min(cap))
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Edge {
    GatewayRuntimeCommand,
    RuntimeWorker,
    WorkerKnowledge,
    Interrupt,
    RuntimeEvents,
}
#[derive(Clone, Copy, Debug)]
pub struct EdgePolicy {
    connect: Duration,
    headers: Duration,
    idle: Duration,
    total: Duration,
    stream: bool,
}
impl Edge {
    /// Preserve existing command 300s, Worker/Knowledge 30s and event liveness
    /// defaults. No heartbeat is execution authority and no policy is unbounded.
    pub fn policy(self) -> EdgePolicy {
        let (c, h, i, t) = match self {
            Self::GatewayRuntimeCommand => (10, 300, 300, 300),
            Self::RuntimeWorker => (5, 30, 30, 30),
            Self::WorkerKnowledge => (5, 30, 30, 30),
            Self::Interrupt => (2, 5, 5, 5),
            Self::RuntimeEvents => (10, 30, 45, 3600),
        };
        EdgePolicy {
            connect: Duration::from_secs(c),
            headers: Duration::from_secs(h),
            idle: Duration::from_secs(i),
            total: Duration::from_secs(t),
            stream: matches!(self, Self::RuntimeEvents),
        }
    }
}
impl EdgePolicy {
    pub fn connect_timeout(self, budget: &Budget, now: Instant) -> Result<Duration, Error> {
        budget.timeout(self.connect, now)
    }
    pub fn header_timeout(self, budget: &Budget, now: Instant) -> Result<Duration, Error> {
        budget.timeout(self.headers, now)
    }
    pub fn idle_timeout(self, budget: &Budget, now: Instant) -> Result<Duration, Error> {
        budget.timeout(self.idle, now)
    }
    /// Streaming uses bounded connect/header/per-read idle, never command-style
    /// whole-response timeout. Its execution authority remains independently finite.
    pub fn total_timeout(self, budget: &Budget, now: Instant) -> Result<Duration, Error> {
        if self.stream {
            return Err(Error::InvalidPolicy);
        }
        budget.timeout(self.total, now)
    }
    pub fn is_streaming(self) -> bool {
        self.stream
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}
/// No automatic mutation retry, including POST read APIs. GET reconnect must
/// recover by durable cursor, and remains bounded by the original budget.
pub fn retry_allowed(method: Method, attempt: u8, budget: &Budget, now: Instant) -> bool {
    method == Method::Get && attempt == 0 && budget.remaining(now).is_ok()
}
#[derive(Clone, Copy, Debug)]
pub enum Effect {
    Read,
    Write,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LateResult {
    ReadUnavailable,
    UnknownSideEffect,
    ReconcileReceipt,
}
/// A late response never fabricates success/cancellation or replays a write.
/// Existing durable receipt/owner-fence checks decide reconciliation outcomes.
pub fn classify_late(effect: Effect, has_durable_receipt: bool) -> LateResult {
    if has_durable_receipt {
        LateResult::ReconcileReceipt
    } else {
        match effect {
            Effect::Read => LateResult::ReadUnavailable,
            Effect::Write | Effect::Unknown => LateResult::UnknownSideEffect,
        }
    }
}
#[cfg(test)]
#[path = "private_transport_tests.rs"]
mod tests;
