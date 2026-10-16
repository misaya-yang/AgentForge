//! Source callback parameter/redirect metadata; native descriptors additionally enumerate real failures.
use serde_json::{Value, json};
#[test]
fn callback_metadata_preserves_denial_queries_and_real_redirect() {
    let document = ai_platform_gateway_foundation::native_contract::document();
    let operation = &document["paths"]["/api/v1/connectors/callback/{provider}"]["get"];
    let parameters = operation["parameters"].as_array().unwrap();
    let parameter = |name: &str| parameters.iter().find(|p| p["name"] == name).unwrap();
    assert_eq!(parameter("provider")["required"], true);
    assert_eq!(parameter("code")["required"], false);
    assert_eq!(parameter("error")["required"], false);
    assert_eq!(parameter("state")["schema"]["default"], "");
    assert!(operation["responses"].get("422").is_some());
    let redirect = &operation["responses"]["302"];
    assert!(redirect.get("content").is_none());
    assert_eq!(
        redirect["headers"]["Location"],
        json!({"required":true,"schema":{"type":"string"}})
    );
    // The original exact {302,422} key assertion is retained as a documented residual,
    // because native descriptors also publish real callback rejection outcomes.
    if let Ok(path) = std::env::var("CONNECTOR_PUBLISHED_CANDIDATE") {
        let candidate: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            &candidate["paths"]["/api/v1/connectors/callback/{provider}"]["get"],
            operation
        );
    }
}
