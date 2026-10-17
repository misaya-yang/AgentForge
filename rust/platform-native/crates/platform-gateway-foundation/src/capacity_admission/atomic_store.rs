//! One Redis script per Cluster slot. Global guards and tenant admission are
//! separate slots; rate, tenant capacity and token intent commit in one script.
use crate::{dispatcher_rate_limiter::RateLimit, quota_store::reservations::TokenEnvelope};
use redis::{Script, aio::ConnectionManager};
use serde::Serialize;
use std::time::Duration;

pub(super) const ACQUIRE: &str = r#"
local p = cjson.decode(ARGV[1])
local t = redis.call('TIME')
local now = tonumber(t[1])*1000 + math.floor(tonumber(t[2])/1000)
local now_us = tonumber(t[1])*1000000 + tonumber(t[2])
local tats = {}
for i, limit in ipairs(p.capacities) do
  redis.call('ZREMRANGEBYSCORE', KEYS[i], '-inf', now)
  if redis.call('ZCARD', KEYS[i]) >= limit then return {0,1,i} end
end
local offset = #p.capacities
local rate_seen = false
local dedupe_key = offset + #p.rates + 1
if p.dedupe_ttl_ms > 0 and #p.rates > 0 then
 redis.call('ZREMRANGEBYSCORE',KEYS[dedupe_key],'-inf',now)
 rate_seen = redis.call('ZSCORE',KEYS[dedupe_key],p.logical_id) ~= false
end
for i, rate in ipairs(p.rates) do
  local interval = rate.window_ms * 1000 / rate.requests
  local tat = math.max(tonumber(redis.call('GET', KEYS[offset+i]) or now_us), now_us)
  local next = tat
  if not rate_seen then next = tat + interval end
  local wait = next - (rate.requests + rate.burst)*interval - now_us
  if not rate_seen and wait > 0 then return {0,2,math.max(1,math.ceil(wait/1000000))} end
  tats[i] = next
end
if p.tokens then
  local e = p.tokens
  if e.daily_limit ~= cjson.null and e.daily_used + e.pending + e.estimate > e.daily_limit then return {0,3,1} end
  if e.monthly_limit ~= cjson.null and e.monthly_used + e.pending + e.estimate > e.monthly_limit then return {0,3,1} end
end
for i, limit in ipairs(p.capacities) do
  redis.call('ZADD', KEYS[i], now+p.ttl_ms, p.member)
  redis.call('PEXPIRE', KEYS[i], p.ttl_ms*2)
end
if not rate_seen then
for i, tat in ipairs(tats) do
  redis.call('SET', KEYS[offset+i], string.format('%.0f', tat), 'PX', math.max(1, math.ceil((tat-now_us)/1000)))
end
if p.dedupe_ttl_ms > 0 and #p.rates > 0 then
 redis.call('ZADD',KEYS[dedupe_key],now+p.dedupe_ttl_ms,p.logical_id)
 redis.call('PEXPIRE',KEYS[dedupe_key],p.dedupe_ttl_ms*2)
