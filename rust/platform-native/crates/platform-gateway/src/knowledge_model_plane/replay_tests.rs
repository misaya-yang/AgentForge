use super::*;
use std::cell::Cell;
fn execution() -> ModelExecution {
    let c = crate::knowledge_model_plane::tests::call();
    unknown(
        &ModelLease {
            lease_id: "lease".into(),
            lease_token: "token".into(),
            call_id: c.call_id,
            request_sha256: "a".repeat(64),
            tenant_id: "t".into(),
            user_id: "u".into(),
            model: c.model,
            expires_at_unix: 1,
        },
        "frozen-book",
    )
}
#[tokio::test]
async fn revoked_actor_model_or_resource_still_settles_but_never_delivers_output() {
    for code in [
        "knowledge_actor_forbidden",
        "knowledge_model_access_denied",
        "knowledge_model_resource_scope_denied",
    ] {
        let settled = Cell::new(false);
        let result = settle_before_delivery(
            async {
                settled.set(true);
                let mut result = execution();
                result.output = json!({"private":"must not escape"});
                result.receipt.settled = true;
                Ok(result)
            },
            async {
                assert!(
                    settled.get(),
                    "mutable ACL must be checked after durable spend"
                );
                Err(PlaneError::new(403, code))
            },
        )
        .await;
        assert!(settled.get());
        assert_eq!(result.unwrap_err().code, code);
    }
}
#[tokio::test]
async fn settlement_failure_or_unknown_commit_cannot_deliver_output() {
    for code in [
        "knowledge_accounting_unavailable",
        "knowledge_settlement_commit_unknown",
    ] {
        let checked = Cell::new(false);
        let result = settle_before_delivery(async { Err(PlaneError::new(503, code)) }, async {
            checked.set(true);
            Ok(())
        })
        .await;
        assert_eq!(result.unwrap_err().code, code);
        assert!(!checked.get());
    }
}
#[tokio::test]
async fn authorized_delivery_keeps_original_frozen_receipt() {
    let result = settle_before_delivery(async { Ok(execution()) }, async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(result.receipt.price_book_revision, "frozen-book");
}
#[test]
fn receipt_revision_comes_from_each_frozen_meter_without_config_fallback() {
    for (pricing, expected) in [
        (
            json!({"price_book_input":{"entry":{"key":{"book_version":"token-book"}}}}),
            "token-book",
        ),
        (
            json!({"pricing_meter":"requests","price_book_request":{"entry":{"key":{"book_version":"request-book"}}}}),
            "request-book",
        ),
    ] {
        assert_eq!(frozen_revision(&pricing).unwrap(), expected);
    }
    for pricing in [
        json!({}),
        json!({"price_book_input":{"entry":{"key":{"book_version":""}}}}),
        json!({"pricing_meter":"requests","price_book_input":{"entry":{"key":{"book_version":"wrong-meter"}}}}),
    ] {
        assert!(frozen_revision(&pricing).is_err());
    }
}
