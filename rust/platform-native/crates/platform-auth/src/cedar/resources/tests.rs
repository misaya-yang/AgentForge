use super::*;
use crate::{
    cedar::{Configuration, DecisionAudit},
    rbac::Rbac,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
struct Audit {
    events: Mutex<Vec<DecisionLog>>,
    fail: AtomicBool,
    reject: AtomicBool,
}
impl DecisionAudit for Audit {
    fn ready(&self) -> bool {
        !self.fail.load(Ordering::SeqCst)
    }
    fn record(&self, event: DecisionLog) -> Result<(), PolicyError> {
        if self.ready() && !self.reject.load(Ordering::SeqCst) {
            self.events.lock().unwrap().push(event);
            Ok(())
        } else {
            Err(PolicyError::AuditUnavailable)
        }
    }
}
fn rbac() -> Rbac {
    Rbac::new(HashMap::from([
        ("admin".into(), vec!["admin:*".into()]),
        (
            "reader".into(),
            vec!["user:list".into(), "console:mcp:view".into()],
        ),
    ]))
}
fn subject() -> ObjectSubject {
    ObjectSubject {
        tenant_id: "tenant".into(),
        user_id: "owner".into(),
        tier: "normal".into(),
        ..Default::default()
    }
}
fn shadow(audit: Arc<Audit>) -> Arc<DecisionPoint> {
    DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &rbac(),
        audit,
    )
    .unwrap()
}
fn enforce(
    shadow: &DecisionPoint,
    kinds: impl IntoIterator<Item = ResourceKind>,
    audit: Arc<Audit>,
) -> Arc<DecisionPoint> {
    DecisionPoint::new(
        Configuration {
            mode: Mode::Enforce,
            resources: ResourceConfiguration {
                enforce: kinds.into_iter().collect(),
                expected_policy_sha256: Some(shadow.resource_policy_sha256().into()),
                expected_matrix_sha256: Some(shadow.resource_matrix_sha256().into()),
            },
            ..Default::default()
        },
        &rbac(),
        audit,
    )
    .unwrap()
}
#[test]
fn actual_schema_validates_six_resource_types_against_the_complete_differential_basis() {
    let p = ObjectPolicy::new(rbac().role_permissions).unwrap();
    let cases = p.fixtures();
    assert!(cases.len() > 5000);
    for (s, r, a) in cases {
        assert_eq!(
            p.evaluate(&s, &r, a).unwrap(),
            reference(&p.roles, &s, &r, a),
            "{s:?} {r:?} {a:?}"
        );
    }
    assert_eq!(p.policy_sha256.len(), 64);
    assert_eq!(p.matrix_sha256.len(), 64);
}
#[test]
fn flips_are_per_resource_digest_pinned_and_sensitive_to_configured_roles() {
    let audit = Arc::new(Audit::default());
    let p = shadow(audit.clone());
    let e = enforce(&p, [ResourceKind::Agent], audit.clone());
    assert!(e.resource_enforced(ResourceKind::Agent));
    assert!(!e.resource_enforced(ResourceKind::Dataset));
    let mut config = e.config.clone();
    config.resources.expected_matrix_sha256 = Some("stale".into());
    assert!(DecisionPoint::new(config, &rbac(), audit.clone()).is_err());
    let mut changed = rbac();
    changed
        .role_permissions
        .insert("new-role".into(), vec!["user:list".into()]);
    assert!(DecisionPoint::new(e.config.clone(), &changed, audit).is_err());
}
#[test]
fn independent_cedar_denial_is_observed_in_shadow_and_blocks_only_the_flipped_resource() {
    let audit = Arc::new(Audit::default());
    let p = shadow(audit.clone());
    let e = enforce(&p, [ResourceKind::Agent], audit.clone());
    let s = subject();
    let agent = ObjectResource::new(ResourceKind::Agent, "a", "tenant");
    let dataset = ObjectResource::new(ResourceKind::Dataset, "d", "tenant");
    let scope = PolicyScope {
        request_id: "correlation".into(),
        trace_id: "trace".into(),
        ..Default::default()
    };
    assert!(p.decide_resource(&s, &agent, ObjectAction::Read, true, &scope));
    assert!(!e.decide_resource(&s, &agent, ObjectAction::Read, true, &scope));
    assert!(e.decide_resource(&s, &dataset, ObjectAction::Read, true, &scope));
    let logs = audit.events.lock().unwrap();
    assert!(logs.iter().all(|e| e.legacy_allowed && !e.cedar_allowed));
    let facts = logs[0].resource.as_ref().unwrap();
    assert_eq!(facts.request_id, "correlation");
    assert_ne!(facts.resource_id_sha256, "a");
    assert_eq!(facts.facts_sha256.len(), 64);
}
#[test]
fn enforced_policy_never_widens_acl_and_rechecks_revocation_and_audit_readiness() {
    let audit = Arc::new(Audit::default());
    let p = shadow(audit.clone());
    let e = enforce(&p, [ResourceKind::Agent], audit.clone());
    let mut r = ObjectResource::new(ResourceKind::Agent, "a", "tenant");
    r.grants = vec!["owner".into()];
    let s = subject();
    let scope = PolicyScope::default();
    assert!(e.decide_resource(&s, &r, ObjectAction::Own, true, &scope));
    assert!(!e.decide_resource(&s, &r, ObjectAction::Own, false, &scope));
    r.grants.clear();
    assert!(!e.decide_resource(&s, &r, ObjectAction::Own, true, &scope));
    r.grants = vec!["owner".into()];
    audit.reject.store(true, Ordering::SeqCst);
    assert!(!e.decide_resource(&s, &r, ObjectAction::Own, true, &scope));
    assert!(p.decide_resource(&s, &r, ObjectAction::Own, true, &scope));
    audit.reject.store(false, Ordering::SeqCst);
    audit.fail.store(true, Ordering::SeqCst);
    assert!(!e.decide_resource(&s, &r, ObjectAction::Own, true, &scope));
    assert!(p.decide_resource(&s, &r, ObjectAction::Own, true, &scope));
}
#[test]
fn current_dataset_exceptions_invalid_grants_and_query_fallback_preserve_existing_acl() {
    let p = ObjectPolicy::new(rbac().role_permissions).unwrap();
    let mut s = subject();
    s.tenant_id = "foreign".into();
    let mut r = ObjectResource::new(ResourceKind::Dataset, "d", "tenant");
    r.visibility = "public".into();
    assert!(p.evaluate(&s, &r, ObjectAction::Read).unwrap());
    assert!(!p.evaluate(&s, &r, ObjectAction::Write).unwrap());
    r.grants = vec!["invalid".into()];
    assert!(!p.evaluate(&s, &r, ObjectAction::Read).unwrap());
    r.profile = ObjectProfile::DatasetQuery;
    assert!(p.evaluate(&s, &r, ObjectAction::Read).unwrap());
    r.owner_id = s.user_id.clone();
    assert!(p.evaluate(&s, &r, ObjectAction::Own).unwrap());
    r.identity_current = false;
    assert!(!p.evaluate(&s, &r, ObjectAction::Read).unwrap());
    r.identity_current = true;
    r.archived = true;
    assert!(!p.evaluate(&s, &r, ObjectAction::Read).unwrap());
}
#[test]
fn account_profiles_and_model_lifecycle_use_current_object_facts() {
    let p = ObjectPolicy::new(rbac().role_permissions).unwrap();
    let mut s = subject();
    s.roles = vec!["admin".into()];
    let mut r = ObjectResource::new(ResourceKind::User, "owner", "tenant");
    r.profile = ObjectProfile::UserActor;
    assert!(p.evaluate(&s, &r, ObjectAction::Admin).unwrap());
    r.current_binding = false;
    assert!(!p.evaluate(&s, &r, ObjectAction::Admin).unwrap());
    assert!(p.evaluate(&s, &r, ObjectAction::Profile).unwrap());
    r.profile = ObjectProfile::UserMutation;
    r.id = "target".into();
    r.protected = true;
    assert!(!p.evaluate(&s, &r, ObjectAction::Admin).unwrap());
    r.protected = false;
    assert!(p.evaluate(&s, &r, ObjectAction::Admin).unwrap());
    let mut model = ObjectResource::new(ResourceKind::Model, "m", "tenant");
    model.access_level = "premium".into();
    assert!(p.evaluate(&s, &model, ObjectAction::Invoke).unwrap());
    model.enabled = false;
    assert!(!p.evaluate(&s, &model, ObjectAction::Invoke).unwrap());
}
#[tokio::test]
async fn retained_scope_survives_spawn_without_cross_request_subject_leakage() {
    let audit = Arc::new(Audit::default());
    let p = shadow(audit);
    let s = subject();
    let future = scoped(
        PolicyScope {
            point: Some(p.clone()),
            request_id: "first".into(),
            ..Default::default()
        },
        async {
            remember_subject(s.clone());
            retained(async {
                assert_eq!(capture().request_id, "first");
                assert_eq!(current_subject().unwrap().user_id, "owner");
                capture().point.is_some()
            })
        },
    )
    .await;
    assert!(tokio::spawn(future).await.unwrap());
    assert!(capture().point.is_none());
    assert!(current_subject().is_none());
    scoped(
        PolicyScope {
            request_id: "second".into(),
            ..Default::default()
        },
        async {
            assert!(current_subject().is_none());
            remember_subject(ObjectSubject {
                user_id: "different".into(),
                ..Default::default()
            });
            remember_subject(s);
            assert_eq!(current_subject().unwrap().user_id, "different");
        },
    )
    .await;
}

