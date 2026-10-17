//! Coding fixture patch materialization and isolated native Cargo host-test execution.
use crate::{
    Result,
    canonical::{digest, sha256},
    seals::{finite, strict_keys},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Stdio,
};
fn text<'a>(v: &'a Value, label: &str) -> Result<&'a str> {
    v.as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{label} must be nonempty text").into())
}
fn strings(v: &Value) -> Result<Vec<String>> {
    let mut out = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("nonempty string list required")?
        .iter()
        .map(|v| text(v, "list item").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    out.sort();
    if out.windows(2).any(|w| w[0] == w[1]) {
        return Err("duplicate list item".into());
    }
    Ok(out)
}
pub fn validate_patch(answer: &Value, contract: &Value) -> Result<BTreeMap<String, String>> {
    strict_keys(
        answer,
        &[
            "status",
            "changed_files",
            "investigated_areas",
            "diagnoses",
            "rejected_hint_ids",
            "replacement_files",
            "validation_commands",
            "remaining_verification_boundary",
        ],
        &[],
        "candidate patch",
    )?;
    if answer["status"] != "proposed_patch" {
        return Err("candidate status must be proposed_patch".into());
    }
    let allowed = strings(&contract["allowed_changes"])?;
    if strings(&answer["changed_files"])? != allowed {
        return Err("changed files differ from allowed patch scope".into());
    }
    let areas = strings(&contract["required_investigation_areas"])?;
    if strings(&answer["investigated_areas"])? != areas {
        return Err("investigation areas incomplete".into());
    }
    let diagnoses = answer["diagnoses"]
        .as_array()
        .filter(|a| a.len() == areas.len())
        .ok_or("one diagnosis per area required")?;
    let evidence = contract["required_diagnosis_evidence"]
        .as_object()
        .ok_or("diagnosis evidence contract missing")?;
    let mut evidence_keys = evidence.keys().cloned().collect::<Vec<_>>();
    evidence_keys.sort();
    if evidence_keys != areas {
        return Err("required diagnosis evidence malformed".into());
    }
    let mut diagnosed = vec![];
    for d in diagnoses {
        strict_keys(d, &["area", "root_cause", "evidence"], &[], "diagnosis")?;
        let area = text(&d["area"], "diagnosis area")?;
        diagnosed.push(area.to_owned());
        text(&d["root_cause"], "root cause")?;
        let required = text(
            &contract["required_diagnosis_evidence"][area],
            "required specialist",
        )?;
        if !strings(&d["evidence"])?.iter().any(|s| s == required) {
            return Err("diagnosis does not cite specialist receipt".into());
        }
    }
    diagnosed.sort();
    if diagnosed != areas {
        return Err("diagnoses incomplete".into());
    }
    if strings(&answer["rejected_hint_ids"])? != strings(&contract["required_rejected_hint_ids"])? {
        return Err("both unsupported hints must be rejected".into());
    }
    if answer["validation_commands"] != contract["test_commands"] {
        return Err("validation commands differ from host contract".into());
    }
    let boundary = text(
        &answer["remaining_verification_boundary"],
        "remaining boundary",
    )?
    .to_lowercase();
    if !boundary.contains("pending") || !boundary.contains("host") {
        return Err("pending host-test boundary required".into());
    }
    let replacements = answer["replacement_files"]
        .as_object()
        .ok_or("replacement files must be object")?;
    let mut keys = replacements.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    if keys != allowed {
        return Err("replacement scope mismatch".into());
    }
    let mut out = BTreeMap::new();
    let mut total = 0;
    for (path, source) in replacements {
        let source = text(source, "replacement source")?;
        total += source.len();
        if source.len() > 32000
            || source.as_bytes().contains(&0)
            || source.lines().any(|s| s.len() > 2000)
        {
            return Err("replacement source exceeds byte/line bounds or contains NUL".into());
        }
        out.insert(path.clone(), source.to_owned());
    }
    if total > 32000 {
        return Err("combined source exceeds byte limit".into());
    }
    Ok(out)
}
fn snapshot(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
        for item in std::fs::read_dir(path)? {
            let p = item?.path();
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                return Err("fixture symlink not allowed".into());
            }
            if m.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                visit(root, &p, out)?
            } else if m.is_file() {
                out.insert(
                    p.strip_prefix(root)?.to_string_lossy().replace('\\', "/"),
                    std::fs::read(p)?,
                );
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out)?;
    Ok(out)
}
/// Bind vendored dependency bytes to the independently source-hashed inventory.
pub fn verify_vendor(case: &Path) -> Result<()> {
    let inventory: Value =
        crate::strict_json::parse(&std::fs::read(case.join("vendor-inventory.json"))?)?;
    if inventory["schema_version"] != "coding-vendor-inventory/v1" {
        return Err("vendor inventory schema invalid".into());
    }
    let root = case.join("repository/vendor");
    let actual = snapshot(&root)?;
    let mut expected = BTreeSet::new();
    for package in inventory["packages"]
        .as_array()
        .filter(|p| !p.is_empty())
        .ok_or("vendor packages missing")?
    {
        let name = text(&package["directory"], "vendor directory")?;
        if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
            return Err("invalid vendor directory".into());
        }
        let manifest_path = format!("{name}/.cargo-checksum.json");
        if !expected.insert(manifest_path.clone()) {
            return Err("duplicate vendor package".into());
        }
        let raw = actual
            .get(&manifest_path)
            .ok_or("vendor checksum manifest missing")?;
        if package["checksum_manifest_sha256"] != sha256(raw) {
            return Err("vendor checksum manifest changed".into());
        }
        let manifest = crate::strict_json::parse(raw)?;
        if manifest["package"] != package["crate_package_checksum"] {
            return Err("vendor crate identity changed".into());
        }
        let files = manifest["files"]
            .as_object()
            .ok_or("vendor files missing")?;
        if package["file_count"].as_u64() != Some(files.len() as u64) {
            return Err("vendor file count changed".into());
        }
        for (path, hash) in files {
            let key = format!("{name}/{path}");
            if !expected.insert(key.clone()) {
                return Err("duplicate vendor file".into());
            }
            let bytes = actual.get(&key).ok_or("vendor file missing")?;
            if hash != &json!(sha256(bytes)) {
                return Err("vendor file checksum mismatch".into());
            }
            if [".py", ".pyc", ".pyo"].iter().any(|x| path.ends_with(x)) {
                return Err("Python vendor runtime forbidden".into());
            }
        }
    }
    if actual.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err("unexpected vendor file".into());
    }
    Ok(())
}
/// SequenceMatcher-style longest contiguous blocks, including the default popular-line heuristic.
pub fn changed_lines(before: &str, after: &str) -> usize {
    let a = before.lines().collect::<Vec<_>>();
    let b = after.lines().collect::<Vec<_>>();
    let mut positions = BTreeMap::<&str, Vec<usize>>::new();
    for (i, line) in b.iter().enumerate() {
        positions.entry(line).or_default().push(i)
    }
    if b.len() >= 200 {
        positions.retain(|_, p| p.len() <= b.len() / 100 + 1);
    }
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut matched = 0;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (mut best_a, mut best_b, mut size) = (alo, blo, 0);
        let mut previous = BTreeMap::new();
        for i in alo..ahi {
            let mut current = BTreeMap::new();
            for j in positions
                .get(a[i])
                .into_iter()
                .flatten()
                .copied()
                .filter(|j| *j >= blo && *j < bhi)
            {
                let count = previous.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
                current.insert(j, count);
                if count > size {
                    best_a = i + 1 - count;
                    best_b = j + 1 - count;
                    size = count
                }
            }
            previous = current;
        }
        while best_a > alo && best_b > blo && a[best_a - 1] == b[best_b - 1] {
            best_a -= 1;
            best_b -= 1;
            size += 1
        }
        while best_a + size < ahi && best_b + size < bhi && a[best_a + size] == b[best_b + size] {
            size += 1
        }
        if size > 0 {
            matched += size;
            if alo < best_a && blo < best_b {
                queue.push((alo, best_a, blo, best_b));
            }
            if best_a + size < ahi && best_b + size < bhi {
                queue.push((best_a + size, ahi, best_b + size, bhi));
            }
        }
    }
    a.len() + b.len() - 2 * matched
}
pub fn materialize(
    template: &Path,
    root: &Path,
    replacements: &BTreeMap<String, String>,
    contract: &Value,
) -> Result<Value> {
    let baseline = snapshot(template)?;
    std::fs::create_dir(root)?;
    for (relative, bytes) in &baseline {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
        std::fs::write(path, bytes)?
    }
    let canonical = root.canonicalize()?;
    for (relative, source) in replacements {
        let path = root.join(relative).canonicalize()?;
        if !path.starts_with(&canonical) || !path.is_file() {
            return Err("unsafe replacement path".into());
        }
        std::fs::write(path, source)?
    }
    let candidate = snapshot(root)?;
    let changed = baseline
        .keys()
        .chain(candidate.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| baseline.get(k) != candidate.get(k))
        .collect::<Vec<_>>();
    if changed != strings(&contract["allowed_changes"])? {
        return Err("materialized patch changed unexpected file".into());
    }
    let mut lines = 0;
    for path in &changed {
        lines += changed_lines(
            std::str::from_utf8(&baseline[path])?,
            std::str::from_utf8(&candidate[path])?,
        );
    }
    if lines
        > contract["max_changed_lines"]
            .as_u64()
            .ok_or("invalid line budget")? as usize
    {
        return Err(format!("patch changes {lines} lines; exceeds budget").into());
    }
    Ok(
        json!({"changed_files":changed,"changed_lines":lines,"replacement_sha256":changed.iter().map(|p|(p,sha256(&candidate[p]))).collect::<BTreeMap<_,_>>()}),
    )
}
pub(crate) fn docker() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("docker");
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}
pub(crate) async fn inspect_image(contract: &Value) -> Result<String> {
    let image = text(&contract["sandbox_image_reference"], "sandbox image")?;
    if !regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._/:@-]{0,255}$")?.is_match(image) {
        return Err("sandbox image malformed".into());
    }
    let (_, hash) = image
        .rsplit_once("@sha256:")
        .ok_or("sandbox image must be pinned")?;
    crate::seals::hash(&json!(hash), "image digest")?;
    let inspected = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        docker()
            .args(["image", "inspect", "--format", "{{.Id}}", image])
            .output(),
    )
    .await??;
    let image_id = String::from_utf8(inspected.stdout)?.trim().to_owned();
    if !inspected.status.success()
        || image_id
            != text(
                &contract["sandbox_image_config_digest"],
                "sandbox config digest",
            )?
    {
        return Err(
            "sandbox image missing or differs from pinned digest; preload exact image".into(),
        );
    }
    Ok(image_id)
}
pub async fn run_tests(
    workspace: &Path,
    contract: &Value,
    timeout_seconds: f64,
) -> Result<(Value, Vec<Value>)> {
    let image = text(&contract["sandbox_image_reference"], "sandbox image")?;
    let image_id = inspect_image(contract).await?;
    if !timeout_seconds.is_finite() || timeout_seconds <= 0. {
        return Err("invalid sandbox timeout".into());
    }
    let mut receipts = vec![];
    for (index, declared) in contract["test_commands"]
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("three host tests required")?
        .iter()
        .enumerate()
    {
        let argv = declared
            .as_array()
            .ok_or("test argv missing")?
            .iter()
            .map(|v| text(v, "test argv"))
            .collect::<Result<Vec<_>>>()?;
        let name = format!(
            "general-agent-code-eval-{}-{index}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        let mount = format!("{}:/work:ro", workspace.canonicalize()?.display());
        let mut command = docker();
        command
            .args([
                "run",
                "--rm",
                "--platform",
                text(&contract["sandbox_platform"], "sandbox platform")?,
                "--pull",
                "never",
                "--name",
                &name,
                "--network",
                "none",
                "--read-only",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--pids-limit",
                "64",
                "--memory",
                "128m",
                "--cpus",
                "0.5",
                "--user",
                "65534:65534",
                "--tmpfs",
                "/tmp:rw,noexec,nosuid,size=16m",
                "--tmpfs",
                "/build:rw,exec,nosuid,size=64m",
                "--log-driver",
                "none",
                "-e",
                "CARGO_TARGET_DIR=/build",
                "-e",
                "CARGO_NET_OFFLINE=true",
                "-v",
                &mount,
                "-w",
                "/work",
                image,
            ])
            .args(&argv)
            .stdout(Stdio::null());
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs_f64(timeout_seconds),
            command.status(),
        )
        .await;
        let (timed_out, exit) = match result {
            Ok(result) => (false, result?.code()),
            Err(_) => {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    docker()
                        .args(["rm", "-f", &name])
                        .stdout(Stdio::null())
                        .status(),
                )
                .await;
                (true, None)
            }
        };
        receipts.push(json!({"command":declared,"exit_code":exit,"timed_out":timed_out,"duration_ms":(start.elapsed().as_secs_f64()*1e6).round_ties_even()/1000.,"passed":exit==Some(0)&&!timed_out}));
    }
    Ok((json!({"reference":image,"image_id":image_id}), receipts))
}
pub fn validate_timeline(events: &[Value], contract: &Value) -> Result<Value> {
    let (mut starts, mut finishes) = (BTreeMap::new(), BTreeMap::new());
    let mut spawn_id = String::new();
    let (mut result, mut final_index, mut terminal) = (None, None, None);
    let mut accumulated = String::new();
    for (position, e) in events.iter().enumerate() {
        let kind = e["event_type"].as_str().unwrap_or("");
        let d = &e["data"];
        let index = d["dispatch_index"].as_i64();
        let call_id = d["tool_call_id"]
            .as_str()
            .or_else(|| d["call_id"].as_str())
            .unwrap_or("");
        if kind == "subagent_started" {
            if let Some(i) = index {
                if starts.insert(i, (position, d)).is_some() {
                    return Err("duplicate child start".into());
                }
            }
        } else if kind == "subagent_finished" {
            if let Some(i) = index {
                if finishes.insert(i, (position, d)).is_some() {
                    return Err("duplicate child finish".into());
                }
            }
        } else if ["tool_call_start", "tool_call_started"].contains(&kind) {
            if d["name"].as_str().or_else(|| d["tool_name"].as_str()) == Some("spawn_subagent") {
                if !spawn_id.is_empty() {
                    return Err("multiple spawn calls".into());
                }
                spawn_id = call_id.into()
            }
        } else if ["tool_call_result", "tool_call_completed", "tool_result"].contains(&kind)
            && !spawn_id.is_empty()
            && call_id == spawn_id
        {
            if result.replace((position, d)).is_some() {
                return Err("duplicate spawn result".into());
            }
        }
        if ["text_delta", "text_message_content"].contains(&kind) {
            accumulated.push_str(
                d.as_str()
                    .or_else(|| {
                        ["content", "message", "delta"]
                            .into_iter()
                            .find_map(|f| d[f].as_str())
                    })
                    .unwrap_or(""),
            );
            if final_index.is_none() && accumulated.contains("<FINAL_JSON>") {
                final_index = Some(position)
            }
        }
        if crate::scenario_events::TERMINALS.contains(&kind) && terminal.replace(position).is_some()
        {
            return Err("multiple parent terminals".into());
        }
    }
    if starts.keys().copied().collect::<Vec<_>>() != [0, 1]
        || finishes.keys().copied().collect::<Vec<_>>() != [0, 1]
    {
        return Err("two complete child lifecycles required".into());
    }
    let (result_index, result) = result
        .filter(|_| !spawn_id.is_empty())
        .ok_or("paired spawn result missing")?;
    if result["success"] != true
        && !matches!(result["status"].as_str(), Some("completed" | "success"))
    {
        return Err("spawn result not successful".into());
    }
    if !["none", "read_only"].contains(
        &result["side_effect_state"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
    ) {
        return Err("spawn result lacks safe typed side-effect state".into());
    }
    let final_index = final_index.ok_or("final JSON missing")?;
    let terminal = terminal.ok_or("parent terminal missing")?;
    let mut intervals = vec![];
    let mut hashes = vec![];
    for i in [0, 1] {
        let (sp, start) = starts[&i];
        let (fp, finish) = finishes[&i];
        if sp >= fp {
            return Err("child terminal before start".into());
        }
        let a = finite(&finish["started_monotonic_ms"], 0., "child start")?;
        let b = finite(&finish["finished_monotonic_ms"], 0., "child finish")?;
        if a != finite(&start["started_monotonic_ms"], 0., "start receipt")? {
            return Err("child start timestamp mismatch".into());
        }
        if b - a
            < finite(
                &contract["minimum_child_duration_ms"],
                0.,
                "minimum duration",
            )?
        {
            return Err("child duration below minimum".into());
        }
        intervals.push((a, b));
        hashes.push(digest(finish));
    }
    let overlap = intervals[0].1.min(intervals[1].1) - intervals[0].0.max(intervals[1].0);
    if overlap
        < finite(
            &contract["minimum_parallel_overlap_ms"],
            0.,
            "minimum overlap",
        )?
    {
        return Err("parallel overlap below minimum".into());
    }
    let last = finishes.values().map(|(p, _)| *p).max().unwrap();
    if !(last < result_index && result_index < final_index && final_index < terminal) {
        return Err("patch synthesis order invalid".into());
    }
    hashes.sort();
    Ok(
        json!({"overlap_ms":(overlap*1000.).round_ties_even()/1000.,"spawn_call_id_sha256":sha256(spawn_id.as_bytes()),"child_terminal_sha256s":hashes,"last_child_terminal_event_index":last,"spawn_result_event_index":result_index,"final_json_event_index":final_index,"parent_terminal_event_index":terminal}),
    )
}
pub async fn validate_observations(
    root: &Path,
    observation_path: &Path,
    live: bool,
) -> Result<Value> {
    let case = root.join("tests/fixtures/eval/general_agent/coding_parallel_v2");
    let scenarios = crate::scenario_contract::load(&case.join("scenario.json"))?;
    let sources = crate::scenario_contract::verify_source_artifacts(&scenarios, &case)?;
    verify_vendor(&case)?;
    let plugins = crate::scenario_contract::verify_plugin_definitions(&scenarios, root)?;
    let observations = crate::strict_json::parse(&std::fs::read(observation_path)?)?;
    if !live {
        return Ok(
            json!({"schema_version":crate::coding_receipt::RECEIPT_SCHEMA,"suite_id":scenarios["suite_id"],"scenario_id":scenarios["scenarios"][0]["scenario_id"],"validation_passed":false,"acceptance_eligible":false,"passed":false,"trials":[],"errors":["offline observations are not accepted; use the live collector path with fresh HMAC, nonce, raw SSE, and runtime binding"]}),
        );
    }
    let key = crate::scenario_runner::attestation_key("GENERAL_AGENT_COLLECTOR_HMAC_KEY")?;
    let (runtime, nonce) = crate::scenario_runner::runtime_identity()?;
    let by_key = crate::scenario_evidence::verify_observations(
        &scenarios,
        &observations,
        &sources,
        &plugins,
        observation_path,
        &key,
        &runtime,
        &nonce,
    )?;
    let contract =
        crate::strict_json::parse(&std::fs::read(case.join("validator_contract.json"))?)?;
    let policy = crate::coding_receipt::validator_policy(
        &case.join("validator_contract.json"),
        &root.join("rust/platform-native/crates/platform-eval/src/coding_sandbox.rs"),
    )?;
    let raw_path = observation_path
        .canonicalize()?
        .parent()
        .ok_or("observation parent missing")?
        .join(
            observations["raw_sse_artifact"]["file_name"]
                .as_str()
                .ok_or("raw SSE file missing")?,
        );
    let (raw, _) = crate::seals::read_private(&raw_path, 50_000_000)?;
    let mut timelines = BTreeMap::new();
    for t in raw["trials"].as_array().ok_or("raw trials missing")? {
        let events = t["raw_sse_payloads"]
            .as_array()
            .ok_or("raw payloads missing")?
            .iter()
            .map(|v| crate::strict_json::parse(text(v, "raw payload")?.as_bytes()))
            .collect::<Result<Vec<_>>>()?;
        let id = (
            text(&t["scenario_id"], "scenario ID")?.to_owned(),
            t["trial"].as_i64().ok_or("trial missing")?,
        );
        timelines.insert(id, validate_timeline(&events, &contract)?);
    }
    let scenario = &scenarios["scenarios"][0];
    let id = text(&scenario["scenario_id"], "scenario ID")?;
    let mut trials = vec![];
    for n in 1..=scenario["repetitions"]
        .as_i64()
        .ok_or("repetitions missing")?
    {
        let o = &by_key[&(id.into(), n)];
        let checks = crate::scenario::execution_checks(scenario, o);
        let (mut patch, mut image, mut tests) = (json!({}), json!({}), vec![]);
        let result: Result<()> = async {
            if checks.is_empty() || checks.iter().any(|c| c["passed"] != true) {
                return Err("coding execution checks not passed".into());
            }
            let answer = crate::scenario::answer_json(
                text(&o["candidate_output"], "candidate output")?,
                "final_json_tag",
            )?;
            let replacements = validate_patch(&answer, &contract)?;
            let temp = tempfile::tempdir()?;
            let workspace = temp.path().join("workspace");
            patch = materialize(
                &case.join("repository"),
                &workspace,
                &replacements,
                &contract,
            )?;
            (image, tests) = run_tests(&workspace, &contract, 20.).await?;
            if tests.iter().any(|t| t["passed"] != true) {
                return Err("one or more isolated host tests failed".into());
            }
            Ok(())
        }
        .await;
        let errors = result
            .err()
            .map(|e| vec![e.to_string()])
            .unwrap_or_default();
        trials.push(json!({"trial":n,"observation_sha256":o["observation_sha256"],"accepted":errors.is_empty(),"execution_checks":checks,"timeline":timelines.get(&(id.into(),n)).ok_or("timeline missing")?,"patch":patch,"sandbox_image":image,"host_test_receipts":tests,"errors":errors}));
    }
    let passed = trials.len() == 3 && trials.iter().all(|t| t["accepted"] == true);
    Ok(
        json!({"schema_version":crate::coding_receipt::RECEIPT_SCHEMA,"suite_id":scenarios["suite_id"],"scenario_id":scenario["scenario_id"],"scenario_contract_sha256":observations["scenario_contract_sha256"],"observations_sha256":observations["observations_sha256"],"validator_contract_sha256":policy["validator_contract_sha256"],"verifier_executable_sha256":policy["verifier_executable_sha256"],"source_artifacts":sources,"plugin_definitions":plugins,"runtime_binding":observations["runtime_binding"],"raw_sse_artifact":observations["raw_sse_artifact"],"collector_attestation_key_id":observations["collector_attestation"]["key_id"],"trials":trials,"three_run_policy_met":passed,"validation_passed":passed,"acceptance_eligible":true,"passed":passed,"evidence_boundary":"Real Gateway SSE lifecycle plus isolated local Docker test execution. This is not a production deployment receipt."}),
    )
}
fn outside_fixture(output: &Path, case: &Path) -> Result<()> {
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    let mut normalized = std::path::PathBuf::new();
    for c in absolute.components() {
        match c {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => normalized.push(c.as_os_str()),
        }
    }
    if normalized.starts_with(case.canonicalize()?) {
        return Err("receipt output must be outside immutable fixture sources".into());
    }
    Ok(())
}
pub async fn cli(raw: impl Iterator<Item = String>) -> Result<(Value, i32)> {
    let args = crate::cli::Args::parse(raw, &["require-hmac"])?;
    args.reject_unknown(&["observations-out", "receipt-out", "image", "require-hmac"])?;
    let root = std::env::current_dir()?;
    let case = root.join("tests/fixtures/eval/general_agent/coding_parallel_v2");
    let observations = Path::new(args.required("observations-out")?);
    let output = Path::new(args.required("receipt-out")?);
    outside_fixture(observations, &case)?;
    outside_fixture(output, &case)?;
    let key =
        crate::scenario_runner::attestation_key(crate::coding_receipt::HMAC_ENVIRONMENT_NAME)?;
    for name in [
        "GENERAL_AGENT_COLLECTOR_HMAC_KEY",
        "GENERAL_AGENT_GOLDEN_HMAC_KEY",
    ] {
        if std::env::var(name).is_ok_and(|other| other == key) {
            return Err("coding host key must be independent".into());
        }
    }
    let contract =
        crate::strict_json::parse(&std::fs::read(case.join("validator_contract.json"))?)?;
    if args
        .options
        .get("image")
        .is_some_and(|i| contract["sandbox_image_reference"] != *i)
    {
        return Err("image override differs from release-pinned policy".into());
    }
    // Check provisioning before incurring live provider work.
    crate::coding_receipt::validator_policy(
        &case.join("validator_contract.json"),
        &root.join("rust/platform-native/crates/platform-eval/src/coding_sandbox.rs"),
    )?;
    let scenarios = crate::scenario_contract::load(&case.join("scenario.json"))?;
    crate::scenario_contract::verify_source_artifacts(&scenarios, &case)?;
    verify_vendor(&case)?;
    inspect_image(&contract).await?;
    crate::scenario_runner::collect(&root, &case.join("scenario.json"), observations).await?;
    let receipt = validate_observations(&root, observations, true).await?;
    let document = crate::seals::seal(&receipt, Some(&key))?;
    crate::seals::write_private(output, &document)?;
    let code = if receipt["passed"] == true { 0 } else { 1 };
    Ok((
        json!({"passed":receipt["passed"],"receipt":output,"observations":observations}),
        code,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_line_budget_counts_replacements() {
        assert_eq!(changed_lines("a\nb\nc\n", "a\nx\nc\n"), 2);
        assert_eq!(changed_lines("same\n", "same\n"), 0);
    }
    #[test]
    fn unbound_image_policy_cannot_produce_live_receipt() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let path = root
            .join("tests/fixtures/eval/general_agent/coding_parallel_v2/validator_contract.json");
        let mut contract: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let verifier = root.join("rust/platform-native/crates/platform-eval/src/coding_sandbox.rs");
        let policy = crate::coding_receipt::validator_policy(&path, &verifier).unwrap();
        assert_ne!(
            policy["sandbox_image_reference"]
                .as_str()
                .unwrap()
                .split('@')
                .nth(1)
                .unwrap(),
            policy["sandbox_image_config_digest"].as_str().unwrap()
        );
        contract["sandbox_image_reference"] = Value::Null;
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), serde_json::to_vec(&contract).unwrap()).unwrap();
        assert!(crate::coding_receipt::validator_policy(temp.path(), &verifier).is_err());
    }
    #[test]
    fn vendored_fixture_checksums_are_complete() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        verify_vendor(&root.join("tests/fixtures/eval/general_agent/coding_parallel_v2")).unwrap();
    }
    #[test]
    fn vendor_integrity_rejects_changed_or_unlisted_files() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("repository/vendor/example");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("lib.rs"), b"original").unwrap();
        let checksum = json!({"package":"a".repeat(64),"files":{"lib.rs":sha256(b"original")}});
        let raw = serde_json::to_vec(&checksum).unwrap();
        std::fs::write(package.join(".cargo-checksum.json"), &raw).unwrap();
        let inventory = json!({"schema_version":"coding-vendor-inventory/v1","packages":[{"directory":"example","crate_package_checksum":"a".repeat(64),"file_count":1,"checksum_manifest_sha256":sha256(&raw)}]});
        std::fs::write(
            temp.path().join("vendor-inventory.json"),
            serde_json::to_vec(&inventory).unwrap(),
        )
        .unwrap();
        verify_vendor(temp.path()).unwrap();
        std::fs::write(package.join("lib.rs"), b"changed").unwrap();
        assert!(verify_vendor(temp.path()).is_err());
        std::fs::write(package.join("lib.rs"), b"original").unwrap();
        std::fs::write(package.join("injected.rs"), b"extra").unwrap();
        assert!(verify_vendor(temp.path()).is_err());
    }
    #[test]
    fn empty_timeline_never_passes() {
        assert!(
            validate_timeline(
                &[],
                &json!({"minimum_parallel_overlap_ms":25,"minimum_child_duration_ms":25})
            )
            .is_err()
        );
    }
}
