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
async fn current_model_access_uses_the_actual_actor_access_and_enabled_predicates() {
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
    resources::scoped(PolicyScope{point:Some(point),..Default::default()},async{for role in ["admin","Admin","tenant_admin","platform_admin","reader",""]{for tier in ["normal","premium","enterprise","admin","ADMIN",""]{for access in ["public","premium","admin","unknown",""]{for enabled in [true,false]{let actor=Actor{tenant_id:"t".into(),user_id:"u".into(),roles:vec![role.into()],tier:tier.into()};let model=json!({"model_id":"m","tenant_id":"t","access_level":access,"is_enabled":enabled});let legacy=enabled&&user_can_access_model(&actor,access);assert_eq!(authorize_model(&actor,&model),legacy);let logs=audit.0.lock().unwrap();let event=logs.last().unwrap();assert!(!event.evaluation_error);assert_eq!(event.cedar_allowed,legacy,"role={role} tier={tier} access={access} enabled={enabled}");}}}}}).await;
    assert_eq!(audit.0.lock().unwrap().len(), 360);
}