end
end
if p.tokens then
  redis.call('HSET', KEYS[#KEYS], p.member, cjson.encode(p.tokens))
  redis.call('PEXPIRE', KEYS[#KEYS], p.ttl_ms*2)
end
local used=0
if #p.rates>0 then
 local rate=p.rates[#p.rates]
 used=math.max(0,math.ceil((tats[#tats]-now_us)/(rate.window_ms*1000/rate.requests)))
end
return {1,0,used}
"#;
const RENEW: &str = r#"
local clock=redis.call('TIME')
local now=tonumber(clock[1])*1000+math.floor(tonumber(clock[2])/1000)
for i=1,#KEYS do
 local score=redis.call('ZSCORE',KEYS[i],ARGV[1])
 if not score or tonumber(score)<=now then return 0 end
end
for i=1,#KEYS do
 redis.call('ZADD',KEYS[i],'XX',now+tonumber(ARGV[2]),ARGV[1])
 redis.call('PEXPIRE',KEYS[i],tonumber(ARGV[2])*2)
end
return 1
"#;
const RELEASE: &str = r#"
for i=1,tonumber(ARGV[2]) do redis.call('ZREM',KEYS[i],ARGV[1]) end
if #KEYS>tonumber(ARGV[2]) then redis.call('HDEL',KEYS[#KEYS],ARGV[1]) end
return 1
"#;
#[derive(Clone, Serialize)]
struct Rate {
    requests: i64,
    window_ms: i64,
    burst: i64,
}
#[derive(Clone, Default)]
pub(super) struct SlotPlan {
    pub capacities: Vec<(String, i64)>,
    rates: Vec<(String, Rate)>,
    pub token_key: Option<String>,
    pub tokens: Option<TokenEnvelope>,
    pub rate_dedupe: Option<(String, i64)>,
}
#[derive(Serialize)]
struct Payload<'a> {
    capacities: Vec<i64>,
    rates: Vec<&'a Rate>,
    member: &'a str,
    logical_id: &'a str,
    dedupe_ttl_ms: i64,
    ttl_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens: Option<&'a TokenEnvelope>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    Capacity,
    Rate(i64),
    Tokens,
    Unavailable,
    Duplicate,
}
pub(super) fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub(super) fn tenant_tag(tenant: &str) -> String {
    format!("{{{}}}", hex(tenant))
}
impl SlotPlan {
    pub fn rate(&mut self, key: String, value: &RateLimit) -> Result<(), Refusal> {
        // Lua doubles exactly represent these integer values. A malformed
        // configured policy fails closed instead of becoming an unlimited tier.
        let window_ms = value.window.checked_mul(1000).ok_or(Refusal::Unavailable)?;
        if value.requests <= 0
            || window_ms <= 0
            || value.burst < 0
            || value.requests > 1_000_000_000
            || window_ms > 1_000_000_000_000
            || value.burst > 1_000_000_000
        {
            return Err(Refusal::Unavailable);
        }
        let debt_us =
            i128::from(window_ms) * 1000 * (i128::from(value.requests) + i128::from(value.burst))
                / i128::from(value.requests);
        if i128::from(value.requests) > i128::from(window_ms) * 1000 || debt_us > 10_000_000_000_000
        {
            return Err(Refusal::Unavailable);
        }
        self.rates.push((
            key,
            Rate {
                requests: value.requests,
                window_ms,
                burst: value.burst,
            },
        ));
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.capacities.is_empty() && self.rates.is_empty() && self.tokens.is_none()
    }
    pub async fn acquire(
        &self,
        connection: &ConnectionManager,
        member: &str,
        ttl_ms: i64,
    ) -> Result<i64, Refusal> {
        self.acquire_logical(connection, member, ttl_ms, member)
            .await
    }
    pub async fn acquire_logical(
        &self,
        connection: &ConnectionManager,
        member: &str,
        ttl_ms: i64,
        logical_id: &str,
    ) -> Result<i64, Refusal> {
        if self.is_empty() {
            return Ok(0);
        }
        if self
            .capacities
            .iter()
            .any(|(_, limit)| *limit <= 0 || *limit > 9_007_199_254_740_991)
        {
            return Err(Refusal::Unavailable);
        }
        if let Some(e) = &self.tokens {
            let values = [
                e.estimate,
                e.pending,
                e.daily_used,
                e.monthly_used,
                e.daily_limit.unwrap_or(0),
                e.monthly_limit.unwrap_or(0),
            ];
            if values
                .into_iter()
                .any(|n| !(0..=1_000_000_000_000_000).contains(&n))
            {
                return Err(Refusal::Unavailable);
            }
        }
        let payload = serde_json::to_string(&Payload {
            capacities: self.capacities.iter().map(|(_, n)| *n).collect(),
            rates: self.rates.iter().map(|(_, r)| r).collect(),
            member,
            logical_id,
            dedupe_ttl_ms: self.rate_dedupe.as_ref().map_or(0, |(_, ttl)| *ttl),
            ttl_ms,
            tokens: self.tokens.as_ref(),
        })
        .map_err(|_| Refusal::Unavailable)?;
        // EVAL directly: Script::invoke_async may add NOSCRIPT/EVALSHA
        // round trips on a cold node, violating the admission call bound.
        let has_dedupe = !self.rates.is_empty() && self.rate_dedupe.is_some();
        let mut invocation = redis::cmd("EVAL");
        invocation.arg(ACQUIRE).arg(
            self.capacities.len()
                + self.rates.len()
                + usize::from(self.token_key.is_some())
                + usize::from(has_dedupe),
        );
        for (key, _) in &self.capacities {
            invocation.arg(key);
        }
        for (key, _) in &self.rates {
            invocation.arg(key);
        }
        if has_dedupe {
            invocation.arg(&self.rate_dedupe.as_ref().expect("dedupe key").0);
        }
        if let Some(key) = &self.token_key {
            invocation.arg(key);
        }
        invocation.arg(payload);
        crate::rate_limits::GATEWAY_HOT_PATH_METRICS.record_admission_attempt();
        let response: Vec<i64> = tokio::time::timeout(
            Duration::from_secs(2),
            invocation.query_async(&mut connection.clone()),
        )
        .await
        .map_err(|_| Refusal::Unavailable)?
        .map_err(|_| Refusal::Unavailable)?;
        match response.as_slice() {
            [1, 0, used] if *used >= 0 => Ok(*used),
            [0, 1, _] => Err(Refusal::Capacity),
            [0, 2, retry] => Err(Refusal::Rate((*retry).max(1))),
            [0, 3, _] => Err(Refusal::Tokens),
            _ => Err(Refusal::Unavailable),
        }
    }
    pub async fn renew(
        &self,
        connection: &ConnectionManager,
        member: &str,
        ttl_ms: i64,
    ) -> Result<(), ()> {
        if self.capacities.is_empty() {
            return Ok(());
        }
        let script = Script::new(RENEW);
        let mut invocation = script.prepare_invoke();
        for (key, _) in &self.capacities {
            invocation.key(key);
        }
        invocation.arg(member).arg(ttl_ms);
        let valid: i64 = tokio::time::timeout(
            Duration::from_secs(2),
            invocation.invoke_async(&mut connection.clone()),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        if valid == 1 { Ok(()) } else { Err(()) }
    }
    pub async fn release(&self, connection: &ConnectionManager, member: &str) -> Result<(), ()> {
        if self.capacities.is_empty() && self.token_key.is_none() {
            return Ok(());
        }
        let script = Script::new(RELEASE);
        let mut invocation = script.prepare_invoke();
        for (key, _) in &self.capacities {
            invocation.key(key);
        }
        if let Some(key) = &self.token_key {
            invocation.key(key);
        }
        invocation.arg(member).arg(self.capacities.len());
        let _: i64 = tokio::time::timeout(
            Duration::from_secs(2),
            invocation.invoke_async(&mut connection.clone()),
        )
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tenant_tags_are_injective_and_not_client_braces() {
        assert_ne!(tenant_tag("a:b"), tenant_tag("a_b"));
        assert_ne!(tenant_tag("t"), tenant_tag(" t"));
        assert_eq!(tenant_tag("{foreign}").matches('{').count(), 1);
    }
    #[test]
    fn invalid_rate_policy_fails_closed() {
        let mut plan = SlotPlan::default();
        for (requests, window, burst) in [
            (0, 1, 0),
            (1, 0, 0),
            (1, 1, -1),
            (i64::MAX, 1, 0),
            (1, i64::MAX, 0),
        ] {
            assert!(
                plan.rate(
                    "k".into(),
                    &RateLimit {
                        requests,
                        window,
                        burst,
                        strategy: "gcra".into()
                    }
                )
                .is_err()
            );
        }
    }
}
