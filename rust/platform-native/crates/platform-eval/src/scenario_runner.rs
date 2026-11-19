//! Independently attested real-domain validation and merge orchestration.
use crate::{
    Result,
    canonical::{digest, sha256},
    scenario_contract as contract, scenario_evidence as evidence,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
pub fn required_env(names: &[&str]) -> Result<String> {
    for name in names {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Ok(v.trim().into());
            }
        }
    }
    Err(format!(
        "missing required environment variable: {}",
        names.join(" or ")
    )
    .into())
}
pub fn attestation_key(name: &str) -> Result<String> {
    let key = required_env(&[name])?;
    if key.len() < 32 {
        return Err(format!("{name} requires at least 32 bytes").into());
    }
    Ok(key)
}
pub fn runtime_identity() -> Result<(String, String)> {
    let attestation = required_env(&["GENERAL_AGENT_RUNTIME_ATTESTATION"])?;
    let expected =
        required_env(&["GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256"])?.to_lowercase();
    crate::seals::hash(&json!(expected), "runtime attestation digest")?;
    if sha256(attestation.as_bytes()) != expected {
        return Err("runtime attestation does not match expected digest".into());
    }
    let nonce = required_env(&["GENERAL_AGENT_SUITE_NONCE"])?.to_lowercase();
    crate::seals::hash(&json!(nonce), "suite nonce")?;
    Ok((attestation, nonce))
}
pub fn independent_keys() -> Result<(String, String)> {
    let collector = attestation_key("GENERAL_AGENT_COLLECTOR_HMAC_KEY")?;
    let golden = attestation_key("GENERAL_AGENT_GOLDEN_HMAC_KEY")?;
    if crate::seals::constant_eq(&collector, &golden) {
        return Err("collector and golden keys must differ".into());
    }
    Ok((collector, golden))
}
pub fn domain_macro(scenarios: &Value, results: &[Value]) -> Result<Value> {
    let domains = scenarios["scenarios"]
        .as_array()
        .ok_or("scenarios missing")?
        .iter()
        .map(|s| {
            (
                s["scenario_id"].as_str().unwrap_or(""),
                s["domain"].as_str().unwrap_or(""),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut counts = BTreeMap::<String, (usize, usize)>::new();
    for r in results {
        let d = domains
            .get(r["scenario_id"].as_str().unwrap_or(""))
            .ok_or("unknown result scenario")?;
        let count = counts.entry((*d).into()).or_default();
        count.0 += 1;
        count.1 += usize::from(r["trial_accepted"] == true);
    }
    if counts.is_empty() {
        return Err("domain macro requires results".into());
    }
    let rates = counts
        .values()
        .map(|(n, accepted)| *accepted as f64 / *n as f64)
        .collect::<Vec<_>>();
    Ok(
        json!({"domains":counts.iter().map(|(d,(n,a))|(d.clone(),json!({"trials":n,"accepted_trials":a,"accepted_trial_rate":*a as f64 / *n as f64,"all_trials_accepted":n==a}))).collect::<BTreeMap<_,_>>(),"macro_accepted_trial_rate":rates.iter().sum::<f64>()/rates.len()as f64,"all_domains_full_pass":counts.values().all(|(n,a)|n==a),"three_run_policy_met":scenarios["scenarios"].as_array().unwrap().iter().all(|s|s["repetitions"]==3)}),
    )
}
struct Context {
    scenarios: Value,
    observations: Value,
    sources: Vec<Value>,
    plugins: Vec<Value>,
    collector: String,
    golden: String,
    nonce: String,
    by_key: evidence::Trials,
    results: Vec<Value>,
}
fn context(root: &Path, scenario_path: &Path, observation_path: &Path) -> Result<Context> {
    let scenarios = contract::load(scenario_path)?;
    let sources = contract::verify_source_artifacts(
        &scenarios,
        scenario_path
            .canonicalize()?
            .parent()
            .ok_or("scenario directory missing")?,
    )?;
    let plugins = contract::verify_plugin_definitions(&scenarios, root)?;
    let (collector, golden) = independent_keys()?;
    let (runtime, nonce) = runtime_identity()?;
    let observations = crate::strict_json::parse(&std::fs::read(observation_path)?)?;
    let by_key = evidence::verify_observations(
        &scenarios,
        &observations,
        &sources,
        &plugins,
        observation_path,
        &collector,
        &runtime,
        &nonce,
    )?;
    let mut results = vec![];
    for ((id, _), trial) in &by_key {
        let s = scenarios["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["scenario_id"] == *id)
            .ok_or("unknown scenario")?;
        results.push(crate::scenario::validated_trial(s, trial)?);
    }
    Ok(Context {
        scenarios,
        observations,
        sources,
        plugins,
        collector,
        golden,
        nonce,
        by_key,
        results,
    })
}
pub fn validate(
    root: &Path,
    scenario_path: &Path,
    observation_path: &Path,
    output: &Path,
) -> Result<Value> {
    let c = context(root, scenario_path, observation_path)?;
    let payload = json!({"schema_version":"real-agent-golden-validation/v1","suite_id":c.scenarios["suite_id"],"scenario_contract_sha256":digest(&c.scenarios),"observations_sha256":c.observations["observations_sha256"],"validator":{"version":"real-domain-golden/v1","executable_sha256":sha256(include_bytes!("scenario_runner.rs")),"semantic_model_used":false},"source_artifacts":c.sources,"plugin_definitions":c.plugins,"collector_attestation_key_id":&sha256(c.collector.as_bytes())[..24],"domain_macro":domain_macro(&c.scenarios,&c.results)?,"trials":c.results});
    let document = crate::seals::seal(&payload, Some(&c.golden))?;
    crate::seals::write_private(output, &document)?;
    Ok(document)
}
pub fn merged_binding(document: &Value, golden: bool) -> Value {
    let p = &document["provenance"];
    let mut binding = json!({"schema_version":"real-agent-validated-receipts/v1","suite_id":document["suite_id"]});
    for f in [
        "scenario_contract_sha256",
        "observations_sha256",
        "runtime_binding_sha256",
        "raw_sse_artifact_sha256",
        "provider_observer_sha256",
        "suite_nonce_sha256",
        "coding_host_test_evidence",
    ] {
        binding[f] = p[f].clone()
    }
    if golden {
        binding["validation_sha256"] = p["validation_sha256"].clone()
    }
    binding["trials"]=json!(document["trials"].as_array().into_iter().flatten().map(|t|{let r=&t[if golden{"golden_validation"}else{"observation"}];let mut v=json!({"scenario_id":r["scenario_id"],"trial":r["trial"],"observation_sha256":r["observation_sha256"]});if golden{v["golden_validation_sha256"]=json!(digest(r))}v}).collect::<Vec<_>>());
    binding
}
pub fn merge(
    root: &Path,
    scenario_path: &Path,
    observation_path: &Path,
    validation_path: &Path,
    output: &Path,
    coding_receipt: Option<&Path>,
) -> Result<Value> {
    let c = context(root, scenario_path, observation_path)?;
    let validation = crate::strict_json::parse(&std::fs::read(validation_path)?)?;
    let v = crate::seals::verify_document(&validation, &c.golden)?;
    if v["schema_version"] != "real-agent-golden-validation/v1"
        || v["suite_id"] != c.scenarios["suite_id"]
        || v["scenario_contract_sha256"] != digest(&c.scenarios)
        || v["observations_sha256"] != c.observations["observations_sha256"]
        || v["source_artifacts"] != json!(c.sources)
        || v["plugin_definitions"] != json!(c.plugins)
        || v["collector_attestation_key_id"] != sha256(c.collector.as_bytes())[..24]
    {
        return Err("validation identity/source binding mismatch".into());
    }
    if v["trials"] != json!(c.results)
        || v["domain_macro"] != domain_macro(&c.scenarios, &c.results)?
    {
        return Err("validation differs from host recomputation".into());
    }
    let merged = c
        .by_key
        .values()
        .zip(&c.results)
        .map(|(o, r)| json!({"observation":o,"golden_validation":r}))
        .collect::<Vec<_>>();
    let mut coding = Value::Null;
    if let Some(s) = c.scenarios["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["scenario_id"] == crate::coding_receipt::SCENARIO_ID)
    {
        let path =
            coding_receipt.ok_or("coding release merge requires --coding-host-test-receipt")?;
        let key = attestation_key(crate::coding_receipt::HMAC_ENVIRONMENT_NAME)?;
        if key == c.collector || key == c.golden {
            return Err("coding, collector and golden keys must differ".into());
        }
        let policy = crate::coding_receipt::validator_policy(
            &root.join(
                "tests/fixtures/eval/general_agent/coding_parallel_v2/validator_contract.json",
            ),
            &root.join("rust/platform-native/crates/platform-eval/src/coding_sandbox.rs"),
        )?;
        let mut trials = BTreeMap::new();
        for n in 1..=3 {
            let o = c
                .by_key
                .get(&(crate::coding_receipt::SCENARIO_ID.into(), n))
                .ok_or("coding merge requires trials 1, 2, 3")?;
            trials.insert(n,json!({"observation_sha256":o["observation_sha256"],"execution_checks":crate::scenario::execution_checks(s,o)}));
        }
        let scenario_hash = digest(&c.scenarios);
        let collector_id = sha256(c.collector.as_bytes());
        coding = crate::coding_receipt::verify_receipt(
            path,
            &key,
            &policy,
            &crate::coding_receipt::Expected {
                scenario_contract_sha256: &scenario_hash,
                observations_sha256: c.observations["observations_sha256"]
                    .as_str()
                    .ok_or("observations digest missing")?,
                collector_attestation_key_id: &collector_id[..24],
                trials: &trials,
                source_artifacts: Some(&json!(c.sources)),
                plugin_definitions: Some(&json!(c.plugins)),
                runtime_binding: Some(&c.observations["runtime_binding"]),
                raw_sse_artifact: Some(&c.observations["raw_sse_artifact"]),
            },
        )?;
    }
    let provider = digest(
        &json!({"collector":c.observations["collector"],"runtime_binding":c.observations["runtime_binding"],"source_artifacts":c.observations["source_artifacts"],"plugin_definitions":c.observations["plugin_definitions"],"raw_sse_artifact":c.observations["raw_sse_artifact"],"trials":c.observations["trials"].as_array().unwrap().iter().map(|t|json!({"scenario_id":t["scenario_id"],"trial":t["trial"],"session_id":t["session_id"],"stream_sha256":t["stream_sha256"],"observation_sha256":t["observation_sha256"]})).collect::<Vec<_>>()}),
    );
    let mut doc = json!({"schema_version":"real-agent-validated-receipts/v1","suite_id":c.scenarios["suite_id"],"provenance":{"scenario_contract_sha256":digest(&c.scenarios),"observations_sha256":c.observations["observations_sha256"],"runtime_binding_sha256":digest(&c.observations["runtime_binding"]),"raw_sse_artifact_sha256":c.observations["raw_sse_artifact"]["content_sha256"],"provider_observer_sha256":provider,"suite_nonce_sha256":sha256(c.nonce.as_bytes()),"validation_sha256":validation["seal"]["digest"],"validation_seal_strength":"hmac-sha256","coding_host_test_evidence":coding},"trials":merged});
    doc["provenance"]["collector_attestation"] =
        crate::scenario_events::attestation(&merged_binding(&doc, false), &c.collector)?;
    doc["provenance"]["golden_attestation"] =
        crate::scenario_events::attestation(&merged_binding(&doc, true), &c.golden)?;
    doc["merged_receipt_sha256"] = json!(digest(&doc));
    crate::seals::write_private(output, &doc)?;
    Ok(doc)
}
pub async fn collect(root: &Path, scenario_path: &Path, output: &Path) -> Result<Value> {
    let scenarios = contract::load(scenario_path)?;
    let sources = contract::verify_source_artifacts(
        &scenarios,
        scenario_path
            .canonicalize()?
            .parent()
            .ok_or("scenario directory missing")?,
    )?;
    let plugins = contract::verify_plugin_definitions(&scenarios, root)?;
    let key = attestation_key("GENERAL_AGENT_COLLECTOR_HMAC_KEY")?;
    let (runtime, nonce) = runtime_identity()?;
    let raw_output = output.with_file_name(format!(
        "{}.raw-sse.json",
        output
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("output filename missing")?
    ));
    let base = std::env::var("GENERAL_AGENT_GATEWAY_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".into())
        .trim_end_matches('/')
        .to_owned();
    let api = format!("{base}/api/v1");
    let email = required_env(&["GENERAL_AGENT_GATEWAY_EMAIL", "ASSISTANT_ISOLATION_EMAIL"])?;
    let password = required_env(&[
        "GENERAL_AGENT_GATEWAY_PASSWORD",
        "ASSISTANT_ISOLATION_PASSWORD",
    ])?;
    let model = std::env::var("GENERAL_AGENT_CANDIDATE_MODEL")
        .unwrap_or_else(|_| "qwen3.7-plus".into())
        .trim()
        .to_owned();
    let timeout = std::env::var("GENERAL_AGENT_GATEWAY_TIMEOUT_S")
        .unwrap_or_else(|_| "300".into())
        .parse::<f64>()?;
    if model.is_empty() || !(5.0..=900.0).contains(&timeout) {
        return Err("invalid candidate model or Gateway timeout".into());
    }
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs_f64(timeout))
        .build()?;
    let login: Value = client
        .post(format!("{api}/auth/login"))
        .json(&json!({"email":email,"password":password}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = login["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("login did not return access token")?;
    let health: Value = client
        .get(format!("{base}/health"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tools: Value = client
        .get(format!("{api}/assistant/tools"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let binding = json!({"gateway_health_sha256":digest(&crate::scenario_events::redact(&health)),"authenticated_tool_catalog_sha256":digest(&crate::scenario_events::redact(&tools)),"operator_container_runtime_attestation_sha256":sha256(runtime.as_bytes())});
    let (mut trials, mut raw_trials) = (vec![], vec![]);
    for s in scenarios["scenarios"].as_array().unwrap() {
        for trial in 1..=s["repetitions"].as_i64().unwrap() {
            let session = uuid::Uuid::new_v4().to_string();
            let mut random = [0u8; 32];
            getrandom::fill(&mut random)
                .map_err(|e| format!("collector challenge entropy unavailable: {e}"))?;
            let challenge = random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            let prompt = crate::scenario::candidate_prompt(s);
            let mut response=client.post(format!("{api}/assistant/chat/stream")).bearer_auth(token).json(&json!({"session_id":session,"message":prompt,"model_id":s.get("model_id").cloned().unwrap_or(json!(model)),"temperature":0.0,"max_tokens":s.get("max_tokens").cloned().unwrap_or(json!(3000)),"kb_mode":"off","web_search_enabled":false,"execution_profile":s.get("execution_profile").cloned().unwrap_or(json!("safe")),"memory_mode":"off","os_agent_enabled":false})).send().await?.error_for_status()?;
            let (mut payloads, mut events, mut bytes, mut pending) =
                (vec![], vec![], vec![], vec![]);
            let mut terminal = false;
            while let Some(chunk) = response.chunk().await? {
                pending.extend_from_slice(&chunk);
                while let Some(index) = pending.iter().position(|b| *b == b'\n') {
                    let raw_line = pending.drain(..=index).collect::<Vec<_>>();
                    let line = std::str::from_utf8(&raw_line)?.trim_end_matches(['\r', '\n']);
                    if let Some(raw) = line
                        .strip_prefix("data: ")
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        payloads.push(raw.to_owned());
                        bytes.extend_from_slice(raw.as_bytes());
                        bytes.push(b'\n');
                        if let Ok(event) = crate::strict_json::parse(raw.as_bytes()) {
                            if event.is_object() {
                                terminal = crate::scenario_events::TERMINALS
                                    .contains(&event["event_type"].as_str().unwrap_or(""));
                                events.push(event);
                            }
                        }
                        if terminal {
                            break;
                        }
                    }
                }
                if terminal {
                    break;
                }
            }
            if !terminal && !pending.is_empty() {
                let line = std::str::from_utf8(&pending)?.trim_end_matches('\r');
                if let Some(raw) = line
                    .strip_prefix("data: ")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    payloads.push(raw.to_owned());
                    bytes.extend_from_slice(raw.as_bytes());
                    bytes.push(b'\n');
                    if let Ok(event) = crate::strict_json::parse(raw.as_bytes()) {
                        if event.is_object() {
                            events.push(event)
                        }
                    }
                }
            }
            let stream_hash = sha256(&bytes);
            let mut observation = crate::scenario_events::summarize(&events, &stream_hash);
            for (field, value) in [
                ("scenario_id", s["scenario_id"].clone()),
                ("trial", json!(trial)),
                ("session_id", json!(session)),
                ("suite_nonce", json!(nonce)),
                ("collector_challenge", json!(challenge)),
                ("prompt_sha256", json!(sha256(prompt.as_bytes()))),
            ] {
                observation[field] = value
            }
            observation["observation_sha256"] = json!(digest(&observation));
            let mut raw = json!({"scenario_id":s["scenario_id"],"trial":trial,"session_id":session,"suite_nonce":nonce,"collector_challenge":challenge,"raw_sse_payloads":payloads,"stream_sha256":stream_hash});
            raw["raw_trial_sha256"] = json!(digest(&raw));
            trials.push(observation);
            raw_trials.push(raw);
        }
    }
    let mut raw = json!({"schema_version":"real-agent-raw-sse/v1","suite_id":scenarios["suite_id"],"suite_nonce":nonce,"trials":raw_trials});
    raw["raw_sse_sha256"] = json!(digest(&raw));
    raw["collector_attestation"] = crate::scenario_events::attestation(&raw, &key)?;
    crate::seals::write_private(&raw_output, &raw)?;
    let mut document = json!({"schema_version":"real-agent-observations/v1","suite_id":scenarios["suite_id"],"scenario_contract_sha256":digest(&scenarios),"suite_nonce":nonce,"collector":{"transport":"gateway-sse","candidate_model_default":model,"semantic_verdicts_emitted":false},"runtime_binding":binding,"source_artifacts":sources,"plugin_definitions":plugins,"raw_sse_artifact":{"file_name":raw_output.file_name().and_then(|s|s.to_str()).ok_or("raw filename missing")?,"content_sha256":sha256(&std::fs::read(&raw_output)?),"mode":"0600"},"trials":trials});
    document["observations_sha256"] = json!(digest(&document));
    document["collector_attestation"] = crate::scenario_events::attestation(&document, &key)?;
    crate::seals::write_private(output, &document)?;
    Ok(document)
}
pub async fn cli(args: &[String]) -> Result<()> {
    let args = crate::cli::Args::parse(args.iter().cloned(), &[])?;
    let command = args
        .positional
        .first()
        .map(String::as_str)
        .ok_or("collect, validate, or merge required")?;
    let root = std::env::current_dir()?;
    let scenario = Path::new(args.required("scenarios")?);
    let output = Path::new(args.required("output")?);
    match command {
        "collect" => {
            args.reject_unknown(&["scenarios", "output"])?;
            collect(&root, scenario, output).await?;
        }
        "validate" => {
            args.reject_unknown(&["scenarios", "observations", "output"])?;
            validate(
                &root,
                scenario,
                Path::new(args.required("observations")?),
                output,
            )?;
        }
        "merge" => {
            args.reject_unknown(&[
                "scenarios",
                "observations",
                "validation",
                "output",
                "coding-host-test-receipt",
            ])?;
            merge(
                &root,
                scenario,
                Path::new(args.required("observations")?),
                Path::new(args.required("validation")?),
                output,
                args.options
                    .get("coding-host-test-receipt")
                    .map(|s| Path::new(s)),
            )?;
        }
        _ => return Err("unsupported scenario command".into()),
    }
    Ok(())
}
