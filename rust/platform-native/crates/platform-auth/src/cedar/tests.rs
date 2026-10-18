use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
struct Audit {
    logs: Mutex<Vec<DecisionLog>>,
    fail: AtomicBool,
}
impl DecisionAudit for Audit {
    fn ready(&self) -> bool {
        !self.fail.load(Ordering::SeqCst)
    }
    fn record(&self, event: DecisionLog) -> Result<(), PolicyError> {
        if !self.ready() {
            Err(PolicyError::AuditUnavailable)
        } else {
            self.logs.lock().unwrap().push(event);
            Ok(())
        }
    }
}
fn rbac() -> Rbac {
    Rbac::new(std::collections::HashMap::from([
        ("operator".into(), vec!["console:*".into()]),
        ("reader".into(), vec!["console:eval:view".into()]),
    ]))
}
#[test]
fn actual_cedar_schema_and_differential_matrix_cover_aliases_wildcards_denials_and_tenants() {
    let rbac = rbac();
    let audit = Arc::new(Audit::default());
    let point = DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &rbac,
        audit.clone(),
    )
    .unwrap();
    assert_eq!(point.matrix_sha256().len(), 64);
    assert!(point.decide(
        &rbac,
        &["operator".into()],
        &[],
        Capability::GatewayEvalRun,
        true
    ));
    assert!(!point.decide(
        &rbac,
        &["reader".into()],
        &[],
        Capability::GatewayEvalRun,
        false
    ));
    let logs = audit.logs.lock().unwrap();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|l| l.legacy_allowed == l.cedar_allowed));
}
#[test]
fn enforcement_requires_the_current_policy_and_role_matrix_and_fails_closed_on_audit_outage() {
    let rbac = rbac();
    let audit = Arc::new(Audit::default());
    let shadow = DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &rbac,
        audit.clone(),
    )
    .unwrap();
    let config = Configuration {
        mode: Mode::Enforce,
        enforce: BTreeSet::from(["GatewayEvalRun".into()]),
        resources: Default::default(),
        expected_policy_sha256: Some(shadow.policy_sha256().into()),
        expected_matrix_sha256: Some(shadow.matrix_sha256().into()),
    };
    let point = DecisionPoint::new(config.clone(), &rbac, audit.clone()).unwrap();
    assert!(point.decide(
        &rbac,
        &["operator".into()],
        &[],
        Capability::GatewayEvalRun,
        true
    ));
    audit.fail.store(true, Ordering::SeqCst);
    assert!(!point.decide(
        &rbac,
        &["operator".into()],
        &[],
        Capability::GatewayEvalRun,
        true
    ));
    let mut changed = rbac.clone();
    changed
        .role_permissions
        .insert("extra".into(), vec!["admin:*".into()]);
    assert!(DecisionPoint::new(config, &changed, audit).is_err());
}
#[test]
fn shadow_never_changes_legacy_denials_on_audit_failure() {
    let rbac = rbac();
    let audit = Arc::new(Audit::default());
    audit.fail.store(true, Ordering::SeqCst);
    let point = DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &rbac,
        audit,
    )
    .unwrap();
    assert!(point.decide(
        &rbac,
        &["operator".into()],
        &[],
        Capability::GatewayEvalRun,
        true
    ));
    assert!(!point.decide(&rbac, &[], &[], Capability::GatewayEvalRun, false));
}
