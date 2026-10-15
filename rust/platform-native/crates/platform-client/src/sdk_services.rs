//! Public official-SDK service configuration. Never retain returned connector secrets.
use crate::{ApiRequest, Error};
use reqwest::Method;
use serde_json::{Value, json};

#[derive(Clone, PartialEq)]
pub struct Settings {
    pub model: String,
    pub instructions: String,
    pub max_turns: u8,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            model: String::new(),
            instructions: String::new(),
            max_turns: 8,
        }
    }
}
impl Settings {
    fn value(&self) -> Result<Value, Error> {
        let model = self.model.trim();
        if model.is_empty()
            || model.len() > 160
            || self.instructions.len() > 32768
            || !(1..=32).contains(&self.max_turns)
        {
            return Err(Error::Contract(
                "Enter a model (up to 160 bytes), instructions (up to 32768 bytes), and 1–32 turns",
            ));
        }
        Ok(json!({"model":model,"instructions":self.instructions,"max_turns":self.max_turns}))
    }
}
#[derive(Clone)]
pub struct SavedService {
    pub id: String,
    pub framework: String,
    pub settings: Settings,
    pub key_configured: bool,
    metadata: Value,
}
fn framework(value: &str) -> bool {
    matches!(value, "openai_agents" | "claude_agent_sdk")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || (i > 0 && matches!(b, b'_' | b'-'))
        })
}
fn key(value: &str) -> Result<(), Error> {
    if !(16..=4096).contains(&value.len())
        || value.starts_with("enc:")
        || value.bytes().any(|b| b <= 32)
    {
        return Err(Error::Contract(
            "Provider key must be 16–4096 bytes without whitespace",
        ));
    }
    Ok(())
}
pub fn load_request(id: &str) -> Result<ApiRequest, Error> {
    if !valid_id(id) {
        return Err(Error::Request);
    }
    Ok(ApiRequest::get(&["api", "v1", "services", id]))
}
pub fn read_service(value: &Value, id: &str) -> Result<SavedService, Error> {
    let fail =
        || Error::Contract("Expected the selected SDK service's public configuration; reload it");
    if value["service_id"] != id || !valid_id(id) {
        return Err(fail());
    }
    let connector = &value["connector_config"];
    if connector.get("sdk_provider_api_key").is_some()
        || connector.get("sdk_provider_api_key_encrypted").is_some()
    {
        return Err(Error::Contract(
            "Server returned a credential field; configuration was discarded",
        ));
    }
    let mut metadata = value["metadata"].as_object().cloned().ok_or_else(fail)?;
    let adapter = metadata
        .get("adapter_type")
        .and_then(Value::as_str)
        .filter(|s| framework(s))
        .ok_or_else(fail)?
        .to_owned();
    let agent = metadata.get("sdk_agent").ok_or_else(fail)?;
    let turns = agent["max_turns"]
        .as_u64()
        .filter(|v| (1..=32).contains(v))
        .ok_or_else(fail)?;
    let settings = Settings {
        model: agent["model"].as_str().ok_or_else(fail)?.into(),
        instructions: agent["instructions"].as_str().ok_or_else(fail)?.into(),
        max_turns: turns as u8,
    };
    settings.value()?;
    let key_configured = metadata.remove("sdk_provider_key_configured") == Some(json!(true));
    Ok(SavedService {
        id: id.into(),
        framework: adapter,
        settings,
        key_configured,
        metadata: Value::Object(metadata),
    })
}
/// Sanitized diagnostics only; no status releases the submitted-write fence.
pub fn registration_failure(status: Option<u16>) -> &'static str {
    match status {
        Some(404 | 405 | 501) => "Atomic SDK registration is unavailable on this server/profile; no legacy registration fallback was attempted",
        Some(409) => "Server reported an SDK service ID conflict; inspect the existing service before another registration",
        _ => "SDK service registration outcome unconfirmed",
    }
}

