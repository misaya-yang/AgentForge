//! Current authority for the supported JWT-only durable /submit path.
//! Persist only a non-bearer session identifier and password revision in the
//! Gateway-only delivery ledger. Never persist raw JWTs, API keys or hashes.
use crate::task_manager::TaskEnvelope;
use ai_platform_auth::{
    accounts::{PgAccounts, StoreError},
    service::Principal,
    sessions::RedisSessions,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskActor {
    session_id: String,
    credential_revision: Option<String>,
    expires_at: u64,
}
impl TaskActor {
    pub fn from_session(principal: &Principal) -> Self {
        Self {
            session_id: principal.jti.clone(),
            expires_at: principal.expires_at,
            credential_revision: principal.account.password_changed_at.map(|time| time.to_rfc3339()),
        }
    }
    pub(crate) fn valid(&self) -> bool {
        self.expires_at > 0 && uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.credential_revision.as_ref().is_none_or(|value| {
                chrono::DateTime::parse_from_rfc3339(value).is_ok()
            })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskActorError { Revoked, Unavailable }
pub type ActorFuture<'a> = Pin<Box<dyn Future<Output=Result<Vec<String>,TaskActorError>> + Send + 'a>>;
pub trait TaskActorValidator: Send + Sync {
    /// Return current roles; the admitted snapshot never grants execution.
    fn revalidate<'a>(&'a self, actor: &'a TaskActor, envelope: &'a TaskEnvelope) -> ActorFuture<'a>;
}
pub struct PgTaskActorValidator {
    accounts: PgAccounts,
    sessions: RedisSessions,
}
impl PgTaskActorValidator {
    pub fn new(pool: sqlx::PgPool, redis: redis::aio::ConnectionManager) -> Self {
        Self { accounts: PgAccounts::new(pool), sessions: RedisSessions::new(redis) }
    }
}
fn session_matches(actor: &TaskActor, session: &serde_json::Value, user: &str, revision: &Option<String>, now: u64) -> bool {
    actor.valid() && now < actor.expires_at
        && session.get("user_id").and_then(serde_json::Value::as_str) == Some(user)
        && actor.credential_revision == *revision
        // Older records lacking the precise revision cannot authorize durable
        // execution. A fresh login provides it; no timestamp approximation.
        && session.get("credential_revision") == Some(&serde_json::json!(revision))
}
impl TaskActorValidator for PgTaskActorValidator {
    fn revalidate<'a>(&'a self, actor: &'a TaskActor, envelope: &'a TaskEnvelope) -> ActorFuture<'a> {
        Box::pin(async move {
            let now = u64::try_from(Utc::now().timestamp()).map_err(|_|TaskActorError::Unavailable)?;
            if !actor.valid() || now >= actor.expires_at { return Err(TaskActorError::Revoked); }
            // Absolute verified JWT expiry and Redis allowlist are both required.
            // Redis TTL alone can outlive the JWT by the login save delay.
            // Missing/expired/logout
            // records fail closed; Redis failure never falls back to roles.
            let session = self.sessions.record(&actor.session_id).await
                .map_err(|_| TaskActorError::Unavailable)?.ok_or(TaskActorError::Revoked)?;
            let account = self.accounts.by_id(&envelope.request.user_id).await
                .map_err(|_| TaskActorError::Unavailable)?.ok_or(TaskActorError::Revoked)?;
            let revision = account.password_changed_at.map(|time| time.to_rfc3339());
            if account.tenant_id != envelope.request.tenant_id || account.status != "active"
                || account.locked_until.is_some_and(|until| until > Utc::now())
                || !session_matches(actor, &session, &account.user_id, &revision, now) {
                return Err(TaskActorError::Revoked);
            }
            // Reuse the canonical current-account/tenant/RBAC reader. It checks
            // both active states in a repeatable-read transaction.
            let authority = self.accounts.authority(&account.user_id, &account.tenant_id).await
                .map_err(|error| if error == StoreError::Changed { TaskActorError::Revoked } else { TaskActorError::Unavailable })?;
            let finished = u64::try_from(Utc::now().timestamp()).map_err(|_|TaskActorError::Unavailable)?;
            if finished >= actor.expires_at { return Err(TaskActorError::Revoked); }
            Ok(authority.roles)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_actor_requires_matching_session_and_exact_password_revision() {
        let actor = TaskActor { session_id: uuid::Uuid::new_v4().to_string(), credential_revision: None, expires_at: 1000 };
        assert!(session_matches(&actor, &serde_json::json!({"user_id":"user","credential_revision":null}), "user", &None, 100));
        for session in [serde_json::json!(null), serde_json::json!({"user_id":"user"}), serde_json::json!({"user_id":"other","credential_revision":null})] {
            assert!(!session_matches(&actor, &session, "user", &None, 100));
        }
        let changed = Some("2026-10-12T11:00:00+00:00".into());
        assert!(!session_matches(&actor, &serde_json::json!({"user_id":"user","credential_revision":changed}), "user", &changed, 100));
    }
    #[test]
    fn task_actor_rejects_expired_jwt_even_when_redis_record_remains() {
        let actor = TaskActor {session_id:uuid::Uuid::new_v4().to_string(), credential_revision:None, expires_at:1000};
        let record=serde_json::json!({"user_id":"user","credential_revision":null});
        assert!(session_matches(&actor,&record,"user",&None,999));
        assert!(!session_matches(&actor,&record,"user",&None,1000));
        assert!(!session_matches(&actor,&record,"user",&None,1001));
        assert!(serde_json::from_value::<TaskActor>(serde_json::json!({"session_id":uuid::Uuid::new_v4().to_string(),"credential_revision":null})).is_err());
    }
    #[test]
    fn task_actor_payload_is_strict_and_not_a_bearer_credential() {
        assert!(serde_json::from_value::<TaskActor>(serde_json::json!({"session_id":"bad","credential_revision":null,"token":"secret"})).is_err());
        let invalid = TaskActor { session_id:"bad".into(), credential_revision:None, expires_at:1000 };
        assert!(!invalid.valid());
    }
}
