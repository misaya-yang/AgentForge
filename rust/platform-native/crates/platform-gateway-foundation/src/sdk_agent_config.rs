//! New SDK credentials are encrypted and write-only; legacy passthrough is forbidden.
use crate::{protocol::{ServiceDefinition, TransportType}, provider_cipher::NativeProviderCipher, provider_service::ProviderCipher};
use serde_json::{Value, json};

pub fn is_sdk(service: &ServiceDefinition) -> bool {
    matches!(service.metadata["adapter_type"].as_str(), Some("openai_agents" | "claude_agent_sdk"))
}
fn valid_key(value: &str) -> bool {
    (16..=4096).contains(&value.len()) && !value.starts_with("enc:") && !value.bytes().any(|b|b <= 32)
}
/// Validation is before persistence and independent of worker availability.
pub fn validate_and_seal(service: &mut ServiceDefinition, encryption_key: &str) -> Result<(), &'static str> {
    if !is_sdk(service) { return Err("sdk_credentials_require_sdk_adapter"); }
    if service.connector_type != TransportType::Http || !service.session_enabled || service.max_retries != 0 {
        return Err("sdk_service_configuration_invalid");
    }
    crate::sdk_read_grants::datasets(service)?;
    let agent = &mut service.metadata["sdk_agent"];
    let object = agent.as_object().ok_or("sdk_agent_configuration_invalid")?;
    if object.len() != 3 || !object.keys().all(|v|matches!(v.as_str(),"model"|"instructions"|"max_turns"))
        || !agent["model"].as_str().is_some_and(|v|!v.trim().is_empty() && v.len()<=160)
        || !agent["instructions"].as_str().is_some_and(|v|v.len()<=32768)
        || !agent["max_turns"].as_u64().is_some_and(|v|(1..=32).contains(&v)) {
        return Err("sdk_agent_configuration_invalid");
    }
    if agent["instructions"].as_str().is_some_and(|v|v.trim().is_empty()) { agent["instructions"]=json!("You are a helpful assistant."); }
    if encryption_key.is_empty() { return Err("sdk_encryption_configuration_required"); }
    let config=service.connector_config.as_object_mut().ok_or("sdk_connector_invalid")?;
    if config.keys().any(|k|!matches!(k.as_str(),"sdk_provider_api_key"|"sdk_provider_api_key_encrypted")) {
        return Err("sdk_connector_invalid");
    }
    if let Some(raw)=config.remove("sdk_provider_api_key") {
        let raw=raw.as_str().filter(|v|valid_key(v)).ok_or("sdk_provider_key_invalid")?;
        let envelope=json!({"schema":"sdk-provider-key/v1","service_id":service.service_id,"api_key":raw}).to_string();
        let cipher=NativeProviderCipher::new();
        let sealed=cipher.encrypt(&envelope,encryption_key).map_err(|_|"sdk_encryption_failed")?;
        // Existing provider compatibility cipher falls back to plaintext. Never persist that fallback here.
        if !sealed.starts_with("enc:") || cipher.decrypt(&sealed,encryption_key).ok().as_deref()!=Some(envelope.as_str()) {
            return Err("sdk_encryption_failed");
        }
        config.insert("sdk_provider_api_key_encrypted".into(),json!(sealed));
    }
    decrypt_key(service,encryption_key).map(|_|())
}
/// Only the private SDK transport calls this after service admission/config lookup.
pub fn decrypt_key(service: &ServiceDefinition, encryption_key: &str) -> Result<String, &'static str> {
    if encryption_key.is_empty() { return Err("sdk_encryption_configuration_required"); }
    let sealed=service.connector_config["sdk_provider_api_key_encrypted"].as_str().filter(|v|v.starts_with("enc:")).ok_or("sdk_provider_key_required")?;
    let plain=NativeProviderCipher::new().decrypt(sealed,encryption_key).map_err(|_|"sdk_decryption_failed")?;
    if plain==sealed { return Err("sdk_decryption_failed"); }
    let value:Value=serde_json::from_str(&plain).map_err(|_|"sdk_decryption_failed")?;
    if value["schema"]!="sdk-provider-key/v1" || value["service_id"]!=service.service_id { return Err("sdk_credential_scope_invalid"); }
    value["api_key"].as_str().filter(|v|valid_key(v)).map(str::to_owned).ok_or("sdk_provider_key_invalid")
}
