//! Actor provenance for RAGAS. Caller authority comes from HTTP admission or a fenced outbox run.
use super::super::{
    common::{CompatError, CompatUser, text},
    eval_executor::EvaluatorExecutor,
};
use ai_platform_service_primitives::{knowledge_model::ModelScope, knowledge_types::UserContext};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn scope(user: &CompatUser) -> Result<ModelScope, CompatError> {
    let actor = UserContext {
        user_id: user.user_id.clone(),
        tenant_id: user.tenant_id.clone(),
        user_tier: user.tier.clone(),
        user_type: "user".into(),
        roles: user.roles.clone(),
        is_authenticated: user.authenticated,
    };
    let scope = ModelScope {
        actor,
        dataset_id: None,
        document_id: None,
        execution_id: None,
    };
    identity_headers(&scope)?;
    Ok(scope)
}

pub(super) fn identity_headers(
    scope: &ModelScope,
) -> Result<BTreeMap<&'static str, String>, CompatError> {
    let actor = &scope.actor;
    if actor.anonymous() || actor.user_id.trim().is_empty() || actor.tenant_id.trim().is_empty() {
        return Err(CompatError::new(403, "Authenticated RAGAS actor required"));
    }
    let headers = BTreeMap::from([
        ("x-user-id", actor.user_id.clone()),
        ("x-tenant-id", actor.tenant_id.clone()),
        ("x-user-tier", actor.user_tier.clone()),
        ("x-user-type", actor.user_type.clone()),
        (
            "x-user-roles",
            serde_json::to_string(&actor.roles)
                .map_err(|_| CompatError::new(422, "Invalid RAGAS actor"))?,
        ),
    ]);
    if headers
        .values()
        .any(|v| axum::http::HeaderValue::from_str(v).is_err())
    {
        return Err(CompatError::new(422, "Invalid RAGAS actor headers"));
    }
    Ok(headers)
}

impl EvaluatorExecutor {
    pub(crate) async fn ragas_actor_scope(&self, job: &Value) -> Result<ModelScope, CompatError> {
        let delegation = self.outbox.resolve_actor(job).await?;
        if text(&delegation, "tenant_id") != text(job, "tenant_id")
            || text(&delegation, "job_id") != text(job, "job_id")
            || text(&delegation, "run_id") != text(&job["payload"], "run_id")
        {
            return Err(CompatError::new(403, "AGENT_EVAL_DELEGATION_INVALID"));
        }
        scope(&CompatUser {
            user_id: text(&delegation, "subject").into(),
            tenant_id: text(job, "tenant_id").into(),
            tier: "normal".into(),
            authenticated: true,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_anonymous_and_header_injection_actors_fail_closed() {
        let user = CompatUser {
            user_id: "member".into(),
            tenant_id: "tenant".into(),
            tier: "normal".into(),
            authenticated: true,
            ..Default::default()
        };
        assert!(scope(&user).is_ok());
        for bad in [
            CompatUser {
                authenticated: false,
                ..user.clone()
            },
            CompatUser {
                tenant_id: String::new(),
                ..user.clone()
            },
            CompatUser {
                user_id: "member\r\nx-forged: true".into(),
                ..user
            },
        ] {
            assert!(scope(&bad).is_err());
        }
    }
}
