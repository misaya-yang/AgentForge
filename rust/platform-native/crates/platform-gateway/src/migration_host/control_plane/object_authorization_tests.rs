use super::*;
use ai_platform_auth::cedar::{
    Configuration, DecisionAudit, DecisionLog, Mode, PolicyError, resources::ResourceConfiguration,
};
use ai_platform_service_primitives::knowledge_auth::ReplayStore;
use axum::{body::Body, http::HeaderMap};
use std::collections::BTreeMap;
use tower::ServiceExt;
struct Audit;
impl DecisionAudit for Audit {
    fn ready(&self) -> bool {
        true
    }
    fn record(&self, _: DecisionLog) -> Result<(), PolicyError> {
        Ok(())
    }
}
fn point() -> Arc<DecisionPoint> {
    DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        &Rbac::new(Default::default()),
        Arc::new(Audit),
    )
    .unwrap()
}
fn verifier() -> GatewayAuth {
    let mut a = GatewayAuth::new(
        "test-only-resource-policy-key".into(),
        BTreeMap::new(),
        "v2".into(),
        ReplayStore::memory(),
    )
    .unwrap();
    a.caller_service = "knowledge-service".into();
    a.audience = "gateway".into();
    a.allowed_path_prefixes = vec![DATASET_DECISION_PATH.into()];
    a
}
#[tokio::test]
async fn private_dataset_pdp_checks_exact_proof_before_parsing_or_current_identity_io() {
    let verifier = Arc::new(verifier());
    let router = router(DatasetPdp {
        point: point(),
        verifier: verifier.clone(),
        actors: CurrentActorStore {
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
                .unwrap(),
        },
    })
    .unwrap();
    let path = format!("{DATASET_DECISION_PATH}/batch");
    let body = b"not json";
    let signed = verifier
        .sign("POST", &path, "", body, &HeaderMap::new())
        .unwrap();
    let make = |signature: &str, payload: &[u8]| {
        Request::builder()
            .method("POST")
            .uri(&path)
            .header("x-gateway-secret", signature)
            .body(Body::from(payload.to_vec()))
            .unwrap()
    };
    assert_eq!(
        router
            .clone()
            .oneshot(make("", body))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router
            .clone()
            .oneshot(make(&signed, b"tampered"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router
            .clone()
            .oneshot(make(&signed, body))
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        router.oneshot(make(&signed, body)).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
#[test]
fn rank_receipts_recheck_current_identity_lifecycle_and_individual_grant_levels() {
    let shadow = point();
    let rbac = Rbac::new(Default::default());
    let enforce = DecisionPoint::new(
        Configuration {
            mode: Mode::Enforce,
            resources: ResourceConfiguration {
                enforce: [ResourceKind::Dataset].into_iter().collect(),
                expected_policy_sha256: Some(shadow.resource_policy_sha256().into()),
                expected_matrix_sha256: Some(shadow.resource_matrix_sha256().into()),
            },
            ..Default::default()
        },
        &rbac,
        Arc::new(Audit),
    )
    .unwrap();
    let subject = ObjectSubject {
        tenant_id: "t".into(),
        user_id: "u".into(),
        ..Default::default()
    };
    let mut resource = ObjectResource::new(ResourceKind::Dataset, "d", "t");
    resource.grants = vec!["editor".into()];
    let check = DatasetRankCheck {
        resource,
        legacy_rank: 3,
    };
    let allowed = ranks(
        &enforce,
        &subject,
        vec![check.clone()],
        true,
        &PolicyScope::default(),
    );
    assert_eq!(allowed[0].allowed_rank, 2);
    assert!(allowed[0].enforced);
    assert_eq!(
        ranks(
            &enforce,
            &subject,
            vec![check.clone()],
            false,
            &PolicyScope::default()
        )[0]
        .allowed_rank,
        0
    );
    assert_eq!(
        ranks(
            &shadow,
            &subject,
            vec![check],
            false,
            &PolicyScope::default()
        )[0]
        .allowed_rank,
        3
    );
}
