//! Immutable Agent Studio regression manifest runner and source-evidence binding.
use crate::{Result, canonical::sha256, cli::Args};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::io::{AsyncBufReadExt, BufReader};
const REPORT_PREFIXES: [&str; 3] = ["reports/", "web/playwright-report/", "web/test-results/"];
pub fn validate_manifest(manifest: &Value, root: &Path) -> Result<()> {
    if manifest["schema_version"] != "agent-studio-regression/v1" {
        return Err("unsupported Agent Studio regression manifest schema".into());
    }
    let gates = manifest["gates"]
        .as_array()
        .filter(|g| !g.is_empty())
        .ok_or("manifest gates must be a non-empty list")?;
    let mut seen = BTreeSet::new();
    let root = root.canonicalize()?;
    for gate in gates {
        if !gate.is_object() {
            return Err("every manifest gate must be an object".into());
        }
        let key = format!(
            "{}:{}",
            gate["phase"].as_str().unwrap_or("None"),
            gate["id"].as_str().unwrap_or("None")
        );
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate manifest gate: {key}").into());
        }
        if gate["required"] != true {
            return Err(format!("manifest gate is not required: {key}").into());
        }
        if ["phase", "id", "cwd", "command"]
            .iter()
            .any(|f| !gate[f].as_str().is_some_and(|s| !s.is_empty()))
        {
            return Err(format!("manifest gate is incomplete: {key}").into());
        }
        let cwd = root.join(gate["cwd"].as_str().unwrap()).canonicalize()?;
        if !cwd.starts_with(&root) {
            return Err(format!("manifest gate leaves repository: {key}").into());
        }
    }
    Ok(())
}
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err("git source evidence unavailable".into());
    }
    Ok(output.stdout)
}
pub fn workspace_hash(root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(git(root, &["rev-parse", "HEAD"])?);
    hash.update(git(
        root,
        &[
            "diff",
            "--binary",
            "HEAD",
            "--",
            ".",
            ":(exclude)reports/**",
            ":(exclude)web/playwright-report/**",
            ":(exclude)web/test-results/**",
        ],
    )?);
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut paths = untracked
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    paths.sort();
    for bytes in paths {
        let relative = std::str::from_utf8(bytes)?;
        if REPORT_PREFIXES.iter().any(|p| relative.starts_with(p)) {
            continue;
        }
        let path = root.join(relative);
        if !path.is_file() {
            continue;
        }
        hash.update(bytes);
        hash.update([0]);
        hash.update(std::fs::read(path)?);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn skipped_count(text: &str) -> usize {
    let r = regex::Regex::new(r"(?im)(?:^|[^\w.])([1-9]\d*)\s+(?:skipped|ignored)(?:\s|,|;|$)")
        .unwrap();
    r.captures_iter(text)
        .filter_map(|m| m[1].parse().ok())
        .max()
        .unwrap_or(0)
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn environment(root: &Path) -> BTreeMap<String, String> {
    let mut env = std::env::vars().collect::<BTreeMap<_, _>>();
    for (key, default) in [
        ("COMPOSE_PARALLEL_LIMIT", "1"),
        ("CI", "1"),
        ("GATEWAY_BASE_URL", "http://127.0.0.1:8080"),
    ] {
        env.entry(key.into()).or_insert_with(|| default.into());
    }
    for key in ["NO_PROXY", "no_proxy"] {
        let mut entries = env
            .get(key)
            .map(|s| {
                s.split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for host in ["localhost", "127.0.0.1", "::1"] {
            if !entries.iter().any(|s| s == host) {
                entries.push(host.into());
            }
        }
        env.insert(key.into(), entries.join(","));
    }
    if !env
        .get("ASSISTANT_E2E_USER1_EMAIL")
        .is_some_and(|s| !s.is_empty())
        || !env
            .get("ASSISTANT_E2E_PASSWORD")
            .is_some_and(|s| !s.is_empty())
    {
        if let Ok(bytes) = std::fs::read(root.join("web/.playwright/e2e-user.json")) {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let (Some(email), Some(password)) = (
                    v["email"].as_str().filter(|s| !s.is_empty()),
                    v["password"].as_str().filter(|s| !s.is_empty()),
                ) {
                    env.insert("ASSISTANT_E2E_USER1_EMAIL".into(), email.into());
                    env.insert("ASSISTANT_E2E_PASSWORD".into(), password.into());
                }
            }
        }
    }
    env
}
async fn run_gate(gate: &Value, root: &Path, log_dir: &Path) -> Result<Value> {
    let phase = gate["phase"].as_str().unwrap();
    let id = gate["id"].as_str().unwrap();
    let log_path = log_dir.join(format!("{}-{id}.log", phase.to_lowercase()));
    let mut log = std::fs::File::create(&log_path)?;
    let start = Instant::now();
    let started_at = now();
    println!("\n[Agent Studio] START {phase}:{id}");
    let mut child = tokio::process::Command::new("/bin/bash")
        .arg("-c")
        .arg(format!("exec 2>&1; {}", gate["command"].as_str().unwrap()))
        .current_dir(root.join(gate["cwd"].as_str().unwrap()))
        .env_clear()
        .envs(environment(root))
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("child stdout unavailable")?);
    let mut line = String::new();
    while stdout.read_line(&mut line).await? > 0 {
        print!("{line}");
        std::io::stdout().flush()?;
        log.write_all(line.as_bytes())?;
        line.clear();
    }
    let status = child.wait().await?;
    log.flush()?;
    let elapsed = (start.elapsed().as_secs_f64() * 1000.).round() / 1000.;
    let bytes = std::fs::read(&log_path)?;
    let skipped = skipped_count(&String::from_utf8_lossy(&bytes));
    let code = status.code().unwrap_or(-1);
    let native_tests = gate["command"].as_str().unwrap().contains("cargo test");
    let executed = regex::Regex::new(r"(?m)^test result: ok\. ([1-9]\d*) passed;")
        .unwrap()
        .is_match(&String::from_utf8_lossy(&bytes));
    let passed = code == 0 && skipped == 0 && (!native_tests || executed);
    let failure = if native_tests && !executed {
        Some("required native gate did not execute any passing tests".to_string())
    } else if skipped > 0 {
        Some(format!("required gate reported {skipped} skipped test(s)"))
    } else if code != 0 {
        Some(format!("gate command exited with status {code}"))
    } else {
        None
    };
    println!(
        "[Agent Studio] {} {phase}:{id} ({elapsed}s)",
        if passed { "PASSED" } else { "FAILED" }
    );
    Ok(
        json!({"phase":phase,"id":id,"status":if passed{"passed"}else{"failed"},"exit_code":code,"skipped_count":skipped,"failure_reason":failure,"started_at":started_at,"completed_at":now(),"elapsed_seconds":elapsed,"log":log_path.strip_prefix(root)?,"log_sha256":sha256(&bytes)}),
    )
}
fn absolute(root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    }
}
pub async fn run(args: Args) -> Result<i32> {
    args.reject_unknown(&["manifest", "output", "validate-only", "list", "fail-fast"])?;
    if !args.positional.is_empty() {
        return Err("unexpected positional arguments".into());
    }
    let root = std::env::current_dir()?.canonicalize()?;
    let manifest_path = absolute(
        &root,
        args.get(
            "manifest",
            "tests/fixtures/agent-studio/regression_manifest.json",
        ),
    )
    .canonicalize()?;
    let bytes = std::fs::read(&manifest_path)?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    validate_manifest(&manifest, &root)?;
    let hash = sha256(&bytes);
    let gates = manifest["gates"].as_array().unwrap();
    if args.options.contains_key("list") {
        for gate in gates {
            println!(
                "{}:{}\t{}",
                gate["phase"].as_str().unwrap(),
                gate["id"].as_str().unwrap(),
                gate["command"].as_str().unwrap()
            );
        }
        return Ok(0);
    }
    println!(
        "Agent Studio manifest v1: {} required gates, sha256:{hash}",
        gates.len()
    );
    if args.options.contains_key("validate-only") {
        return Ok(0);
    }
    let before = workspace_hash(&root)?;
    let output = absolute(
        &root,
        args.get(
            "output",
            "reports/agent-studio/agent-studio-regression-v1-result.json",
        ),
    );
    let parent = output.parent().ok_or("output parent required")?;
    std::fs::create_dir_all(parent)?;
    let logs = parent.join("regression-v1");
    std::fs::create_dir_all(&logs)?;
    let started_at = now();
    let mut results = vec![];
    for gate in gates {
        let result = run_gate(gate, &root, &logs).await?;
        let passed = result["status"] == "passed";
        results.push(result);
        if !passed && args.options.contains_key("fail-fast") {
            break;
        }
    }
    let stable = before == workspace_hash(&root)?;
    let passed_count = results.iter().filter(|r| r["status"] == "passed").count();
    let passed = results.len() == gates.len() && stable && passed_count == results.len();
    let payload = json!({"schema_version":"agent-studio-regression-result/v1","status":if passed{"passed"}else{"failed"},"started_at":started_at,"completed_at":now(),"manifest":manifest_path.strip_prefix(&root)?,"manifest_sha256":hash,"source_sha256":before,"source_stable":stable,"required_gate_count":gates.len(),"executed_gate_count":results.len(),"passed_gate_count":passed_count,"failed_gate_count":results.len()-passed_count,"results":results});
    crate::write_report(&output, &payload)?;
    println!(
        "\nAgent Studio aggregate {}: {passed_count}/{} gates; source_stable={stable}; result={}",
        payload["status"].as_str().unwrap(),
        gates.len(),
        output.strip_prefix(&root)?.display()
    );
    Ok(if passed { 0 } else { 1 })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skips_are_never_green() {
        assert_eq!(skipped_count("4 passed, 2 skipped in 0.3s\n1 skipped"), 2);
        assert_eq!(
            skipped_count(
                "test result: ok. 4 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out"
            ),
            2
        );
        assert_eq!(skipped_count("0 skipped\n0.4 skipped\nword2 skipped"), 0);
    }
    #[test]
    fn duplicate_manifest_gates_rejected() {
        let gate = json!({"phase":"AS-00","id":"same","cwd":".","command":"true","required":true});
        let m = json!({"schema_version":"agent-studio-regression/v1","gates":[gate.clone(),gate]});
        assert!(validate_manifest(&m, &std::env::current_dir().unwrap()).is_err());
    }
}
