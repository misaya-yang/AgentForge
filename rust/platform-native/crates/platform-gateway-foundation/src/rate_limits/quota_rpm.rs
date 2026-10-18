//! Shared quota RPM. Server time and one atomic GCRA operation prevent replica
//! multiplication and client-clock manipulation. Failures never fall back locally.
use super::{MinuteQuotaCheck, QuotaRpmLimiter, RateLimitError, normalize_limit};
use redis::{Script, aio::ConnectionManager};
use std::time::Duration;

const CHECK: &str = r#"
local limit = tonumber(ARGV[1])
local clock = redis.call('TIME')
local now = tonumber(clock[1]) * 1000000 + tonumber(clock[2])
local interval = 60000000 / limit
local tat = math.max(tonumber(redis.call('GET', KEYS[1]) or now), now)
local next = tat + interval
local allowed_at = next - 60000000
if allowed_at > now then
  return {0, math.ceil((tat - now) / interval), math.max(1, math.ceil((allowed_at - now) / 1000000))}
end
redis.call('SET', KEYS[1], string.format('%.0f', next), 'PX', math.max(1, math.ceil((next - now) / 1000)))
return {1, math.ceil((next - now) / interval), 0}
"#;

// Hex encodes every byte: braces cannot inject a Cluster tag and distinct
// tenant/user values cannot collapse through delimiter replacement.
fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn key(tenant: &str, user: &str) -> String {
    format!("gateway:quota-rpm:{{{}}}:{}", hex(tenant), hex(user))
}
impl QuotaRpmLimiter {
    pub fn shared_authority_configured(&self) -> bool {
        self.shared.is_some()
    }

    pub fn shared(connection: ConnectionManager) -> Self {
        Self {
            shared: Some(connection),
            ..Self::default()
        }
    }

    /// Preserves canonical static-limit precedence before charging shared RPM.
    pub async fn check_quota_shared(
        &self,
        tenant: &str,
        user: &str,
        quota: Option<&crate::billing::UserQuota>,
        estimated_tokens: i64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::billing::QuotaCheckResult, RateLimitError> {
        let mut minute_limit = None;
        let preliminary = crate::billing::check_quota(quota, estimated_tokens, now, |limit| {
            minute_limit = Some(limit);
            MinuteQuotaCheck::default()
        })
        .map_err(|_| RateLimitError::InvalidWindow)?;
        if let Some(limit) = minute_limit {
            let minute =
                match crate::capacity_admission::atomic::quota_minute_decision(tenant, user) {
                    Some(decision) => decision,
                    None => self.check_shared(tenant, user, Some(limit)).await?,
                };
            crate::billing::check_quota(quota, estimated_tokens, now, |_| minute)
                .map_err(|_| RateLimitError::InvalidWindow)
        } else {
            Ok(preliminary)
        }
    }

    /// The shared production path rejects finite quotas without a backend.
    /// `check`/`check_at` remain explicit legacy local-only APIs.
    pub async fn check_shared(
        &self,
        tenant: &str,
        user: &str,
        limit: Option<i64>,
    ) -> Result<MinuteQuotaCheck, RateLimitError> {
        let Some(limit) = normalize_limit(limit) else {
            return Ok(MinuteQuotaCheck::default());
        };
        // Larger rates lose microsecond GCRA precision; reject configuration.
        if limit > 60_000_000 || tenant.is_empty() || user.is_empty() {
            return Err(RateLimitError::InvalidWindow);
        }
        let mut connection = self
            .shared
            .clone()
            .ok_or(RateLimitError::RedisUnavailable)?;
        let result: Vec<i64> = tokio::time::timeout(Duration::from_secs(2), async {
            Script::new(CHECK)
                .key(key(tenant, user))
                .arg(limit)
                .invoke_async(&mut connection)
                .await
        })
        .await
        .map_err(|_| RateLimitError::RedisUnavailable)?
        .map_err(|_| RateLimitError::RedisUnavailable)?;
        match result.as_slice() {
            [allowed, used, retry] if (0..=1).contains(allowed) && *used >= 0 && *retry >= 0 => {
                Ok(MinuteQuotaCheck {
                    allowed: *allowed == 1,
                    used: *used,
                    retry_after_seconds: *retry,
                })
            }
            _ => Err(RateLimitError::InvalidRedisReply),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_preserve_tenant_and_subject_identity() {
        assert_ne!(key("a:b", "c"), key("a", "b:c"));
        assert_ne!(key("tenant", "a:b"), key("tenant", "a_b"));
        assert_ne!(key("tenant", "u"), key(" tenant", "u"));
        let value = key("{foreign}", "}");
        assert_eq!(value.matches('{').count(), 1);
        assert_eq!(value.matches('}').count(), 1);
        assert_eq!(
            key("tenant", "one").split('}').next(),
            key("tenant", "two").split('}').next()
        );
    }
    #[tokio::test]
    async fn finite_quota_without_shared_backend_fails_closed() {
        let limiter = QuotaRpmLimiter::default();
        assert_eq!(
            limiter.check_shared("t", "u", Some(1)).await.unwrap_err(),
            RateLimitError::RedisUnavailable
        );
        assert!(limiter.check_shared("t", "u", None).await.unwrap().allowed);
        assert_eq!(
            limiter
                .check_shared("t", "u", Some(60_000_001))
                .await
                .unwrap_err(),
            RateLimitError::InvalidWindow
        );
    }
    #[tokio::test]
    async fn canonical_static_refusal_does_not_require_or_charge_rpm() {
        let limiter = QuotaRpmLimiter::default();
        let mut quota = crate::billing::UserQuota::new("t", "u");
        quota.requests_per_minute = Some(1);
        quota.is_blocked = true;
        let refused = limiter
            .check_quota_shared("t", "u", Some(&quota), 10, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(refused.status, crate::billing::QuotaStatus::Blocked);
        quota.is_blocked = false;
        assert_eq!(
            limiter
                .check_quota_shared("t", "u", Some(&quota), 10, chrono::Utc::now())
                .await
                .unwrap_err(),
            RateLimitError::RedisUnavailable
        );
        quota.daily_token_limit = Some(1);
        let refused = limiter
            .check_quota_shared("t", "u", Some(&quota), 10, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(refused.status, crate::billing::QuotaStatus::Exceeded);
    }
}
