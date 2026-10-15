//! Gateway-owned SDK scope index and durable deletion outbox. No SDK state or keys.
//! Lock order: durable barrier, canonical generic session row, then scope rows.
use reqwest::{Client, Url, header::HeaderValue};
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug)]
pub enum CleanupError {
    Database,
    SessionUnavailable,
    WorkerConfiguration,
    Runtime(u16),
}
impl From<sqlx::Error> for CleanupError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
#[derive(Clone, Serialize)]
pub struct SdkScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub service_id: String,
    pub framework: String,
}
impl SdkScope {
    fn valid(&self) -> bool {
        [
            &self.tenant_id,
            &self.user_id,
            &self.session_id,
            &self.service_id,
        ]
        .iter()
        .all(|v| !v.trim().is_empty() && v.len() <= 255)
            && matches!(
                self.framework.as_str(),
                "openai_agents" | "claude_agent_sdk"
            )
    }
}
fn service_claim(bound: Option<&str>, requested: &str) -> Result<bool, CleanupError> {
    match bound {
        None | Some("") => Ok(true),
        Some(existing) if existing == requested => Ok(false),
        _ => Err(CleanupError::SessionUnavailable),
    }
}
/// Called only by authenticated SDK adapter, before sending any private worker run.
/// A deleted service cannot erase this immutable server-owned scope evidence.
pub async fn admit(pool: &PgPool, scope: &SdkScope) -> Result<(), CleanupError> {
    if !scope.valid() {
        return Err(CleanupError::SessionUnavailable);
    }
    let mut tx = pool.begin().await?;
    // Reject unowned/nonexistent sessions before any barrier mutation. Recheck
    // under the canonical row lock after acquiring the common lifecycle fence.
    let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assistant.sessions WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND status <> 'deleted')")
        .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_one(&mut *tx).await?;
    if !owned || !ai_platform_gateway_foundation::session_storage::lock_session_barrier(&mut *tx,Some(&scope.tenant_id),Some(&scope.user_id),&scope.session_id).await? {
        return Err(CleanupError::SessionUnavailable);
    }
    let row = sqlx::query("SELECT service_id FROM assistant.sessions WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND status <> 'deleted' AND (expires_at IS NULL OR expires_at > now()) FOR UPDATE")
        .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(&mut *tx).await?
        .ok_or(CleanupError::SessionUnavailable)?;
    let bound: Option<String> = row.try_get("service_id")?;
    let claim = service_claim(bound.as_deref(), &scope.service_id)?;
    // The generic manager only caches first-use binding. SDK needs a durable claim
    // under the owned row lock; never rebind another nonempty service.
    if claim {
        sqlx::query("UPDATE assistant.sessions SET service_id=$4,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND (service_id IS NULL OR service_id='')")
            .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(&scope.service_id).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO gateway.sdk_session_scopes(tenant_id,user_id,session_id,service_id,framework) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
        .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(&scope.service_id).bind(&scope.framework)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
/// Atomic logical deletion plus durable worker intent. Runtime cleanup, when a
/// durable core binding exists, must succeed before either mutation can commit.
/// SDK-only scopes never depend on unrelated Codex Runtime availability.
pub async fn delete_owned<F, Fut>(
    pool: &PgPool,
    tenant: &str,
    user: &str,
    session: &str,
    cleanup_core: F,
) -> Result<(), CleanupError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), CleanupError>>,
{
    let mut tx = pool.begin().await?;
    let authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assistant.sessions WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND status <> 'deleted')")
        .bind(tenant).bind(user).bind(session).fetch_one(&mut *tx).await?;
    if !authorized || !ai_platform_gateway_foundation::session_storage::lock_session_barrier(&mut *tx,Some(tenant),Some(user),session).await? {
        return Err(CleanupError::SessionUnavailable);
    }
    let owned: Option<String> = sqlx::query_scalar("SELECT session_id FROM assistant.sessions WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND status <> 'deleted' FOR UPDATE")
        .bind(tenant).bind(user).bind(session).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Err(CleanupError::SessionUnavailable);
    }
    // Existing server-owned Runtime binding, not mutable service/session metadata.
    let core = sqlx::query("SELECT count(*)>0 AS referenced,COALESCE(bool_or(deleted_at IS NULL),false) AS live FROM assistant.assistant_runtime_threads WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3")
        .bind(tenant).bind(user).bind(session).fetch_one(&mut *tx).await?;
    let core_referenced: bool = core.try_get("referenced")?;
    let core_bound: bool = core.try_get("live")?;
    if core_bound {
        cleanup_core().await?;
    }
    sqlx::query("INSERT INTO gateway.sdk_session_barriers(tenant_id,user_id,session_id,deleting) VALUES($1,$2,$3,true) ON CONFLICT(tenant_id,user_id,session_id) DO UPDATE SET deleting=true")
        .bind(tenant).bind(user).bind(session).execute(&mut *tx).await?;
    sqlx::query("UPDATE gateway.sdk_session_scopes SET state='pending',next_attempt_at=now(),deletion_requested_at=now() WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND state='active'")
        .bind(tenant).bind(user).bind(session).execute(&mut *tx).await?;
    if core_referenced {
        // Existing Runtime receipts require the canonical FK parent to remain.
        sqlx::query("UPDATE assistant.sessions SET status='deleted',updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3")
            .bind(tenant).bind(user).bind(session).execute(&mut *tx).await?;
    } else {
        // A concurrent first Runtime binding waits on this row lock, then its FK
        // must fail after deletion. Leaving an unreferenced tombstone would permit
        // that binding because the Runtime FK does not check canonical status.
        // Unexpected non-Runtime FK failures abort ALL intent; never silently
        // recover via a tombstone/savepoint and reopen this admission race.
        sqlx::query(
            "DELETE FROM assistant.sessions WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3",
        )
        .bind(tenant)
        .bind(user)
        .bind(session)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
/// Checked by the public owned-session seam even when its manager cache is stale.
pub async fn is_deleted(
    pool: &PgPool,
    tenant: &str,
    user: &str,
    session: &str,
) -> Result<bool, CleanupError> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.sdk_session_barriers WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND deleting)")
        .bind(tenant).bind(user).bind(session).fetch_one(pool).await?)
}

