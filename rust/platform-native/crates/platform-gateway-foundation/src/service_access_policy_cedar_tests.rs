use super::*;
use ai_platform_auth::{
    cedar::{
        Configuration, DecisionAudit, DecisionLog, DecisionPoint, Mode, PolicyError,
        resources::{self, PolicyScope},
    },
    rbac::Rbac,
};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Audit(Mutex<Vec<DecisionLog>>);
impl DecisionAudit for Audit {
    fn ready(&self) -> bool {
        true
    }
    fn record(&self, event: DecisionLog) -> Result<(), PolicyError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}
#[tokio::test]
async fn cedar_differential_uses_the_actual_scope_intersection_and_user_policy_functions() {
    let audit = Arc::new(Audit::default());
    let point = DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &Rbac::new(Default::default()),
        audit.clone(),
    )
    .unwrap();
    resources::scoped(
        PolicyScope {
            point: Some(point),
            ..Default::default()
        },
        async {
            for aliases in [
                vec![],
                vec![json!("")],
                vec![json!("tool")],
                vec![json!("TOOL"), json!("other")],
                vec![json!(" 工具 ")],
            ] {
                for scope in [
                    vec![],
                    vec!["".into()],
                    vec!["*".into()],
                    vec!["tool".into()],
                    vec!["to*".into()],
                    vec!["no".into()],
                ] {
                    for second in [vec![], vec!["*".into()], vec!["other".into()]] {
                        for mode in [ServiceAccessMode::All, ServiceAccessMode::Allowlist] {
                            for allow in [
                                vec![],
                                vec!["".into()],
                                vec!["tool".into()],
                                vec!["*".into()],
                            ] {
                                for deny in [
                                    vec![],
                                    vec!["".into()],
                                    vec!["tool".into()],
                                    vec!["o*".into()],
                                ] {
                                    for admin in [false, true] {
                                        let policy = ServiceAccessPolicy {
                                            mode,
                                            allowed_services: allow.clone(),
                                            denied_services: deny.clone(),
                                        };
                                        let scopes = vec![
                                            ("key".into(), scope.clone()),
                                            ("tenant".into(), second.clone()),
                                        ];
                                        let legacy = legacy_service_visible_with_scopes(
                                            &aliases, &scopes, &policy, admin,
                                        );
                                        assert_eq!(
                                            service_visible_with_scopes(
                                                &aliases, &scopes, &policy, admin
                                            ),
                                            legacy
                                        );
                                        let logs = audit.0.lock().unwrap();
                                        let last = logs.last().unwrap();
                                        assert!(!last.evaluation_error);
                                        assert_eq!(
                                            last.cedar_allowed, legacy,
                                            "{aliases:?} {scopes:?} {policy:?} admin={admin}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
    .await;
    assert_eq!(audit.0.lock().unwrap().len(), 5760);
}
