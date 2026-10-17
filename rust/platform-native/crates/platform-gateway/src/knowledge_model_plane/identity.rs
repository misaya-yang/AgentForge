//! Workload authentication and current account authority are separate checks.
use super::*;
use ai_platform_auth::accounts::PgAccounts;
use ai_platform_service_primitives::{knowledge_auth::GatewayAuth, knowledge_types::UserContext};
use axum::http::HeaderMap;
/// Ed25519 verifies a Knowledge-owned workload key, never a Gateway lease key.
/// HMAC is an explicit, bounded compatibility window, not workload identity v3.
pub enum WorkloadAuthentication {
    Ed25519 {
        keys: signature::PublicKeyring,
    },
    Transition {
        keys: signature::PublicKeyring,
        legacy: Arc<GatewayAuth>,
        accept_until_ms: u64,
    },
}
pub struct WorkloadVerifier {
    mode: WorkloadAuthentication,
    pool: PgPool,
}
impl WorkloadVerifier {
    pub fn new(pool: PgPool, mode: WorkloadAuthentication) -> PlaneResult<Self> {
        match &mode {
            WorkloadAuthentication::Ed25519 { keys } => keys
                .validate()
                .map_err(|_| PlaneError::new(503, "knowledge_workload_keyring_invalid"))?,
            WorkloadAuthentication::Transition {
                keys,
                legacy,
                accept_until_ms,
            } => {
                keys.validate()
                    .map_err(|_| PlaneError::new(503, "knowledge_workload_keyring_invalid"))?;
                if *accept_until_ms <= now_ms()
                    || *accept_until_ms > now_ms() + 86_400_000
                    || legacy.version != "v2"
                    || legacy.caller_service != "knowledge-service"
                    || legacy.audience != "gateway"
                    || legacy.allow_anonymous
                    || legacy.allowed_path_prefixes != [wire::RESERVE_PATH]
                {
                    return Err(PlaneError::new(
                        503,
                        "knowledge_workload_transition_invalid",
                    ));
                }
            }
        }
        Ok(Self { mode, pool })
    }
    pub(super) async fn verify(
        &self,
        headers: &HeaderMap,
        path: &str,
        body: &[u8],
        call: &ModelCall,
    ) -> PlaneResult<()> {
        let denied = || PlaneError::new(401, "knowledge_workload_unauthorized");
        if let Some(token) = headers
            .get("x-knowledge-workload")
            .and_then(|v| v.to_str().ok())
        {
            let keys = match &self.mode {
                WorkloadAuthentication::Ed25519 { keys }
                | WorkloadAuthentication::Transition { keys, .. } => keys,
            };
            let envelope: signature::SignedEnvelope =
                signature::decode(signature::ENVELOPE_PREFIX, token).map_err(|_| denied())?;
            keys.verify(&envelope, "knowledge-model-workload", now_ms())
                .map_err(|_| denied())?;
            verify_binding(&envelope.claims, path, body, call)?;
            if envelope.expires_at_ms.saturating_sub(envelope.issued_at_ms) > 60_000 {
                return Err(denied());
            }
            let nonce = text(&envelope.claims, "nonce");
            if nonce.is_empty() || nonce.len() > 128 {
                return Err(denied());
            }
            let used=sqlx::query("INSERT INTO gateway.knowledge_model_workload_nonces(key_id,nonce,expires_at) VALUES($1,$2,to_timestamp($3::double precision/1000)) ON CONFLICT DO NOTHING")
    .bind(&envelope.key_id).bind(nonce).bind(envelope.expires_at_ms as i64).execute(&self.pool).await?.rows_affected();
            if used != 1 {
                return Err(denied());
            }
            return Ok(());
        }
        let WorkloadAuthentication::Transition {
            legacy,
            accept_until_ms,
            ..
        } = &self.mode
        else {
            return Err(denied());
        };
        if now_ms() >= *accept_until_ms {
            return Err(denied());
        }
        let header = headers
            .get("x-gateway-signature")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(denied)?;
        legacy
            .verify(header, "POST", path, "", body, headers)
            .await
            .map_err(|_| denied())?;
        if headers.get("x-user-id").and_then(|v| v.to_str().ok())
            != Some(call.scope.actor.user_id.as_str())
            || headers.get("x-tenant-id").and_then(|v| v.to_str().ok())
                != Some(call.scope.actor.tenant_id.as_str())
        {
            return Err(denied());
        }
        Ok(())
    }
}
fn verify_binding(claims: &Value, path: &str, body: &[u8], call: &ModelCall) -> PlaneResult<()> {
    if claims["caller_service"] != "knowledge-service"
        || claims["audience"] != "gateway"
        || claims["method"] != "POST"
        || claims["path"] != path
        || claims["body_sha256"] != hash(body)
        || claims["tenant_id"] != call.scope.actor.tenant_id
        || claims["user_id"] != call.scope.actor.user_id
    {
        return Err(PlaneError::new(401, "knowledge_workload_binding_invalid"));
    }
    Ok(())
}
impl KnowledgeModelPlane {
    pub(super) async fn current_actor(&self, call: &ModelCall) -> PlaneResult<UserContext> {
        let supplied = &call.scope.actor;
        let store = PgAccounts::new(self.pool.clone());
        let account = store
            .by_id(&supplied.user_id)
            .await
            .map_err(|_| PlaneError::new(503, "knowledge_actor_authority_unavailable"))?
            .ok_or(PlaneError::new(403, "knowledge_actor_forbidden"))?;
        if account.tenant_id != supplied.tenant_id || account.status != "active" {
            return Err(PlaneError::new(403, "knowledge_actor_forbidden"));
        }
        let authority = store
            .current_authority(&account.user_id, &account.tenant_id)
            .await
            .map_err(|_| PlaneError::new(403, "knowledge_actor_forbidden"))?;
        Ok(UserContext {
            user_id: account.user_id,
            tenant_id: account.tenant_id,
            user_tier: authority.tier,
            user_type: "user".into(),
            roles: authority.roles,
            is_authenticated: true,
        })
    }
    pub(super) async fn authorize(&self, call: &ModelCall) -> PlaneResult<()> {
        let actor = self.current_actor(call).await?;
        // A signed hop establishes attribution, but current account/model/dataset
        // authority is re-read for reserve AND execute. Body roles grant nothing.
        let snapshot = self.authorization_snapshot(call).await?;
        let allowed = match text(&snapshot["model"], "access_level") {
            "public" => true,
            "premium" => {
                actor.tenant_admin()
                    || ["premium", "enterprise"].contains(&actor.user_tier.as_str())
            }
            "admin" => actor.tenant_admin(),
            _ => false,
        };
        if !allowed {
            return Err(PlaneError::new(403, "knowledge_model_access_denied"));
        }

        // Batch/judge operations without a dataset remain actor-scoped, never system.
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workload_binding_rejects_forged_context() {
        let call = super::super::tests::call();
        let body = b"{}";
        let base = json!({"caller_service":"knowledge-service","audience":"gateway","method":"POST","path":wire::RESERVE_PATH,"body_sha256":hash(body),"tenant_id":"t","user_id":"u"});
        assert!(verify_binding(&base, wire::RESERVE_PATH, body, &call).is_ok());
        for key in [
            "caller_service",
            "audience",
            "method",
            "path",
            "body_sha256",
            "tenant_id",
            "user_id",
        ] {
            let mut forged = base.clone();
            forged[key] = json!("forged");
            assert!(verify_binding(&forged, wire::RESERVE_PATH, body, &call).is_err())
        }
    }
}
