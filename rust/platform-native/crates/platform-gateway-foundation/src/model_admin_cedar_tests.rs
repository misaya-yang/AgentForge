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
async fn current_model_admin_capability_aliases_and_fixed_role_map_match_independent_cedar() {
    let audit = Arc::new(Audit::default());
    let point = DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &Rbac::new(HashMap::from([("operator".into(), vec!["admin:*".into()])])),
        audit.clone(),
    )
    .unwrap();
    resources::scoped(
        PolicyScope {
            point: Some(point),
            ..Default::default()
        },
        async {
            for role in [
                "admin",
                " admin ",
                "Admin",
                "admin:*",
                "console:models:edit",
                "console:models:*",
                "console:*",
                "operator",
                "premium",
                "",
            ] {
                let user = ResolvedModelUser {
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                    roles: vec![role.into()],
                    is_authenticated: true,
                };
                let allowed = ModelAdminApi::require_write(&user).is_ok();
                let logs = audit.0.lock().unwrap();
                let log = logs.last().unwrap();
                assert_eq!(log.cedar_allowed, log.legacy_allowed, "{role}");
                assert_eq!(allowed, log.legacy_allowed);
                assert!(!log.evaluation_error);
            }
        },
    )
    .await;
}
