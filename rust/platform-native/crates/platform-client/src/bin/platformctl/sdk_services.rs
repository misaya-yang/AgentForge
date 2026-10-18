//! Typed, one-attempt SDK service configuration for the management CLI.
use super::*;
use ai_platform_client::sdk_services::{self as sdk, SavedService, Settings};
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    name: Option<String>,
    framework: Option<String>,
    model: Option<String>,
    instructions: Option<String>,
    max_turns: Option<u8>,
}

pub(super) enum Action {
    Get(String),
    Configure {
        id: String,
        create: bool,
        input: Input,
        key: String,
        apply: bool,
    },
}

fn read_file(path: &str, limit: u64) -> Result<String, &'static str> {
    if !std::fs::metadata(path)
        .map_err(|_| "Could not inspect input file")?
        .is_file()
    {
        return Err("Input must be a regular file");
    }
    let file = std::fs::File::open(path).map_err(|_| "Could not open input file")?;
    if !file
        .metadata()
        .map_err(|_| "Could not inspect input file")?
        .is_file()
    {
        return Err("Input must be a regular file");
    }
    let mut value = String::new();
    file.take(limit + 1)
        .read_to_string(&mut value)
        .map_err(|_| "Could not read UTF-8 input file")?;
    if value.len() as u64 > limit {
        return Err("Input file exceeds its size limit");
    }
    Ok(value)
}
fn decode(text: &str, create: bool) -> Result<Input, &'static str> {
    let input: Input = serde_json::from_str(text)
        .map_err(|_| "Invalid SDK settings JSON; only documented typed fields are accepted")?;
    if create
        && (input.name.as_ref().is_none_or(|s| s.trim().is_empty())
            || input.framework.is_none()
            || input.model.is_none())
    {
        return Err("Create requires name, framework, and model");
    }
    if !create && (input.name.is_some() || input.framework.is_some()) {
        return Err(
            "Edit accepts only model, instructions, and max_turns; framework cannot change",
        );
    }
    Ok(input)
}
fn merged(input: &Input, base: &Settings) -> Settings {
    Settings {
        model: input.model.clone().unwrap_or_else(|| base.model.clone()),
        instructions: input
            .instructions
            .clone()
            .unwrap_or_else(|| base.instructions.clone()),
        max_turns: input.max_turns.unwrap_or(base.max_turns),
    }
}
fn projection(saved: &SavedService) -> Value {
    json!({"service_id":saved.id,"framework":saved.framework,"model":saved.settings.model,
        "instructions":saved.settings.instructions,"max_turns":saved.settings.max_turns,
        "provider_key_configured":saved.key_configured})
}
pub(super) fn parse(args: &mut Arguments, words: &[&str]) -> Result<Action, &'static str> {
    let action = match words {
        ["sdk-services", "get", id] => {
            sdk::load_request(id).map_err(|_| "Invalid SDK service ID")?;
            if args.yes {
                return Err("Get does not accept --yes");
            }
            Action::Get((*id).into())
        }
        ["sdk-services", "create"] | ["sdk-services", "edit", _] => {
            let create = words[1] == "create";
            let id = if create {
                // Fresh invocation identity; the create-only endpoint enforces atomic absence.
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| "Could not generate a service identity")?
                    .as_nanos();
                format!("sdk-{nanos:x}-{:x}", std::process::id())
            } else {
                words[2].to_owned()
            };
            sdk::load_request(&id).map_err(|_| "Invalid SDK service ID")?;
            let path = args
                .options
                .remove("--file")
                .ok_or("Provide --file with typed SDK settings JSON")?;
            let input = decode(&read_file(&path, 65536)?, create)?;
            let key = args
                .options
                .remove("--provider-key-file")
                .map(|path| read_file(&path, 4098))
                .transpose()?
                .map(|s| s.trim_end_matches(['\r', '\n']).to_owned());
            if key.as_ref().is_some_and(String::is_empty) {
                return Err("An explicit provider key file must not be empty");
            }
            if create && key.is_none() {
                return Err("Create requires --provider-key-file");
            }
            let key = key.unwrap_or_default();
            if !create
                && input.model.is_none()
                && input.instructions.is_none()
                && input.max_turns.is_none()
                && key.is_empty()
            {
                return Err("Edit requires at least one settings field or replacement key");
            }
            if !create {
                let base = Settings {
                    model: "validation-only".into(),
                    ..Settings::default()
                };
                sdk::create_request(
                    "sdk-validation",
                    "Validation",
                    "openai_agents",
                    &merged(&input, &base),
                    if key.is_empty() {
                        "validation-placeholder"
                    } else {
                        &key
                    },
                )
                .map_err(|_| "Invalid SDK settings or provider key")?;
            }
            if create {
                sdk::create_request(
                    &id,
                    input.name.as_deref().unwrap(),
                    input.framework.as_deref().unwrap(),
                    &merged(&input, &Settings::default()),
                    &key,
                )
                .map_err(|_| "Invalid SDK settings or provider key")?;
            }
            Action::Configure {
                id,
                create,
                input,
                key,
                apply: args.yes,
            }
        }
        _ => return Err("Use sdk-services get ID, create, or edit ID"),
    };
    if !args.options.is_empty() {
        return Err("Option is not supported by this command");
    }
    Ok(action)
}
fn matches_review(saved: &SavedService, review: &Value) -> bool {
    let instructions = review["instructions"].as_str().unwrap_or_default();
    let expected_instructions = if instructions.trim().is_empty() {
        "You are a helpful assistant."
    } else {
        instructions
    };
    saved.id == review["service_id"].as_str().unwrap_or_default()
        && saved.framework == review["framework"].as_str().unwrap_or_default()
        && saved.settings.model == review["model"].as_str().unwrap_or_default().trim()
        && saved.settings.instructions == expected_instructions
        && Some(saved.settings.max_turns as u64) == review["max_turns"].as_u64()
        && (review["provider_key_action"] == "preserve" || saved.key_configured)
}
async fn load(client: &Client, id: &str) -> Result<SavedService, Error> {
    sdk::read_service(&client.execute(sdk::load_request(id)?).await?, id)
}
fn absence(result: Result<Value, Error>) -> Result<(), Error> {
    match result {
        Err(error) if error.status() == Some(404) => Ok(()),
        Ok(_) => Err(Error::Contract(
            "Generated service ID already exists; no write submitted",
        )),
        Err(error) => Err(error),
    }
}
pub(super) async fn execute(
    client: &Client,
    action: Action,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        Action::Get(id) => output(&projection(&load(client, &id).await?))?,
        Action::Configure {
            id,
            create,
            input,
            key,
            apply,
        } => {
            let (request, review) = if create {
                absence(client.execute(sdk::load_request(&id)?).await)?;
                let settings = merged(&input, &Settings::default());
                let framework = input.framework.as_deref().unwrap();
                let request = sdk::create_request(
                    &id,
                    input.name.as_deref().unwrap(),
                    framework,
                    &settings,
                    &key,
                )?;
                let review = json!({"service_id":id,"name":input.name,"framework":framework,"model":settings.model,"instructions":settings.instructions,"max_turns":settings.max_turns,"provider_key_action":"set","mode":if apply {"apply"} else {"review"}});
                (request, review)
            } else {
                let saved = load(client, &id).await?;
                let settings = merged(&input, &saved.settings);
                let request = sdk::update_request(&saved, &settings, &key)?;
                let mut review = projection(&saved);
                review["model"] = json!(settings.model);
                review["instructions"] = json!(settings.instructions);
                review["max_turns"] = json!(settings.max_turns);
                review["provider_key_action"] = json!(if key.is_empty() {
                    "preserve"
                } else {
                    "replace"
                });
                review["mode"] = json!(if apply { "apply" } else { "review" });
                (request, review)
            };
            drop(key);
            if !apply {
                output(&review)?;
                return Ok(());
            }
            // Record the exact recovery target before dispatch, including interruption/output failure.
            writeln!(
                std::io::stderr().lock(),
                "Submitting once for service {id}; recover with sdk-services get {id}"
            )?;
            if let Err(error) = client.execute(request).await {
                // Even 400 can follow persistence; do not classify any attempted write as safely retryable.
                let reason = if create {
                    sdk::registration_failure(error.status())
                } else {
                    "SDK service write outcome unconfirmed"
                };
                return Err(format!("{reason}; use sdk-services get {id}. Do not repeat create or edit automatically").into());
            }
            match load(client, &id).await {
                Ok(saved) => {
                    let matched = matches_review(&saved, &review);
                    output(&json!({"write_acknowledged":true,"settings_match":matched,"service":projection(&saved)}))?;
                    if !matched { return Err(format!("Readback differs from requested SDK settings; inspect sdk-services get {id}; do not automatically replay").into()); }
                },
                Err(_) => return Err(format!("SDK service write acknowledged but readback unavailable; use sdk-services get {id}").into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_input_rejects_secrets_unknowns_and_framework_edits() {
        for field in [
            "api_key",
            "sdk_provider_api_key",
            "connector_config",
            "metadata",
            "service_id",
        ] {
            assert!(decode(&format!(r#"{{"{field}":"secret"}}"#), false).is_err());
        }
        assert!(decode(r#"{"framework":"openai_agents"}"#, false).is_err());
        assert!(
            decode(
                r#"{"name":"Demo","framework":"openai_agents","model":"m"}"#,
                true
            )
            .is_ok()
        );
        assert!(decode(r#"{"model":"m"}"#, true).is_err());
    }
    #[test]
    fn edit_preserves_omitted_fields_and_allows_clearing_instructions() {
        let base = Settings {
            model: "model-a".into(),
            instructions: "existing".into(),
            max_turns: 8,
        };
        let value = merged(
            &decode(r#"{"instructions":"","max_turns":12}"#, false).unwrap(),
            &base,
        );
        assert_eq!(value.model, "model-a");
        assert_eq!(value.instructions, "");
        assert_eq!(value.max_turns, 12);
    }
    #[test]
    fn create_uses_atomic_route_and_sanitized_status_messages() {
        let request = sdk::create_request(
            "sdk-test",
            "Demo",
            "openai_agents",
            &Settings {
                model: "m".into(),
                ..Settings::default()
            },
            "fixture-provider-key",
        )
        .unwrap();
        assert_eq!(request.segments, ["api", "v1", "service-registrations"]);
        for status in [404, 405, 501] {
            assert!(sdk::registration_failure(Some(status)).contains("unavailable"));
        }
        for status in [400, 401, 403, 408, 422, 500] {
            assert!(sdk::registration_failure(Some(status)).contains("unconfirmed"));
        }
        assert!(sdk::registration_failure(Some(409)).contains("conflict"));
    }
    #[test]
    fn only_exact_not_found_permits_create() {
        assert!(absence(Err(Error::Status(404))).is_ok());
        for status in [400, 401, 403, 409, 500] {
            assert!(absence(Err(Error::Status(status))).is_err());
        }
        assert!(absence(Err(Error::Transport)).is_err());
        assert!(absence(Ok(json!({}))).is_err());
    }
    #[test]
    fn allowlist_projection_excludes_unrelated_fields() {
        let saved=sdk::read_service(&json!({"service_id":"sdk-test","connector_config":{},"metadata":{"adapter_type":"openai_agents","sdk_agent":{"model":"m","instructions":"i","max_turns":8},"private":"never-output","sdk_provider_key_configured":true}}),"sdk-test").unwrap();
        assert!(!projection(&saved).to_string().contains("never-output"));
        assert_eq!(projection(&saved)["provider_key_configured"], true);
    }
    #[test]
    fn readback_matches_server_normalization_and_rejects_changed_fields() {
        let mut saved=sdk::read_service(&json!({"service_id":"sdk-test","connector_config":{},"metadata":{"adapter_type":"openai_agents","sdk_agent":{"model":"m","instructions":"You are a helpful assistant.","max_turns":8},"sdk_provider_key_configured":true}}),"sdk-test").unwrap();
        let review = json!({"service_id":"sdk-test","framework":"openai_agents","model":" m ","instructions":" ","max_turns":8,"provider_key_action":"set"});
        assert!(matches_review(&saved, &review));
        saved.settings.max_turns = 9;
        assert!(!matches_review(&saved, &review));
        saved.settings.max_turns = 8;
        saved.key_configured = false;
        assert!(!matches_review(&saved, &review));
    }
    #[test]
    fn bounded_file_and_parser_paths_do_not_touch_transport() {
        let dir = std::env::temp_dir().join(format!("cli-sdk-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("settings.json");
        let key = dir.join("key.txt");
        std::fs::write(&key, "fixture-provider-key-1234\n").unwrap();
        let parse_file = |action: &str, yes: bool, with_key: bool| {
            let mut args = Arguments::parse(vec![
                "--origin".into(),
                "https://gateway.example".into(),
                "--token-stdin".into(),
            ])
            .unwrap();
            args.yes = yes;
            args.options
                .insert("--file".into(), config.to_str().unwrap().into());
            if with_key {
                args.options
                    .insert("--provider-key-file".into(), key.to_str().unwrap().into());
            }
            if action == "create" {
                parse(&mut args, &["sdk-services", "create"])
            } else {
                parse(&mut args, &["sdk-services", "edit", "sdk-test"])
            }
        };
        for framework in ["openai_agents", "claude_agent_sdk"] {
            std::fs::write(
                &config,
                format!(r#"{{"name":"Demo","framework":"{framework}","model":"m"}}"#),
            )
            .unwrap();
            assert!(matches!(
                parse_file("create", false, true).unwrap(),
                Action::Configure { apply: false, .. }
            ));
            assert!(matches!(
                parse_file("create", true, true).unwrap(),
                Action::Configure { apply: true, .. }
            ));
            assert!(parse_file("create", true, false).is_err());
        }
        for invalid in [
            r#"{"model":""}"#,
            r#"{"max_turns":0}"#,
            r#"{"max_turns":33}"#,
            r#"{"sdk_provider_api_key":"never-print"}"#,
            r#"{}"#,
        ] {
            std::fs::write(&config, invalid).unwrap();
            assert!(parse_file("edit", true, false).is_err());
        }
        std::fs::write(&config, r#"{"instructions":""}"#).unwrap();
        assert!(parse_file("edit", true, false).is_ok());
        std::fs::write(&key, "").unwrap();
        assert!(parse_file("edit", true, true).is_err());
        std::fs::write(&key, " ").unwrap();
        assert!(parse_file("edit", true, true).is_err());
        assert!(read_file(dir.to_str().unwrap(), 64).is_err());
        std::fs::write(&config, "x".repeat(65537)).unwrap();
        assert!(parse_file("edit", false, false).is_err());
    }
}

#[cfg(test)]
mod problem_absence_tests {
    use super::*;
    fn problem(status: u16) -> Error {
        let bytes=serde_json::to_vec(&json!({"type":"about:blank","title":"private-marker","status":status,"detail":"private-marker"})).unwrap();
        Error::Problem(Box::new(
            ai_platform_client::problem::RemoteProblem::decode(status, &bytes).unwrap(),
        ))
    }
    #[test]
    fn only_problem_not_found_confirms_absence_without_swallowing_uncertainty() {
        assert!(absence(Err(problem(404))).is_ok());
        for status in [400, 401, 403, 408, 409, 422, 429, 500, 503, 599] {
            let error = absence(Err(problem(status))).unwrap_err();
            assert_eq!(error.status(), Some(status));
            assert!(matches!(&error, Error::Problem(_)));
            assert!(!error.to_string().contains("private-marker"));
        }
        assert!(matches!(
            absence(Err(Error::UnknownOutcome)),
            Err(Error::UnknownOutcome)
        ));
        assert!(absence(Ok(json!({}))).is_err());
    }
}