fn worker_url(base: &str) -> Result<Url, CleanupError> {
    let url = Url::parse(base).map_err(|_| CleanupError::WorkerConfiguration)?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    if url.host_str().is_none()
        || (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(CleanupError::WorkerConfiguration);
    }
    url.join("v1/sessions")
        .map_err(|_| CleanupError::WorkerConfiguration)
}
struct Worker {
    client: Client,
    url: Url,
    auth: HeaderValue,
}
impl Worker {
    fn configured() -> Result<Self, CleanupError> {
        let base =
            std::env::var("SDK_AGENT_WORKER_URL").map_err(|_| CleanupError::WorkerConfiguration)?;
        let token = std::env::var("SDK_AGENT_WORKER_TOKEN")
            .map_err(|_| CleanupError::WorkerConfiguration)?;
        if token.len() < 32 {
            return Err(CleanupError::WorkerConfiguration);
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| CleanupError::WorkerConfiguration)?;
        auth.set_sensitive(true);
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| CleanupError::WorkerConfiguration)?,
            url: worker_url(&base)?,
            auth,
        })
    }
    async fn delete(&self, scope: &SdkScope) -> bool {
        let Ok(mut response) = self
            .client
            .delete(self.url.clone())
            .header("authorization", self.auth.clone())
            .json(scope)
            .send()
            .await
        else {
            return false;
        };
        if response.status() != reqwest::StatusCode::OK {
            return false;
        }
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if bytes.len() + chunk.len() <= 1024 => {
                    bytes.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => return false,
            }
        }
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .is_ok_and(|v| v.get("deleted") == Some(&serde_json::Value::Bool(true)))
    }
}
/// A bounded retry sweep. Row lock is the lease; interrupted/unknown outcomes retry.
/// A 200 acknowledgement is the only transition to deleted. No raw errors persisted.
pub async fn drain_pending(pool: &PgPool) -> Result<(), CleanupError> {
    // Do not even read optional worker configuration when there are no pending scopes.
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.sdk_session_scopes WHERE state='pending' AND next_attempt_at<=now())")
        .fetch_one(pool).await?;
    if !pending {
        return Ok(());
    }
    let worker = Worker::configured()?;
    for _ in 0..16 {
        let mut tx = pool.begin().await?;
        let row = sqlx::query("SELECT tenant_id,user_id,session_id,service_id,framework FROM gateway.sdk_session_scopes WHERE state='pending' AND next_attempt_at<=now() ORDER BY next_attempt_at LIMIT 1 FOR UPDATE SKIP LOCKED")
            .fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(());
        };
        let scope = SdkScope {
            tenant_id: row.try_get("tenant_id")?,
            user_id: row.try_get("user_id")?,
            session_id: row.try_get("session_id")?,
            service_id: row.try_get("service_id")?,
            framework: row.try_get("framework")?,
        };
        let deleted = worker.delete(&scope).await;
        sqlx::query("UPDATE gateway.sdk_session_scopes SET state=CASE WHEN $6 THEN 'deleted' ELSE 'pending' END,attempts=attempts+1,next_attempt_at=now()+interval '30 seconds',deleted_at=CASE WHEN $6 THEN now() ELSE NULL END WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND service_id=$4 AND framework=$5")
            .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(&scope.service_id).bind(&scope.framework).bind(deleted).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}