pub fn create_request(
    id: &str,
    name: &str,
    adapter: &str,
    settings: &Settings,
    api_key: &str,
) -> Result<ApiRequest, Error> {
    if !valid_id(id) || name.trim().is_empty() || !framework(adapter) {
        return Err(Error::Request);
    }
    key(api_key)?;
    Ok(ApiRequest::mutation(
        Method::POST,
        &["api", "v1", "service-registrations"],
        Some(json!({
            "service_id":id,"name":name.trim(),"service_type":"custom","connector_type":"http",
            "status":"active","supported_modes":["sync","stream"],"accepted_content_types":["text"],
            "output_content_types":["text"],"session_enabled":true,"max_retries":0,
            "connector_config":{"sdk_provider_api_key":api_key},
            "metadata":{"adapter_type":adapter,"sdk_agent":settings.value()?}
        })),
    ))
}
pub fn update_request(
    saved: &SavedService,
    settings: &Settings,
    replacement_key: &str,
) -> Result<ApiRequest, Error> {
    let mut metadata = saved.metadata.clone();
    metadata["adapter_type"] = json!(saved.framework);
    metadata["sdk_agent"] = settings.value()?;
    let mut body = json!({"metadata":metadata});
    if !replacement_key.is_empty() {
        key(replacement_key)?;
        body["connector_config"] = json!({"sdk_provider_api_key":replacement_key});
    }
    Ok(ApiRequest::mutation(
        Method::PUT,
        &["api", "v1", "services", &saved.id],
        Some(body),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            model: "model-a".into(),
            instructions: "Help".into(),
            max_turns: 8,
        }
    }
    fn service(adapter: &str) -> Value {
        json!({"service_id":"sdk-test","connector_config":{},"metadata":{
        "adapter_type":adapter,"sdk_agent":settings().value().unwrap(),"sdk_tools":{"capability_ids":["tool-a"]},
        "unrelated":{"preserve":true},"sdk_provider_key_configured":true}})
    }
    #[test]
    fn both_frameworks_create_and_edit_with_exact_routes() {
        for adapter in ["openai_agents", "claude_agent_sdk"] {
            let create = create_request(
                "sdk-test",
                "Example",
                adapter,
                &settings(),
                "fixture-key-123456",
            )
            .unwrap();
            assert_eq!(create.method, Method::POST);
            assert_eq!(create.segments, ["api", "v1", "service-registrations"]);
            let body = create.body.unwrap();
            assert_eq!(body["metadata"]["adapter_type"], adapter);
            assert_eq!(body["session_enabled"], true);
            assert_eq!(body["max_retries"], 0);
            let saved = read_service(&service(adapter), "sdk-test").unwrap();
            assert!(saved.key_configured);
            let update = update_request(&saved, &settings(), "").unwrap();
            assert_eq!(update.method, Method::PUT);
            assert_eq!(update.segments, ["api", "v1", "services", "sdk-test"]);
            let body = update.body.unwrap();
            assert!(body.get("connector_config").is_none());
            assert_eq!(
                body["metadata"]["sdk_tools"]["capability_ids"],
                json!(["tool-a"])
            );
            assert_eq!(body["metadata"]["unrelated"]["preserve"], true);
            assert!(
                body["metadata"]
                    .get("sdk_provider_key_configured")
                    .is_none()
            );
        }
    }
    #[test]
    fn registration_failures_are_specific_without_classifying_other_statuses_as_safe() {
        for status in [404, 405, 501] {
            assert!(registration_failure(Some(status)).contains("unavailable"));
        }
        assert!(registration_failure(Some(409)).contains("conflict"));
        for status in [400, 401, 403, 408, 422, 500, 502] {
            assert!(registration_failure(Some(status)).contains("unconfirmed"));
        }
        assert!(registration_failure(None).contains("unconfirmed"));
    }
    #[test]
    fn replacement_key_is_only_in_write_request() {
        let saved = read_service(&service("openai_agents"), "sdk-test").unwrap();
        let request = update_request(&saved, &settings(), "fixture-new-key-1234").unwrap();
        assert_eq!(
            request.body.unwrap()["connector_config"]["sdk_provider_api_key"],
            "fixture-new-key-1234"
        );
        assert!(update_request(&saved, &settings(), " ").is_err());
        assert!(update_request(&saved, &settings(), "enc:fixture-key-123456").is_err());
    }
    #[test]
    fn rejects_wrong_identity_framework_and_leaked_credentials() {
        let value = service("openai_agents");
        assert!(read_service(&value, "other").is_err());
        assert!(read_service(&service("langgraph"), "sdk-test").is_err());
        for field in ["sdk_provider_api_key", "sdk_provider_api_key_encrypted"] {
            let mut value = value.clone();
            value["connector_config"][field] = json!("never-display");
            assert!(read_service(&value, "sdk-test").is_err());
        }
    }
    #[test]
    fn rejects_invalid_settings_before_any_request() {
        for turns in [0, 33] {
            let mut s = settings();
            s.max_turns = turns;
            assert!(s.value().is_err());
        }
        let mut s = settings();
        s.model = "界".repeat(54);
        assert!(s.value().is_err());
        s = settings();
        s.instructions = "x".repeat(32769);
        assert!(s.value().is_err());
        assert!(
            create_request(
                "../bad",
                "Name",
                "openai_agents",
                &settings(),
                "fixture-key-123456"
            )
            .is_err()
        );
        assert!(create_request("sdk-test", "Name", "openai_agents", &settings(), "").is_err());
    }
}
