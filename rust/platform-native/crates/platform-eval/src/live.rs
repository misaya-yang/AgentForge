use crate::{Result, cli::Args};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
fn redact(mut text: String) -> String {
    for name in ["GATEWAY_TOKEN", "GATEWAY_ADMIN_JWT", "GATEWAY_API_KEY"] {
        if let Ok(secret) = std::env::var(name) {
            if !secret.is_empty() {
                text = text.replace(&secret, "[redacted]");
            }
        }
    }
    text
}
pub fn api_root(base: &str) -> Result<String> {
    let root = base.trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(root).map_err(|_| "--base-url must be an absolute HTTP(S) URL")?;
    if !["http", "https"].contains(&parsed.scheme()) || parsed.host_str().is_none() {
        return Err("--base-url must be an absolute HTTP(S) URL".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("--base-url must not contain credentials".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("--base-url must not contain a query or fragment".into());
    }
    Ok(if root.ends_with("/api/v1/eval") {
        root.into()
    } else if root.ends_with("/api/v1") {
        format!("{root}/eval")
    } else {
        format!("{root}/api/v1/eval")
    })
}
fn quote(id: &str) -> String {
    id.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
async fn request(
    client: &reqwest::Client,
    root: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let token = std::env::var("GATEWAY_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("GATEWAY_ADMIN_JWT")
                .ok()
                .filter(|s| !s.is_empty())
        });
    let key = std::env::var("GATEWAY_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    if token.is_none() && key.is_none() {
        return Err("GATEWAY_TOKEN/GATEWAY_ADMIN_JWT or GATEWAY_API_KEY is required".into());
    }
    let mut request = client
        .request(method.clone(), format!("{root}{path}"))
        .header("Accept", "application/json");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(key) = key {
        request = request.header("X-API-Key", key);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| format!("Eval API {method} {path} failed: transport unavailable"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Eval API {method} {path} failed with HTTP {}",
            response.status().as_u16()
        )
        .into());
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| format!("Eval API {method} {path} returned invalid JSON"))?;
    if !value.is_object() {
        return Err(format!("Eval API {method} {path} returned a non-object response").into());
    }
    Ok(value)
}
pub fn compatible(v: &Value) -> bool {
    v.as_bool()
        .or_else(|| v["compatible"].as_bool())
        .or_else(|| v["is_compatible"].as_bool())
        .unwrap_or_else(|| {
            ["compatible", "pass"]
                .contains(&v["status"].as_str().unwrap_or("").to_lowercase().as_str())
        })
}
fn numeric(v: &Value) -> Value {
    json!(
        v.as_object()
            .into_iter()
            .flatten()
            .filter(|(_, v)| v.is_number() || v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<serde_json::Map<_, _>>()
    )
}
fn result(
    experiment: &str,
    baseline: &str,
    candidate: &str,
    status: &str,
    comparison: Option<&Value>,
    error: Option<String>,
) -> Value {
    let mut v = json!({"schema_version":"eval-live-regression-v1","experiment_id":experiment,"baseline_run_id":baseline,"candidate_run_id":candidate,"run_status":status});
    if let Some(error) = error {
        v["error"] = json!(redact(error));
    }
    if let Some(c) = comparison {
        v["compatible"] = json!(compatible(&c["compatibility"]));
        if c["compatibility"].is_object() {
            v["compatibility_status"] = c["compatibility"]["status"].clone();
        }
        if c["gate"].is_object() {
            v["gate"] = json!({"status":c["gate"]["status"],"failures":c["gate"]["failures"].as_array().into_iter().flatten().map(|v|redact(v.as_str().map(str::to_string).unwrap_or_else(||v.to_string()))).collect::<Vec<_>>()});
        }
        v["candidate_metrics"] = numeric(
            if c["candidate_summary"]
                .as_object()
                .is_some_and(|m| !m.is_empty())
            {
                &c["candidate_summary"]
            } else {
                &c["metrics"]
            },
        );
        v["deltas"] = numeric(&c["deltas"]);
    }
    v
}
pub async fn run(args: Args) -> Result<(Value, i32)> {
    if args.positional.len() != 2 {
        return Err("live requires experiment_id".into());
    }
    args.reject_unknown(&[
        "evaluator-id",
        "dataset-id",
        "baseline-run-id",
        "repetitions",
        "system-prompt-file",
        "base-url",
        "poll-interval",
        "timeout",
        "request-timeout",
    ])?;
    let evaluators = args
        .repeated
        .get("evaluator-id")
        .filter(|v| !v.is_empty())
        .ok_or("--evaluator-id is required")?;
    let repetitions: u32 = args.number("repetitions", 3)?;
    if !(1..=10).contains(&repetitions) {
        return Err("--repetitions must be between 1 and 10".into());
    }
    let poll: f64 = args.number("poll-interval", 2.)?;
    let timeout: f64 = args.number("timeout", 1800.)?;
    let request_timeout: f64 = args.number("request-timeout", 30.)?;
    if !poll.is_finite()
        || poll < 0.
        || !timeout.is_finite()
        || timeout <= 0.
        || !request_timeout.is_finite()
        || request_timeout <= 0.
    {
        return Err("poll interval must be non-negative and timeouts must be positive".into());
    }
    let base = std::env::var("GATEWAY_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let root = api_root(args.get("base-url", &base))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs_f64(request_timeout))
        .build()?;
    let experiment_id = &args.positional[1];
    let path = format!("/experiments/{}", quote(experiment_id));
    let experiment = request(&client, &root, reqwest::Method::GET, &path, None).await?;
    let baseline = args
        .options
        .get("baseline-run-id")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            experiment["baseline_run_id"]
                .as_str()
                .filter(|s| !s.is_empty())
        })
        .ok_or("No baseline is configured; pass --baseline-run-id or promote one first")?;
    let mut body = json!({"run_mode":"live_candidate","evaluator_ids":evaluators,"repetitions":repetitions,"baseline_run_id":baseline,"metadata":{"source":"eval_golden_live_cli"}});
    if let Some(dataset) = args.options.get("dataset-id") {
        body["dataset_id"] = json!(dataset);
    }
    if let Some(file) = args.options.get("system-prompt-file") {
        body["candidate_config"] = json!({"system_prompt_override":std::fs::read_to_string(file)?});
    }
    let started = request(
        &client,
        &root,
        reqwest::Method::POST,
        &format!("{path}:run"),
        Some(&body),
    )
    .await?;
    let mut ids = BTreeSet::new();
    if let Some(id) = started["run_id"].as_str().filter(|s| !s.is_empty()) {
        ids.insert(id.to_string());
    }
    for job in started["jobs"].as_array().into_iter().flatten() {
        if let Some(id) = job["run_id"].as_str().filter(|s| !s.is_empty()) {
            ids.insert(id.to_string());
        }
    }
    if ids.len() != 1 {
        return Err("Live experiment must create exactly one suite run".into());
    }
    let candidate = ids.into_iter().next().unwrap();
    let run_path = format!("/experiment-runs/{}", quote(&candidate));
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    loop {
        let run = request(&client, &root, reqwest::Method::GET, &run_path, None).await?;
        let status = run["status"].as_str().unwrap_or("unknown");
        let mode = run["run_mode"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| run["target_snapshot"]["run_mode"].as_str());
        if mode != Some("live_candidate") {
            return Ok((
                result(
                    experiment_id,
                    baseline,
                    &candidate,
                    status,
                    None,
                    Some("Server did not confirm live_candidate mode".into()),
                ),
                1,
            ));
        }
        if status == "succeeded" {
            break;
        }
        if ["failed", "cancelled"].contains(&status) {
            return Ok((
                result(
                    experiment_id,
                    baseline,
                    &candidate,
                    status,
                    None,
                    Some(
                        run["error_message"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("Live run {status}")),
                    ),
                ),
                1,
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok((
                result(
                    experiment_id,
                    baseline,
                    &candidate,
                    status,
                    None,
                    Some(format!("Timed out after {timeout}s")),
                ),
                1,
            ));
        }
        if poll > 0. {
            tokio::time::sleep(Duration::from_secs_f64(poll).min(remaining)).await;
        }
    }
    let comparison = request(
        &client,
        &root,
        reqwest::Method::GET,
        &format!(
            "/experiment-runs:compare?baseline_run_id={}&candidate_run_id={}",
            quote(baseline),
            quote(&candidate)
        ),
        None,
    )
    .await?;
    let passed = compatible(&comparison["compatibility"])
        && ["pass", "warning"].contains(&comparison["gate"]["status"].as_str().unwrap_or(""));
    Ok((
        result(
            experiment_id,
            baseline,
            &candidate,
            "succeeded",
            Some(&comparison),
            None,
        ),
        if passed { 0 } else { 1 },
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_url_and_compatibility() {
        assert!(api_root("https://user:pass@example.test").is_err());
        assert!(api_root("https://example.test?key=x").is_err());
        assert_eq!(
            api_root("http://127.0.0.1:8080/api/v1").unwrap(),
            "http://127.0.0.1:8080/api/v1/eval"
        );
        assert!(!compatible(&json!({"status":"unknown"})));
        assert!(compatible(&json!({"is_compatible":true})));
    }
}