#[test]
fn shipped_resource_policy_parses_without_unsupported_quantifier_syntax() {
    // Cedar has no lambda/exists method. Parse directly so future syntax failures
    // report the actual parser diagnostic instead of only Configuration at startup.
    let policies = POLICY
        .parse::<PolicySet>()
        .expect("shipped Cedar resource policy must parse");
    assert_eq!(policies.policies().count(), ResourceKind::ALL.len());
}

#[test]
fn dataset_nonempty_grant_test_preserves_unknown_grant_denials_and_query_fallback() {
    let point = ObjectPolicy::new(rbac().role_permissions).unwrap();
    let actor = ObjectSubject {
        user_id: "reader".into(),
        ..subject()
    };
    let mut dataset = ObjectResource::new(ResourceKind::Dataset, "dataset", "tenant");
    dataset.visibility = "public".into();
    // Standard datasets treat any nonempty raw grant as an explicit ACL result,
    // including unrecognized grants: those must suppress public fallback.
    for (grants, allowed) in [
        (vec![], true),
        (vec![""], true),
        (vec!["", ""], true),
        (vec!["unrecognized"], false),
        (vec!["", "unrecognized"], false),
        (vec![" "], false),
        (vec!["viewer"], true),
    ] {
        dataset.grants = grants.into_iter().map(str::to_owned).collect();
        assert_eq!(
            point
                .evaluate(&actor, &dataset, ObjectAction::Read)
                .unwrap(),
            allowed,
            "{:?}",
            dataset.grants
        );
        assert_eq!(
            reference(&point.roles, &actor, &dataset, ObjectAction::Read),
            allowed
        );
    }
    dataset.profile = ObjectProfile::DatasetQuery;
    dataset.grants = vec!["unrecognized".into()];
    assert!(
        point
            .evaluate(&actor, &dataset, ObjectAction::Read)
            .unwrap()
    );
    dataset.visibility = "private".into();
    assert!(
        !point
            .evaluate(&actor, &dataset, ObjectAction::Read)
            .unwrap()
    );
    dataset.grants = vec!["viewer".into()];
    assert!(
        point
            .evaluate(&actor, &dataset, ObjectAction::Read)
            .unwrap()
    );
    assert!(
        !point
            .evaluate(&actor, &dataset, ObjectAction::Write)
            .unwrap()
    );
    dataset.archived = true;
    assert!(
        !point
            .evaluate(&actor, &dataset, ObjectAction::Read)
            .unwrap()
    );
    dataset.archived = false;
    dataset.identity_current = false;
    assert!(
        !point
            .evaluate(&actor, &dataset, ObjectAction::Read)
            .unwrap()
    );
}