/// Bounded operator diagnostic: no identities, prompts or keys leave this projection.
/// `sample_limit_reached` means counts are a lower bound, never a false healthy state.
#[derive(Debug, Serialize)]
pub struct CleanupStatus {
    pub pending_sample: i64,
    pub retrying_sample: i64,
    pub oldest_pending_seconds: Option<i64>,
    pub sample_limit_reached: bool,
}
pub async fn status(pool: &PgPool) -> Result<CleanupStatus, CleanupError> {
    let row = sqlx::query("SELECT count(*)::bigint AS pending, count(*) FILTER(WHERE attempts>0)::bigint AS retrying, floor(extract(epoch FROM now()-min(deletion_requested_at)))::bigint AS oldest FROM (SELECT attempts,deletion_requested_at FROM gateway.sdk_session_scopes WHERE state='pending' ORDER BY next_attempt_at LIMIT 1001) s")
        .fetch_one(pool).await?;
    let pending: i64 = row.try_get("pending")?;
    Ok(CleanupStatus {
        pending_sample: pending,
        retrying_sample: row.try_get("retrying")?,
        oldest_pending_seconds: row.try_get("oldest")?,
        sample_limit_reached: pending == 1001,
    })
}
/// Host-owned task: stop cancels the current bounded HTTP attempt; durable work remains.
pub async fn run(pool: PgPool, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            changed = stop.changed() => { if changed.is_err() || *stop.borrow() { return; } },
            _ = tick.tick() => {
                tokio::select! {
                    _ = stop.changed() => return,
                    _ = drain_pending(&pool) => {},
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_first_use_claims_only_unbound_service() {
        assert!(service_claim(None, "sdk-a").unwrap());
        assert!(service_claim(Some(""), "sdk-a").unwrap());
        assert!(!service_claim(Some("sdk-a"), "sdk-a").unwrap());
        assert!(matches!(
            service_claim(Some("sdk-b"), "sdk-a"),
            Err(CleanupError::SessionUnavailable)
        ));
        assert!(matches!(
            service_claim(Some(" "), "sdk-a"),
            Err(CleanupError::SessionUnavailable)
        ));
    }
    #[test]
    fn cleanup_target_is_only_operator_origin() {
        for base in [
            "https://worker.example",
            "http://127.0.0.1:8097",
            "http://[::1]:8097",
            "http://localhost:8097",
        ] {
            assert_eq!(worker_url(base).unwrap().path(), "/v1/sessions");
        }
        for base in [
            "http://worker.example",
            "https://user:pass@worker.example",
            "https://worker.example/x",
            "https://worker.example/?x=y",
            "https://worker.example/#x",
            "file:///tmp",
        ] {
            assert!(worker_url(base).is_err());
        }
    }
    #[tokio::test]
    async fn deletion_requires_explicit_ack_and_never_sends_provider_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (status, body, expected) in [
            (200, r#"{"deleted":true}"#, true),
            (200, r#"{"deleted":false}"#, false),
            (409, r#"{"deleted":true}"#, false),
            (503, "unavailable", false),
            (200, "not json", false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = worker_url(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0 && bytes.len() + n <= 8192);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let request = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                assert!(request.starts_with("delete /v1/sessions "));
                assert!(request.contains("authorization: bearer "));
                assert!(!request.contains("x-sdk-provider-key"));
                socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
            let worker = Worker {
                client: Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(2))
                    .build()
                    .unwrap(),
                url,
                auth: HeaderValue::from_static("Bearer test-token"),
            };
            let scope = SdkScope {
                tenant_id: "t".into(),
                user_id: "u".into(),
                session_id: "s".into(),
                service_id: "svc".into(),
                framework: "openai_agents".into(),
            };
            assert_eq!(worker.delete(&scope).await, expected);
            server.await.unwrap();
        }
    }
    #[test]
    fn exact_framework_and_bounded_owned_scope() {
        let mut s = SdkScope {
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            service_id: "svc".into(),
            framework: "openai_agents".into(),
        };
        assert!(s.valid());
        s.framework = "claude_agent_sdk".into();
        assert!(s.valid());
        s.framework = "custom".into();
        assert!(!s.valid());
        s.framework = "openai_agents".into();
        s.user_id = " ".into();
        assert!(!s.valid());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 5);
        assert!(json.get("provider_key").is_none());
    }
}
