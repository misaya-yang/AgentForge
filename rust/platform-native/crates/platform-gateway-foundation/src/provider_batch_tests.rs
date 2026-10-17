use super::*;
fn plan() -> BatchPlan {
    BatchPlan {
        provider_id: "openai".into(),
        model_id: "configured-model".into(),
        input_file_id: "file-owned".into(),
        endpoint: "/v1/responses".into(),
    }
}
fn receipt(nonce: Uuid) -> Value {
    json!({"id":"batch_123","input_file_id":"file-owned","endpoint":"/v1/responses","completion_window":"24h","metadata":{"agentforge_nonce":nonce.to_string()},"status":"completed"})
}
#[test]
fn provider_receipt_cannot_cross_intent_or_replace_job() {
    let nonce = Uuid::new_v4();
    let valid = receipt(nonce);
    assert_eq!(
        verify_receipt(&valid, &plan(), nonce, Some("batch_123")),
        Ok("batch_123".into())
    );
    for key in ["input_file_id", "endpoint", "completion_window", "id"] {
        let mut forged = valid.clone();
        forged[key] = json!("foreign");
        assert_eq!(
            verify_receipt(&forged, &plan(), nonce, None),
            Err(BatchError::BindingMismatch)
        );
    }
    assert!(verify_receipt(&valid, &plan(), Uuid::new_v4(), None).is_err());
    assert!(verify_receipt(&valid, &plan(), nonce, Some("batch_other")).is_err());
}
#[test]
fn pending_or_unknown_provider_state_never_proves_success() {
    for status in ["validating", "in_progress", "finalizing", "cancelling"] {
        assert_eq!(
            terminal_status(&json!({"status":status})),
            Err(BatchError::NotTerminal)
        );
    }
    for status in ["", "unknown", "pending", "success"] {
        assert_eq!(
            terminal_status(&json!({"status":status})),
            Err(BatchError::BindingMismatch)
        );
    }
    for status in ["failed", "expired", "cancelled"] {
        assert_eq!(terminal_status(&json!({"status":status})), Ok("failed"));
    }
}
struct NeverProvider;
impl BatchProvider for NeverProvider {
    fn submit<'a>(&'a self, _: &'a BatchPlan, _: Uuid) -> BatchFuture<'a, Value> {
        Box::pin(async { panic!("must not submit") })
    }
    fn observe<'a>(&'a self, _: &'a str) -> BatchFuture<'a, Value> {
        Box::pin(async { Err(BatchError::Unavailable) })
    }
}
#[test]
fn tenant_provider_registration_is_not_replaceable() {
    let mut registry = BatchProviders::default();
    let original: Arc<dyn BatchProvider> = Arc::new(NeverProvider);
    registry
        .register("tenant", "openai", original.clone())
        .unwrap();
    assert!(
        registry
            .register("tenant", "openai", Arc::new(NeverProvider))
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &registry.get("tenant", "openai").unwrap(),
        &original
    ));
    assert!(registry.get("foreign", "openai").is_err());
}
#[test]
fn invalid_plan_and_operator_credentials_do_not_create_client() {
    let mut p = plan();
    p.endpoint = "https://attacker.invalid".into();
    assert_eq!(p.validate(), Err(BatchError::InvalidPlan));
    assert!(OpenAiBatchProvider::new(String::new()).is_err());
    assert!(OpenAiBatchProvider::new("secret\nheader".into()).is_err());
}
