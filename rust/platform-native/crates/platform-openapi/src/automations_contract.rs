//! Automations publish a source-owned contract through the canonical native producer.
use serde_json::Value;
use sha2::{Digest, Sha256};
const SOURCE: &str =
    include_str!("../../platform-gateway/src/migration_agent/automations/public-contract.json");
pub fn extend_document(document: &mut Value) {
    let contract: Value = serde_json::from_str(SOURCE).expect("source-owned automation contract");
    document["paths"].as_object_mut().expect("paths").extend(
        contract["paths"]
            .as_object()
            .expect("automation paths")
            .clone(),
    );
    document["components"]["schemas"]
        .as_object_mut()
        .expect("schemas")
        .extend(
            contract["components"]["schemas"]
                .as_object()
                .expect("automation schemas")
                .clone(),
        );
    document["x-native-contract"]["common_source_sha256"]["automations/public-contract.json"] =
        Value::String(format!("{:x}", Sha256::digest(SOURCE.as_bytes())));
}
#[cfg(test)]
mod tests {
    #[test]
    fn draft_and_confirm_are_distinct_and_webhook_auth_is_explicit() {
        let contract: serde_json::Value = serde_json::from_str(super::SOURCE).unwrap();
        assert!(contract["paths"]["/api/v1/automations/draft"]["post"].is_object());
        assert_eq!(
            contract["paths"]["/api/v1/automations/{automation_id}/confirm"]["post"]["requestBody"]
                ["content"]["application/json"]["schema"]["properties"]["confirm"]["const"],
            true
        );
        assert_eq!(
            contract["paths"]["/api/v1/automations/{automation_id}/webhook"]["post"]["parameters"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
}
