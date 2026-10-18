//! External agents are registered platform tools. Tenant tool name/category
//! constraints intersect with capability admission and the durable call budget.
use super::*;
use ai_platform_auth::cedar::resources::{
    self, ObjectAction, ObjectProfile, ObjectResource, ObjectSubject, ResourceKind,
};
use std::collections::BTreeSet;
fn names(policy: &Value, key: &str) -> Result<BTreeSet<String>, AgentError> {
    let Some(value) = policy.get(key).filter(|v| !v.is_null()) else {
        return Ok(BTreeSet::new());
    };
    let values = value
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or_else(|| err(503, "EXTERNAL_AGENT_POLICY_UNAVAILABLE"))?;
    values
        .iter()
        .map(|v| {
            let v = v
                .as_str()
                .map(str::trim)
                .filter(|v| {
                    !v.is_empty()
                        && v.len() <= 128
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                })
                .ok_or_else(|| err(503, "EXTERNAL_AGENT_POLICY_UNAVAILABLE"))?;
            Ok(v.to_owned())
        })
        .collect()
}
fn facts(
    actor: &Actor,
    agent: &str,
    policy: &Value,
) -> Result<(ObjectSubject, ObjectResource, bool), AgentError> {
    let name = format!("external_agent_{agent}");
    let allowed = names(policy, "allowed_tools")?;
    let blocked = names(policy, "blocked_tools")?;
    let categories = names(policy, "allowed_categories")?;
    let relationships = [
        (
            "tenant_names",
            allowed.is_empty() || allowed.contains(&name),
        ),
        ("tenant_denials", !blocked.contains(&name)),
        (
            "tenant_categories",
            categories.is_empty() || categories.contains("external_agents"),
        ),
    ];
    let subject = ObjectSubject {
        tenant_id: actor.tenant_id.clone(),
        user_id: actor.user_id.clone(),
        roles: actor.roles.clone(),
        tier: actor.tier.clone(),
        ..Default::default()
    };
    let mut resource = ObjectResource::new(ResourceKind::Tool, &name, &actor.tenant_id);
    resource.profile = ObjectProfile::ToolVisibility;
    resource.aliases.insert(name);
    for (name, satisfied) in relationships {
        resource.required_scopes.insert(name.into());
        if satisfied {
            resource.satisfied_scopes.insert(name.into());
        }
    }
    Ok((
        subject,
        resource,
        relationships.iter().all(|(_, allowed)| *allowed),
    ))
}
pub(super) async fn authorize(
    server: &Server,
    actor: &Actor,
    name: &str,
) -> Result<(), AgentError> {
    let policy:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('allowed_tools',allowed_tools,'blocked_tools',blocked_tools,'allowed_categories',allowed_categories) FROM tenant_tool_policies WHERE tenant_id=$1").bind(&actor.tenant_id).fetch_optional(&server.agent.repository.pool).await.map_err(|_|err(503,"EXTERNAL_AGENT_POLICY_UNAVAILABLE"))?;
    let (s, r, legacy) = facts(actor, name, &policy.unwrap_or_else(|| json!({})))?;
    if !resources::compare_current(&s, &r, ObjectAction::Invoke, legacy) || !legacy {
        return Err(err(403, "EXTERNAL_AGENT_TOOL_DENIED"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tenant_tool_denials_and_categories_apply_even_to_administrators() {
        struct Audit;
        impl ai_platform_auth::cedar::DecisionAudit for Audit {
            fn ready(&self) -> bool {
                true
            }
            fn record(
                &self,
                _: ai_platform_auth::cedar::DecisionLog,
            ) -> Result<(), ai_platform_auth::cedar::PolicyError> {
                Ok(())
            }
        }
        let point = ai_platform_auth::cedar::DecisionPoint::new(
            ai_platform_auth::cedar::Configuration {
                mode: ai_platform_auth::cedar::Mode::Shadow,
                ..Default::default()
            },
            &Rbac::new(Default::default()),
            Arc::new(Audit),
        )
        .unwrap();
        let actor = Actor {
            tenant_id: "t".into(),
            user_id: "u".into(),
            roles: vec!["admin".into()],
            tier: "admin".into(),
        };
        for (policy, expected) in [
            (json!({}), true),
            (json!({"allowed_tools":["external_agent_peer"]}), true),
            (json!({"allowed_tools":["other"]}), false),
            (json!({"blocked_tools":["external_agent_peer"]}), false),
            (json!({"allowed_categories":["external_agents"]}), true),
            (json!({"allowed_categories":["knowledge"]}), false),
        ] {
            let (subject, r, legacy) = facts(&actor, "peer", &policy).unwrap();
            assert_eq!(
                point
                    .evaluate_resource(&subject, &r, ObjectAction::Invoke)
                    .unwrap(),
                legacy
            );
            assert_eq!(legacy, expected);
            assert_eq!(r.required_scopes.is_subset(&r.satisfied_scopes), expected);
        }
        assert!(facts(&actor, "peer", &json!({"blocked_tools":"*"})).is_err());
    }
}
