//! Observable assertions from tests/core/auth/{test_rbac,test_permissions,
//! test_service_access}.py against production authority primitives.
use ai_platform_auth::rbac::{
    Capability::*, Rbac, check_capability, permission_denied_detail, require_capability,
};
use ai_platform_gateway_foundation::service_access_policy::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}
fn rbac() -> Rbac {
    Rbac::new(HashMap::from([
        ("guest".into(), strings(&["console:dashboard:view"])),
        (
            "user".into(),
            strings(&["console:dashboard:view", "conversation:playground:access"]),
        ),
        (
            "viewer".into(),
            strings(&["console:dashboard:view", "console:services:view"]),
        ),
        ("admin".into(), strings(&["admin:*"])),
        ("editor".into(), strings(&["console:services:*"])),
    ]))
}
#[test]
fn rbac_role_union_exact_hierarchical_direct_and_require_contracts() {
    let r = rbac();
    assert_eq!(
        r.permissions_for_roles(&strings(&["guest"])),
        HashSet::from(["console:dashboard:view"])
    );
    assert_eq!(
        r.permissions_for_roles(&strings(&["guest", "user"])),
        HashSet::from(["console:dashboard:view", "conversation:playground:access"])
    );
    assert!(
        r.permissions_for_roles(&strings(&["nonexistent"]))
            .is_empty()
    );
    assert!(r.permissions_for_roles(&[]).is_empty());
    for (roles, permission, expected) in [
        (vec!["user"], "conversation:playground:access", true),
        (vec!["guest"], "conversation:playground:access", false),
        (vec!["admin"], "console:services:edit", true),
        (vec!["admin"], "some:random:permission", true),
        (vec!["editor"], "console:services:view", true),
        (vec!["editor"], "console:services:edit", true),
        (vec!["editor"], "console:dashboard:view", false),
        (
            vec!["conversation:playground:access"],
            "conversation:playground:access",
            true,
        ),
        (
            vec!["guest", "conversation:playground:access"],
            "conversation:playground:access",
            true,
        ),
    ] {
        assert_eq!(
            r.has_permission(&strings(&roles), permission),
            expected,
            "{roles:?} {permission}"
        );
    }
    r.require(&strings(&["user"]), "conversation:playground:access")
        .unwrap();
    assert!(
        r.require(&strings(&["guest"]), "conversation:playground:access")
            .unwrap_err()
            .contains("Missing permission")
    );
}
#[test]
fn capabilities_alias_decision_denial_and_seed_contracts() {
    let r = rbac();
    assert_eq!(
        AgentInvoke.canonical_permission(),
        "conversation:playground:access"
    );
    assert_eq!(
        ServiceListRead.canonical_permission(),
        "console:services:view"
    );
    assert_eq!(
        ServiceConfigWrite.canonical_permission(),
        "console:services:edit"
    );
    assert_eq!(
        AgentInvoke.accepted_permissions(),
        &["conversation:playground:access", "service:invoke"]
    );
    assert!(
        !GatewayMetricsRead
            .accepted_permissions()
            .contains(&"console:dashboard:view")
    );
    for (c, p) in [
        (GatewayProviderConfigRead, "console:providers:edit"),
        (GatewaySkillRead, "console:skills:edit"),
        (GatewayMcpRead, "console:mcp:edit"),
    ] {
        assert!(c.accepted_permissions().contains(&p));
    }
    for (roles, permissions, c, matched) in [
        (
            vec!["user"],
            vec![],
            AgentInvoke,
            Some("conversation:playground:access"),
        ),
        (vec!["viewer"], vec![], AgentInvoke, None),
        (
            vec!["service:invoke"],
            vec![],
            AgentInvoke,
            Some("service:invoke"),
        ),
        (
            vec!["admin"],
            vec![],
            ServiceConfigWrite,
            Some("console:services:edit"),
        ),
        (
            vec!["viewer"],
            vec!["conversation:playground:access"],
            AgentInvoke,
            Some("conversation:playground:access"),
        ),
        (vec!["user"], vec![], GatewayMetricsRead, None),
    ] {
        let d = check_capability(&r, &strings(&roles), &strings(&permissions), c);
        assert_eq!(d.matched_permission, matched);
        assert_eq!(d.allowed(), matched.is_some());
    }
    assert!(
        require_capability(&r, &strings(&["user"]), &[], AgentInvoke)
            .unwrap()
            .allowed()
    );
    assert!(
        require_capability(&r, &strings(&["viewer"]), &[], AgentInvoke)
            .unwrap_err()
            .contains("Missing capability")
    );
    let d = permission_denied_detail(AgentInvoke, "abc", None);
    assert_eq!(d["required_capability"], "AgentInvoke");
    assert_eq!(d["trace_id"], "abc");
    assert!(
        d["accepted_permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("conversation:playground:access"))
    );
    assert_eq!(
        permission_denied_detail(AgentInvoke, "", Some("Custom denied"))["message"],
        "Custom denied"
    );
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database/migrations");
    let seed = [
        "005_account_permission_system.sql",
        "063_eval_console_permissions.sql",
        "068_gateway_console_capability_permissions.sql",
    ]
    .iter()
    .map(|p| std::fs::read_to_string(root.join(p)).unwrap())
    .collect::<Vec<_>>()
    .join("\n");
    for c in [
        AgentInvoke,
        ServiceListRead,
        ServiceConfigWrite,
        GatewayMetricsRead,
        GatewayUsageRead,
        GatewayQuotaRead,
        GatewayQuotaWrite,
        GatewayRateLimitWrite,
        GatewayProviderConfigRead,
        GatewayProviderConfigWrite,
        GatewaySkillRead,
        GatewaySkillWrite,
        GatewayMcpRead,
        GatewayMcpWrite,
        GatewayModelConfigWrite,
        GatewayServiceConfigWrite,
        GatewayEvalTraceRead,
        GatewayEvalRun,
        GatewayConnectorConfigRead,
        GatewayConnectorConfigWrite,
    ] {
        assert_eq!(c.accepted_permissions()[0], c.canonical_permission());
        assert!(seed.contains(c.canonical_permission()), "unseeded {c:?}");
    }
}
#[test]
fn service_scope_normalization_matching_and_user_policy_matrix() {
    for (value, expected) in [
        (json!("agent, flash, gpt"), vec!["agent", "flash", "gpt"]),
        (json!(["Agent", "Flash"]), vec!["agent", "flash"]),
        (json!(["A", "B"]), vec!["a", "b"]),
        (Value::Null, vec![]),
        (json!(""), vec![]),
        (json!("a,a,b"), vec!["a", "b"]),
        (json!(42), vec![]),
    ] {
        assert_eq!(normalize_service_scope(&value), strings(&expected));
    }
    for (scope, candidates, expected) in [
        (vec!["agent"], vec!["agent"], true),
        (vec!["agent"], vec!["flash"], false),
        (vec!["*"], vec!["any-service"], true),
        (vec!["agent-*"], vec!["agent-prod", "agent-dev"], true),
        (vec!["agent-*"], vec!["flash"], false),
        (vec![], vec!["any-service"], true),
        (vec!["agent"], vec![], false),
    ] {
        assert_eq!(
            service_scope_matches(
                &json!(scope),
                &candidates.into_iter().map(|v| json!(v)).collect::<Vec<_>>()
            ),
            expected
        );
    }
    assert_eq!(
        service_access_policy_from_metadata(&Value::Null).mode,
        ServiceAccessMode::All
    );
    let p = service_access_policy_from_metadata(
        &json!({"service_access":{"allowed_services":"agent,flash"}}),
    );
    assert_eq!(p.mode, ServiceAccessMode::Allowlist);
    assert!(p.allowed_services.contains(&"agent".into()));
    assert_eq!(
        service_access_policy_from_metadata(&json!({"service_access":{"mode":"all"}})).mode,
        ServiceAccessMode::All
    );
    assert!(
        service_access_policy_from_metadata(
            &json!({"service_access":{"denied_services":"dangerous"}})
        )
        .denied_services
        .contains(&"dangerous".into())
    );
    for (mode, allowed, denied, candidate, result) in [
        (
            ServiceAccessMode::All,
            vec![],
            vec![],
            "agent",
            (true, "allowed"),
        ),
        (
            ServiceAccessMode::All,
            vec![],
            vec!["agent"],
            "agent",
            (false, "denied_by_user_policy"),
        ),
        (
            ServiceAccessMode::Allowlist,
            vec!["agent", "flash"],
            vec![],
            "agent",
            (true, "allowed"),
        ),
        (
            ServiceAccessMode::Allowlist,
            vec!["agent"],
            vec![],
            "flash",
            (false, "not_in_user_allowlist"),
        ),
        (
            ServiceAccessMode::Allowlist,
            vec![],
            vec![],
            "agent",
            (false, "user_allowlist_empty"),
        ),
    ] {
        assert_eq!(
            evaluate_service_access(
                &ServiceAccessPolicy {
                    mode,
                    allowed_services: strings(&allowed),
                    denied_services: strings(&denied)
                },
                &[json!(candidate)]
            ),
            result
        );
    }
    assert_eq!(
        normalize_service_candidates(&[json!("Agent"), json!("agent"), json!("Flash")]),
        strings(&["agent", "flash"])
    );
    assert!(normalize_service_candidates(&[]).is_empty());
}
