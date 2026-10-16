//! The live handler and offline producer share the identical pure extension.
use ai_platform_gateway::migration_host::{application, openapi_document};
#[tokio::test]
async fn live_document_uses_shared_native_declarations() {
    let axum::Json(actual) = application::openapi_document().await;
    let expected = openapi_document::extend_document(
        ai_platform_gateway_foundation::native_contract::document(),
    );
    assert_eq!(actual, expected);
    assert!(actual["paths"]["/api/v1/conversations"]["post"].is_object());
    assert!(actual["paths"]["/api/v1/agents/{agent}/publish"]["post"].is_object());
    assert_eq!(actual["x-native-route-registry"]["complete"], false);
    assert_eq!(
        actual["paths"]["/api/v1/conversations"]["post"]["x-native-declaration"]["schema_verified"],
        false
    );
}