#[test]
fn tool_alias_presence_never_bypasses_current_scopes_tenant_or_denials() {
    let point = ObjectPolicy::new(rbac().role_permissions).unwrap();
    let actor = ObjectSubject {
        user_id: "reader".into(),
        ..subject()
    };
    let admin = ObjectSubject {
        roles: vec!["admin".into()],
        ..actor.clone()
    };
    let mut tool = ObjectResource::new(ResourceKind::Tool, "tool", "tenant");
    tool.profile = ObjectProfile::ToolVisibility;
    assert!(!point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    assert!(!point.evaluate(&admin, &tool, ObjectAction::Read).unwrap());
    tool.aliases.insert("search".into());
    tool.required_scopes.insert("knowledge:read".into());
    assert!(!point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    assert!(!point.evaluate(&admin, &tool, ObjectAction::Read).unwrap());
    tool.satisfied_scopes.insert("knowledge:read".into());
    assert!(point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    tool.denied_aliases.insert("search".into());
    assert!(!point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    tool.denied_aliases.clear();
    tool.allowlist_mode = true;
    assert!(!point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    tool.allowed_aliases.insert("search".into());
    assert!(point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    tool.tenant_id = "foreign".into();
    assert!(!point.evaluate(&actor, &tool, ObjectAction::Read).unwrap());
    assert!(!point.evaluate(&admin, &tool, ObjectAction::Read).unwrap());
}
