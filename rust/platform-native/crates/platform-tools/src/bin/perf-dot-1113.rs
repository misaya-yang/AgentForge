//! Offline report assembly plus explicitly configured live load probes. Never starts services.
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("missing required environment: {name}").into())
}
fn local_url(name: &str) -> Result<String> {
    let value = required(name)?;
    let url = reqwest::Url::parse(&value)?;
    if !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(format!("{name} must identify the existing loopback benchmark stack").into());
    }
    Ok(value)
}
fn percentile(value: &Value) -> Result<f64> {
    if let Some(n) = value["latencyPercentiles"]["p95"].as_f64() {
        return Ok(n * 1000.0);
    }
    if let Some(rows) = value["latencyDistribution"].as_array() {
        for row in rows {
            if row["percentage"].as_f64() == Some(95.0) {
                return row["latency"]
                    .as_f64()
                    .map(|n| n * 1000.0)
                    .ok_or_else(|| "invalid oha p95".into());
            }
        }
    }
    Err("oha output lacks a measured p95 percentile".into())
}
fn probe(url: &str, body: Option<&str>, token: Option<&str>) -> Result<f64> {
    let mut command = Command::new("oha");
    command.args(["--no-tui", "--json", "-n", "1000", "-c", "8"]);
    if let Some(body) = body {
        command.args([
            "-m",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
        ]);
    }
    if let Some(token) = token {
        command
            .arg("-H")
            .arg(format!("Authorization: Bearer {token}"));
    }
    command.arg(url);
    let output = command.output()?;
    if !output.status.success() {
        return Err("oha failed; raw output withheld to protect runtime inputs".into());
    }
    let data: Value = serde_json::from_slice(&output.stdout)?;
    let statuses = data["statusCodeDistribution"]
        .as_object()
        .ok_or("oha lacks status-code evidence")?;
    if statuses.is_empty()
        || statuses
            .iter()
            .any(|(code, count)| !code.starts_with('2') && count.as_u64().unwrap_or(0) > 0)
    {
        return Err("load probe returned non-success responses".into());
    }
    if statuses
        .values()
        .map(|count| count.as_u64().unwrap_or(0))
        .sum::<u64>()
        != 1000
    {
        return Err("load probe did not complete all 1000 successful responses".into());
    }
    percentile(&data)
}
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "performance gate failed; configure the existing benchmark stack, oha, fixtures and generated CWV samples (credential and response values withheld)"
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: perf-dot-1113 ROOT YYYY-MM-DD".into());
    }
    let root = PathBuf::from(&args[0]).canonicalize()?;
    let date = chrono::NaiveDate::parse_from_str(&args[1], "%Y-%m-%d")?;
    let token = required("PERF_PLATFORM_API_KEY")?;
    let gateway = local_url("PERF_GATEWAY_CHAT_URL")?;
    let upstream = local_url("PERF_MOCK_UPSTREAM_CHAT_URL")?;
    if std::env::var("PERF_UPSTREAM_IS_MOCK").as_deref() != Ok("1") {
        return Err("mock-upstream attestation required".into());
    }
    let retrieval = local_url("PERF_RETRIEVAL_URL")?;
    let spaces = local_url("PERF_SPACES_URL")?;
    let retrieval_body = required("PERF_RETRIEVAL_BODY")?;
    let retrieval_config: Value = serde_json::from_str(&retrieval_body)?;
    if retrieval_config["rerank"] != false {
        return Err("retrieval benchmark must explicitly disable rerank".into());
    }
    let body=json!({"model":std::env::var("PERF_MODEL").unwrap_or_else(|_|"perf-mock".into()),"messages":[{"role":"user","content":"performance fixture"}],"stream":false}).to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .no_proxy()
        .build()?;
    // Count actual distinct records from the complete cursor walk; a caller-supplied count is not evidence.
    let mut next = Some(spaces.clone());
    let mut count = 0;
    let mut cursors = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    while let Some(url) = next.take() {
        let response = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let items = response["items"]
            .as_array()
            .ok_or("space fixture lacks item rows")?;
        for item in items {
            if let Some(id) = item["item_id"].as_str() {
                ids.insert(id.to_owned());
            }
        }
        count += items.len();
        if let Some(cursor) = response["next_cursor"].as_str() {
            if !cursors.insert(cursor.to_owned()) {
                return Err("space cursor repeated".into());
            }
            let mut url = reqwest::Url::parse(&spaces)?;
            url.query_pairs_mut().append_pair("cursor", cursor);
            next = Some(url.to_string());
        }
        if count > 100000 {
            return Err("bounded fixture count exceeded".into());
        }
    }
    if ids.len() < 10000 {
        return Err("space benchmark requires at least 10000 actual distinct items".into());
    }
    let upstream_p95 = probe(&upstream, Some(&body), None)?;
    let gateway_p95 = probe(&gateway, Some(&body), Some(&token))?;
    let overhead = gateway_p95 - upstream_p95;
    let retrieval_p95 = probe(&retrieval, Some(&retrieval_body), Some(&token))?;
    let spaces_p95 = probe(&spaces, None, Some(&token))?;
    let mut metrics = Vec::new();
    for route in ["workbench", "assistant", "knowledge", "spaces"] {
        let mut samples = Vec::new();
        for run in 1..=5 {
            let value: Value = serde_json::from_slice(&fs::read(
                root.join(format!("reports/perf-dot_1113/cwv/{route}-{run}.json")),
            )?)?;
            if value["route"] != format!("/{route}") {
                return Err("CWV route identity differs".into());
            }
            samples.push(value);
        }
        metrics.push(json!({"route":route,"samples":samples}));
    }
    let result = json!({"date":date.to_string(),"gateway_p95_ms":gateway_p95,"mock_upstream_p95_ms":upstream_p95,"gateway_p95_delta_ms":overhead,"retrieval_p95_ms":retrieval_p95,"spaces_p95_ms":spaces_p95,"space_fixture_distinct_items":ids.len(),"cwv":metrics});
    let passed = gateway_p95 < 15.0
        && retrieval_p95 < 300.0
        && spaces_p95 < 150.0
        && metrics.iter().all(|route| {
            route["samples"].as_array().unwrap().iter().all(|s| {
                s["lcp_ms"].as_f64().is_some_and(|n| n > 0.0 && n < 2500.0)
                    && s["inp_ms"].as_f64().is_some_and(|n| n >= 16.0 && n < 200.0)
                    && s["cls"].as_f64().is_some_and(|n| (0.0..0.1).contains(&n))
            })
        });
    let reports = root.join("reports");
    fs::create_dir_all(&reports)?;
    fs::write(
        reports.join(format!("perf-dot_1113-{date}.json")),
        serde_json::to_vec_pretty(&result)?,
    )?;
    let baseline = std::env::var("PERF_BASELINE_JSON")
        .ok()
        .map(|path| {
            fs::read(path)
                .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(std::io::Error::other))
        })
        .transpose()?;
    let before = |key: &str| {
        baseline
            .as_ref()
            .and_then(|v| v[key].as_f64())
            .map(|n| format!("{n:.2}ms"))
            .unwrap_or_else(|| "not measured".into())
    };
    let report = format!(
        "# dot_1113 performance — {date}\n\nGate: {}. Existing stack only; no service started.\n\n| Metric | Before | After | Budget |\n| --- | --- | --- | --- |\n| Gateway total p95 (conservative overhead bound) | {} | {gateway_p95:.2}ms | <15ms |\n| Retrieval p95 (rerank disabled in fixture) | {} | {retrieval_p95:.2}ms | <300ms |\n| Space-list p95 ({} distinct rows) | {} | {spaces_p95:.2}ms | <150ms |\n\nCWV: five cold-context samples per route in the companion JSON. INP uses observed Event Timing interactions with a 16ms reporting bound. Frontend API fixtures isolate render costs; backend probes use the real services and prepared data.\n\nThe strict gate uses total Gateway p95 as a conservative bound on added latency; use a zero-delay mock. The separately recorded p95 delta is the difference of two oha samples under matched load (1000 requests, concurrency 8); it is not the percentile of paired per-request differences. Baseline cells stay unmeasured unless PERF_BASELINE_JSON is supplied.\n",
        if passed { "PASS" } else { "FAIL" },
        before("gateway_p95_ms"),
        before("retrieval_p95_ms"),
        ids.len(),
        before("spaces_p95_ms")
    );
    fs::write(reports.join(format!("perf-dot_1113-{date}.md")), report)?;
    if !passed {
        return Err("measured performance exceeds required budgets".into());
    }
    println!("performance gate passed; reports/perf-dot_1113-{date}.md");
    Ok(())
}
