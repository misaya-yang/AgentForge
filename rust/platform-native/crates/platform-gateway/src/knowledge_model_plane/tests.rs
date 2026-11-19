use super::*;
use ai_platform_service_primitives::knowledge_types::UserContext;
pub(super) fn call() -> ModelCall {
    ModelCall {
        version: 1,
        call_id: "call-1".into(),
        scope: ModelScope {
            actor: UserContext {
                user_id: "u".into(),
                tenant_id: "t".into(),
                roles: vec![],
                user_tier: "normal".into(),
                user_type: "user".into(),
                is_authenticated: true,
            },
            dataset_id: None,
            document_id: None,
            execution_id: None,
        },
        model: ModelIdentity {
            provider: "p".into(),
            model: "m".into(),
            revision: "r".into(),
        },
        purpose: ModelPurpose::Qa,
        wire: ModelWire::ChatCompletions,
        stream: false,
        budget: ModelBudget {
            input_bytes: 1000,
            max_output_tokens: 10,
            deadline_ms: 1000,
        },
        payload: json!({"model":"m","messages":[],"max_tokens":10}),
    }
}
fn keys(seed: &[u8; 32]) -> signature::PublicKeyring {
    signature::PublicKeyring {
        schema_version: "gateway-verification-keys/v2".into(),
        keys: vec![signature::PublicKey {
            key_id: "test".into(),
            public_key_hex: signature::hex(
                &ed25519_dalek::SigningKey::from_bytes(seed)
                    .verifying_key()
                    .to_bytes(),
            ),
            not_before_ms: 0,
            not_after_ms: u64::MAX,
            revoked: false,
        }],
    }
}
fn lease(signer: &LeaseAuthority) -> ModelLease {
    let c = call();
    let mut l = ModelLease {
        lease_id: "lease".into(),
        lease_token: String::new(),
        call_id: c.call_id.clone(),
        request_sha256: call_digest(&c).unwrap(),
        tenant_id: "t".into(),
        user_id: "u".into(),
        model: c.model,
        expires_at_unix: now_ms() / 1000 + 60,
    };
    l.lease_token = signer
        .sign(&claims(&l), now_ms(), l.expires_at_unix * 1000)
        .unwrap();
    l
}
#[test]
fn lease_signature_binds_tenant_actor_body_model_and_identity() {
    let seed = [7; 32];
    let signer = LeaseAuthority::new(seed, "test".into(), keys(&seed)).unwrap();
    let l = lease(&signer);
    assert!(signer.verify(&l.lease_token, &l).is_ok());
    for i in 0..6 {
        let mut forged = l.clone();
        match i {
            0 => forged.tenant_id = "foreign".into(),
            1 => forged.user_id = "other".into(),
            2 => forged.request_sha256 = "f".repeat(64),
            3 => forged.model.revision = "next".into(),
            4 => forged.call_id = "other".into(),
            _ => forged.lease_id = "other".into(),
        }
        assert!(signer.verify(&l.lease_token, &forged).is_err())
    }
}
#[test]
fn foreign_purpose_or_revoked_key_cannot_authorize_model_dispatch() {
    let seed = [8; 32];
    let public = keys(&seed);
    let signer = LeaseAuthority::new(seed, "test".into(), public.clone()).unwrap();
    let l = lease(&signer);
    let envelope = signature::SignedEnvelope::issue(
        &seed,
        "test",
        "model-lease",
        claims(&l),
        now_ms(),
        l.expires_at_unix * 1000,
        &public,
    )
    .unwrap();
    assert!(
        signer
            .verify(
                &signature::encode(signature::ENVELOPE_PREFIX, &envelope).unwrap(),
                &l
            )
            .is_err()
    );
    let mut public = public;
    public.keys[0].revoked = true;
    assert!(LeaseAuthority::new(seed, "test".into(), public).is_err());
}
#[test]
fn body_conflicts_anonymous_and_budget_overruns_fail_before_authority() {
    let c = call();
    let digest = call_digest(&c).unwrap();
    assert!(validate(&c, &digest).is_ok());
    let mut changed = c.clone();
    changed.payload["messages"] = json!([{"content":"changed"}]);
    assert_eq!(validate(&changed, &digest).unwrap_err().status, 409);
    let mut anonymous = c.clone();
    anonymous.scope.actor.is_authenticated = false;
    assert_eq!(
        validate(&anonymous, &call_digest(&anonymous).unwrap())
            .unwrap_err()
            .status,
        422
    );
    let mut budget = c;
    budget.budget.input_bytes = 1;
    assert_eq!(
        validate(&budget, &call_digest(&budget).unwrap())
            .unwrap_err()
            .status,
        422
    );
}
#[test]
fn duplicate_identity_replays_but_conflicting_body_never_reuses_lease() {
    let seed = [9; 32];
    let signer = LeaseAuthority::new(seed, "test".into(), keys(&seed)).unwrap();
    let l = lease(&signer);
    let row = json!({"lease":l,"request_sha256":l.request_sha256});
    assert_eq!(
        existing_lease(&row, &l.request_sha256).unwrap().lease_id,
        l.lease_id
    );
    assert_eq!(
        existing_lease(&row, &"a".repeat(64)).unwrap_err().status,
        409
    );
}
#[test]
fn unknown_receipt_never_claims_zero_usage_or_settlement() {
    let seed = [4; 32];
    let signer = LeaseAuthority::new(seed, "test".into(), keys(&seed)).unwrap();
    let l = lease(&signer);
    let result = unknown(&l, "book-r1");
    assert_eq!(result.state, ExecutionState::DispatchUnknown);
    assert!(!result.receipt.settled);
    assert!(result.receipt.units.is_empty());
    assert_eq!(result.receipt.tenant_id, l.tenant_id);
}
struct NoProvider;
impl RuntimeProviderCredentials for NoProvider {
    fn runtime_provider<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> crate::migration_agent::AgentFuture<'a, Value> {
        Box::pin(async { panic!("unauthorized HTTP must not resolve credentials") })
    }
}
async fn plane() -> KnowledgeModelPlane {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/fixture")
        .unwrap();
    let seed = [12; 32];
    let signer = Arc::new(LeaseAuthority::new(seed, "test".into(), keys(&seed)).unwrap());
    let workload = Arc::new(
        WorkloadVerifier::new(
            pool.clone(),
            WorkloadAuthentication::Ed25519 { keys: keys(&seed) },
        )
        .unwrap(),
    );
    let admission = Arc::new(
        crate::capacity_admission::CapacityAdmissionController::new(Default::default(), None)
            .unwrap(),
    );
    let knowledge = Arc::new(
        crate::knowledge::KnowledgeService::new(
            crate::knowledge::KnowledgeConfig::new(
                "http://127.0.0.1:1",
                "fixture".into(),
                "fixture-secret-long-enough".into(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    KnowledgeModelPlane::new(
        pool,
        signer,
        workload,
        Arc::new(NoProvider),
        admission,
        Arc::new(Default::default()),
        "book".into(),
        knowledge,
    )
    .unwrap()
}
#[tokio::test]
async fn http_unsigned_or_foreign_actor_cannot_reach_account_or_provider() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let call = call();
    let body = serde_json::to_vec(&ReserveRequest {
        request_sha256: call_digest(&call).unwrap(),
        call,
    })
    .unwrap();
    let router = http::router(plane().await);
    let response = router
        .clone()
        .oneshot(
            Request::post(wire::RESERVE_PATH)
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let seed = [12; 32];
    let now = now_ms();
    let claims = json!({"caller_service":"knowledge-service","audience":"gateway","method":"POST","path":wire::RESERVE_PATH,"body_sha256":hash(&body),"tenant_id":"foreign","user_id":"u","nonce":"one"});
    let token = signature::SignedEnvelope::issue(
        &seed,
        "test",
        "knowledge-model-workload",
        claims,
        now,
        now + 30_000,
        &keys(&seed),
    )
    .unwrap();
    let response = router
        .oneshot(
            Request::post(wire::RESERVE_PATH)
                .header("content-type", "application/json")
                .header(
                    "x-knowledge-workload",
                    signature::encode(signature::ENVELOPE_PREFIX, &token).unwrap(),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}
#[tokio::test]
async fn verify_only_mode_rejects_hmac_without_opening_database() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let call = call();
    let body = serde_json::to_vec(&ReserveRequest {
        request_sha256: call_digest(&call).unwrap(),
        call,
    })
    .unwrap();
    let response = http::router(plane().await)
        .oneshot(
            Request::post(wire::RESERVE_PATH)
                .header("content-type", "application/json")
                .header("x-gateway-signature", "v2:forged")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}
#[tokio::test]
async fn malformed_http_and_query_tampering_are_rejected() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let router = http::router(plane().await);
    for path in [
        wire::RESERVE_PATH,
        &format!("{}?tenant=foreign", wire::RESERVE_PATH),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 422);
    }
}
