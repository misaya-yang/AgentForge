//! Monthly U-RT-07 release gate. Evidence only: no fetch, merge, build, deployment
//! or promotion, and no second program ledger. The primary links receipts from
//! this command into the existing native-enterprise-upgrade loop-state.json.
use crate::*;
use std::{
    collections::BTreeSet,
    io::Write,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[path = "upstream_sync_inventory.rs"]
mod inventory;
const WINDOW_SCHEMA: &str = "agentforge/monthly-upstream-window/v1";
const REPORT_SCHEMA: &str = "agentforge/monthly-upstream-report/v1";
const RUN_SCHEMA: &str = "agentforge/source-bound-gate-run/v1";
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing {key}").into())
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn load(path: &Path) -> Result<Value> {
    let m = std::fs::symlink_metadata(path)?;
    require(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= 16 * 1024 * 1024,
        "bounded regular JSON evidence required",
    )?;
    object(path)
}
fn stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn millis(s: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)?.timestamp_millis())
}
fn immutable(path: &Path, value: &Value) -> Result<()> {
    require(
        path.is_absolute(),
        "absolute external receipt destination required",
    )?;
    let parent = path.parent().ok_or("receipt parent missing")?;
    std::fs::create_dir_all(parent)?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    f.write_all(&bytes)?;
    f.sync_all()?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
fn external(args: &Args, path: &Path, inputs: &[&Path]) -> Result<()> {
    let p = resolve_path(path.to_str().ok_or("receipt path encoding")?)?;
    require(
        !p.starts_with(args.root().canonicalize()?),
        "receipt must be external to repository",
    )?;
    for i in inputs {
        require(
            !p.starts_with(i.canonicalize()?),
            "receipt would overwrite an input",
        )?;
    }
    Ok(())
}
fn identity(package: &Path) -> Result<Value> {
    let p = package.join("source-unit.json");
    let u = load(&p)?;
    require(
        u["schema"] == "ai-platform/runtime-source-unit/v1"
            && hex(text(&u, "target")?, 40)
            && hex(text(&u, "target_tree")?, 40),
        "source-unit identity invalid",
    )?;
    Ok(
        json!({"upstream_revision":u["target"],"upstream_tree":u["target_tree"],"source_unit_sha256":digest(&p)?,"cargo_lock_sha256":u["resolved_lock_sha256"]}),
    )
}
fn pinned_tag(checkout: &Path, tag: &str, target: &str) -> Result<Value> {
    let version = tag
        .strip_prefix("rust-v")
        .ok_or("stable rust-v tag required")?;
    let pieces = version.split('.').collect::<Vec<_>>();
    require(
        pieces.len() == 3
            && pieces
                .iter()
                .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
        "stable rust-v major.minor.patch tag required",
    )?;
    require(
        inventory::git_text(checkout, &["remote", "get-url", "origin"])?
            == "https://github.com/openai/codex.git",
        "canonical local upstream required",
    )?;
    require(
        inventory::git_text(
            checkout,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty(),
        "upstream checkout must be clean",
    )?;
    let reference = format!("refs/tags/{tag}");
    let resolved = inventory::git_text(
        checkout,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )?;
    require(
        resolved == target,
        "stable tag differs from fixed source commit",
    )?;
    let object = inventory::git_text(checkout, &["rev-parse", "--verify", &reference])?;
    let tree = inventory::git_text(
        checkout,
        &["rev-parse", "--verify", &format!("{target}^{{tree}}")],
    )?;
    Ok(json!({"name":tag,"object":object,"commit":resolved,"tree":tree}))
}
const LEDGER: &str = "deploy/runbooks/native-enterprise-upgrade/loop-state.json";
fn registered_windows(root: &Path) -> Result<Vec<Value>> {
    let program = load(&root.join(LEDGER))?;
    let package = program["work_packages"]
        .as_array()
        .ok_or("sole program ledger work packages absent")?
        .iter()
        .find(|v| v["id"] == "U-RT-07")
        .ok_or("U-RT-07 missing from sole ledger")?;
    match package.get("sync_windows") {
        None => Ok(Vec::new()),
        Some(v) => Ok(v
            .as_array()
            .ok_or("ledger sync_windows must be evidence references")?
            .clone()),
    }
}
fn registered(args: &Args, w: &Value, window_sha: &str) -> Result<()> {
    let windows = registered_windows(&args.root())?;
    let entries = windows
        .iter()
        .filter(|v| v["month"] == w["month"])
        .collect::<Vec<_>>();
    require(
        entries.len() == 1,
        "register exactly one monthly window in the existing U-RT-07 ledger before gates",
    )?;
    let row = entries[0];
    require(
        row["window_sha256"] == window_sha
            && row["started_at"] == w["started_at"]
            && row["target_commit"] == w["tag"]["commit"],
        "monthly ledger pin differs; window clock cannot reset",
    )
}
fn anchor(args: &Args) -> Result<Value> {
    let started_at = stamp();
    let package = Path::new(args.get("--package")?);
    let previous = Path::new(args.get("--baseline-package")?);
    let checkout = Path::new(args.get("--upstream")?);
    let lock_path = Path::new(args.get("--baseline-lock")?);
    let schema = Path::new(args.get("--baseline-schema")?);
    let month = args.get("--month")?;
    require(
        month == chrono::Utc::now().format("%Y-%m").to_string(),
        "only the actual current UTC month can open; historical windows cannot be manufactured",
    )?;
    require(
        !registered_windows(&args.root())?
            .iter()
            .any(|v| v["month"] == month),
        "this month is already registered; continue its original clock",
    )?;
    let candidate = identity(package)?;
    let baseline = identity(previous)?;
    let lock = load(lock_path)?;
    require(
        lock["schema_version"] == "ai-platform/agent-runtime-source-lock/v2"
            && lock["source"]["upstream_sha"] == baseline["upstream_revision"],
        "baseline source-lock differs from N-1 source unit",
    )?;
    let previous_receipt_path = Path::new(args.get("--baseline-source-receipt")?);
    let previous_receipt = load(previous_receipt_path)?;
    require(
        digest(previous_receipt_path)? == lock["build"]["source_receipt_sha256"]
            && previous_receipt["evidence_provenance"]["source_unit_sha256"]
                == baseline["source_unit_sha256"]
            && previous_receipt["source"]["upstream_sha"] == baseline["upstream_revision"]
            && lock["source"]["git_tree_sha"] == baseline["upstream_tree"]
            && lock["build"]["cargo_lock_sha256"] == baseline["cargo_lock_sha256"],
        "baseline source-unit lacks source-lock receipt provenance",
    )?;
    require(
        candidate["upstream_revision"] != baseline["upstream_revision"],
        "monthly upstream sync must advance the upstream commit",
    )?;
    let tag = pinned_tag(
        checkout,
        args.get("--tag")?,
        text(&candidate, "upstream_revision")?,
    )?;
    require(
        tag["tree"] == candidate["upstream_tree"],
        "target tree differs",
    )?;
    let schema_identity = crate::source::bundle_identity(schema, false)?;
    require(
        schema_identity["sha256"] == lock["build"]["app_server_schema_sha256"],
        "baseline compiled schema differs from lock",
    )?;
    let mut sboms = serde_json::Map::new();
    for (kind, option, key) in [
        ("runtime", "--baseline-sbom", "sbom_sha256"),
        (
            "worker",
            "--baseline-worker-sbom",
            "capability_worker_sbom_sha256",
        ),
    ] {
        let path = Path::new(args.get(option)?);
        let sha = digest(path)?;
        require(
            lock["build"][key] == sha,
            "baseline SBOM differs from source-lock",
        )?;
        sboms.insert(
            kind.into(),
            json!({"sha256":sha,"components":inventory::sbom_components(path)?}),
        );
    }
    let report = json!({"schema":WINDOW_SCHEMA,"month":month,"started_at":started_at,"tag":tag,"initial_candidate":candidate,"baseline":baseline,"baseline_lock_sha256":digest(lock_path)?,"baseline_source_receipt_sha256":digest(previous_receipt_path)?,"baseline_schema":schema_identity,"baseline_schema_files":inventory::schema_files(schema)?,"baseline_sboms":sboms,"initial_patch":inventory::metrics(package,previous,checkout)?,"cadence":"one real window per UTC month, existing program ledger remains authoritative","acceptance":false});
    let output = Path::new(args.get("--output")?);
    external(args, output, &[package, previous, checkout, schema])?;
    immutable(output, &report)?;
    Ok(
        json!({"result":"pass","scope":"window opened; no gate or acceptance claim","window":output,"window_sha256":digest(output)?,"observed":report}),
    )
}
fn window(path: &Path) -> Result<Value> {
    let w = load(path)?;
    require(w["schema"] == WINDOW_SCHEMA, "window schema invalid")?;
    let started = chrono::DateTime::parse_from_rfc3339(text(&w, "started_at")?)?;
    require(
        started.format("%Y-%m").to_string() == text(&w, "month")?
            && started.timestamp_millis() <= chrono::Utc::now().timestamp_millis(),
        "window is future-dated or assigned to another month",
    )?;
    Ok(w)
}
fn input_digest(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    require(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 64 * 1024 * 1024,
        "gate input must be a bounded regular file",
    )?;
    digest(path)
}
fn pinned_inputs(spec: &Value) -> Result<Vec<Value>> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for file in spec["inputs"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("gate input inventory missing")?
    {
        let path = Path::new(text(file, "path")?);
        let expected = text(file, "sha256")?;
        require(
            path.is_absolute() && hex(expected, 64) && seen.insert(path.to_owned()),
            "gate input invalid or duplicate",
        )?;
        require(input_digest(path)? == expected, "gate input changed")?;
        result.push(file.clone());
    }
    Ok(result)
}
/// Runs only an explicitly selected prebuilt gate executable, without shell,
/// inherited credentials, builds or provider fallback. The operator's normal
/// authorization still applies to the selected runner; receipts grant none.
fn record(args: &Args) -> Result<Value> {
    let window_path = Path::new(args.get("--window")?);
    let w = window(window_path)?;
    registered(args, &w, &digest(window_path)?)?;
    let package = Path::new(args.get("--package")?);
    let lock = Path::new(args.get("--source-lock")?);
    let kind = args.get("--kind")?;
    require(
        ["history", "replay", "eval"].contains(&kind),
        "unknown gate kind",
    )?;
    let spec = load(Path::new(args.get("--spec")?))?;
    require(
        spec["schema"] == "agentforge/upstream-gate-command/v1",
        "gate command schema invalid",
    )?;
    let executable = Path::new(text(&spec, "executable")?);
    let metadata = std::fs::symlink_metadata(executable)?;
    require(
        executable.is_absolute() && metadata.is_file() && !metadata.file_type().is_symlink(),
        "prebuilt regular gate executable required",
    )?;
    let allowed = match kind {
        "history" => "ai-platform-history-gate",
        "replay" => "agent-runtime-replay",
        "eval" => "agent-runtime-eval-smoke",
        _ => unreachable!(),
    };
    require(
        executable.file_name().and_then(|v| v.to_str()) == Some(allowed),
        "gate executable name not admitted; shell/build/provider commands forbidden",
    )?;
    let executable_sha = digest(executable)?;
    require(
        spec["executable_sha256"] == executable_sha,
        "gate executable digest differs",
    )?;
    let inputs = pinned_inputs(&spec)?;
    let before = identity(package)?;
    let lock_sha = digest(lock)?;
    require(
        before["upstream_revision"] == w["tag"]["commit"],
        "gate targets another monthly commit",
    )?;
    let argv = spec["argv"]
        .as_array()
        .ok_or("gate argv missing")?
        .iter()
        .map(|v| v.as_str().ok_or("gate argv invalid"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    require(
        argv.len() <= 64 && argv.iter().all(|v| v.len() <= 4096 && !v.contains('\0')),
        "gate argv exceeds bounds",
    )?;
    let receipt = Path::new(text(&spec, "receipt")?);
    require(
        receipt.is_absolute() && !receipt.exists(),
        "gate result must be a fresh absolute path",
    )?;
    external(args, receipt, &[package, window_path, lock, executable])?;
    let limit = spec["timeout_seconds"]
        .as_u64()
        .filter(|v| (1..=3600).contains(v))
        .ok_or("gate timeout must be 1..3600 seconds")?;
    let started = stamp();
    let clock = Instant::now();
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if clock.elapsed() >= Duration::from_secs(limit) {
            timed_out = true;
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let after = identity(package)?;
    let unchanged = before == after
        && lock_sha == digest(lock)?
        && executable_sha == digest(executable)?
        && pinned_inputs(&spec)? == inputs;
    let raw = load(receipt);
    let mut failures = Vec::new();
    if timed_out {
        failures.push("gate timed out".to_string());
    }
    if !status.success() {
        failures.push("gate command failed".into());
    }
    if !unchanged {
        failures.push("gate source/input identity changed".into());
    }
    let raw = match raw {
        Ok(v) => v,
        Err(_) => {
            failures.push("gate did not produce bounded JSON evidence".into());
            Value::Null
        }
    };
    let value = json!({"schema":RUN_SCHEMA,"result":if failures.is_empty(){"pass"}else{"blocked"},"kind":kind,"window_sha256":digest(window_path)?,"candidate":before,"source_lock_sha256":lock_sha,"executable_sha256":executable_sha,"command_spec_sha256":hash(&serde_json::to_vec(&spec)?),"command_argv_sha256":hash(&serde_json::to_vec(&argv)?),"started_at":started,"finished_at":stamp(),"elapsed_ms":clock.elapsed().as_millis()as u64,"exit_code":status.code(),"timed_out":timed_out,"inputs":inputs,"raw_receipt_schema":raw.get("schema").and_then(Value::as_str),"raw_receipt_path":receipt,"raw_receipt_sha256":if receipt.is_file(){Some(input_digest(receipt)?)}else{None},"findings":failures});
    let output = Path::new(args.get("--output")?);
    external(args, output, &[package, executable, window_path, lock])?;
    immutable(output, &value)?;
    Ok(value)
}
fn read_map(v: &Value) -> Result<BTreeMap<String, String>> {
    Ok(serde_json::from_value(v.clone())?)
}
fn input_json(run: &Value, sha: &str) -> Result<Value> {
    require(hex(sha, 64), "gate evidence digest invalid")?;
    let reference = run["inputs"]
        .as_array()
        .ok_or("gate inputs absent")?
        .iter()
        .find(|v| v["sha256"] == sha)
        .ok_or("gate did not bind its input")?;
    let path = Path::new(text(reference, "path")?);
    require(digest(path)? == sha, "gate input changed after execution")?;
    load(path)
}
fn validate_gate(
    kind: &str,
    run: &Value,
    w: &Value,
    candidate: &Value,
    lock: &Value,
    window_sha: &str,
    lock_sha: &str,
) -> Result<()> {
    require(
        run["schema"] == RUN_SCHEMA
            && run["kind"] == kind
            && run["result"] == "pass"
            && run["exit_code"] == 0
            && run["timed_out"] == false,
        "gate run incomplete or failed",
    )?;
    require(
        run["window_sha256"] == window_sha
            && run["candidate"] == *candidate
            && run["source_lock_sha256"] == lock_sha,
        "gate source is stale or belongs to another window",
    )?;
    require(
        millis(text(run, "started_at")?)? >= millis(text(w, "started_at")?)?
            && millis(text(run, "finished_at")?)? >= millis(text(run, "started_at")?)?
            && millis(text(run, "finished_at")?)? <= chrono::Utc::now().timestamp_millis(),
        "gate timestamps invalid",
    )?;
    for input in run["inputs"].as_array().ok_or("gate inputs absent")? {
        require(
            input_digest(Path::new(text(input, "path")?))? == text(input, "sha256")?,
            "gate input no longer immutable",
        )?;
    }
    let raw_path = Path::new(text(run, "raw_receipt_path")?);
    require(
        input_digest(raw_path)? == text(run, "raw_receipt_sha256")?,
        "raw gate receipt changed",
    )?;
    let raw_value = load(raw_path)?;
    let raw = &raw_value;
    let writer = json!({"upstream_revision":candidate["upstream_revision"],"schema_sha256":lock["build"]["app_server_schema_sha256"]});
    match kind {
        "history" => {
            require(
                raw["schema"] == "ai-platform/history-gate/v1"
                    && raw["result"] == "PASS"
                    && raw["direction"] == "forward"
                    && raw["skipped"] == 0
                    && raw["decoder"] == writer
                    && raw["candidate"] == writer
                    && raw["source_lock_sha256"] == lock_sha,
                "N-1 decoder gate absent or stale",
            )?;
            require(
                raw["previous"]["upstream_revision"] == w["baseline"]["upstream_revision"]
                    && raw["previous"]["schema_sha256"] == w["baseline_schema"]["sha256"],
                "history is not the actual opened N-1 baseline",
            )?;
            let manifest = input_json(run, text(raw, "manifest_sha256")?)?;
            require(
                manifest["candidate"] == writer && manifest["previous"] == raw["previous"],
                "history corpus identity differs",
            )?;
            let required = [
                "session_start",
                "ordinary_turn",
                "tool_roundtrip",
                "approval_pause",
                "cancelled_turn",
                "failed_turn",
                "compacted_resume",
                "subagent_history",
            ];
            coverage(&raw["observations"], &required, "scenario", "items")?;
            let manifest_ref = run["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["sha256"] == raw["manifest_sha256"])
                .ok_or("corpus reference absent")?;
            let corpus_root = Path::new(text(manifest_ref, "path")?)
                .parent()
                .ok_or("corpus parent absent")?;
            for case in manifest["cases"]
                .as_array()
                .ok_or("corpus cases absent")?
                .iter()
                .filter(|c| c["direction"] == "forward")
            {
                for (file, sha) in [
                    ("file", "sha256"),
                    ("source_receipt", "source_receipt_sha256"),
                    ("capture_receipt", "capture_receipt_sha256"),
                ] {
                    let path = safe(corpus_root, text(case, file)?)?;
                    require(
                        input_digest(&path)? == text(case, sha)?,
                        "N-1 corpus changed after native decode",
                    )?;
                }
            }
        }
        "replay" => {
            require(
                raw["schema"] == "ai-platform/deterministic-turn-replay/v1"
                    && raw["result"] == "PASS"
                    && raw["candidate"] == writer
                    && raw["transport"] == "deterministic_mock_provider"
                    && raw["external_provider_calls"] == 0
                    && raw["seed"].as_u64().is_some()
                    && raw["skipped"] == 0,
                "recorded deterministic mock replay required",
            )?;
            let required = [
                "ordinary_turn",
                "tool_roundtrip",
                "approval_pause",
                "cancelled_turn",
                "compacted_resume",
            ];
            coverage(&raw["observations"], &required, "scenario", "mock_requests")?;
            for case in raw["observations"].as_array().unwrap() {
                require(
                    hex(text(case, "expected_sha256")?, 64)
                        && case["expected_sha256"] == case["observed_sha256"]
                        && case["repetitions"].as_u64().is_some_and(|v| v >= 2),
                    "replay is not deterministic or differs from frozen oracle",
                )?;
                let capture = input_json(run, text(case, "capture_receipt_sha256")?)?;
                require(
                    capture["schema"] == "ai-platform/history-capture/v1"
                        && capture["method"] == "postgresql_rollout_items"
                        && capture["writer"]
                            == json!({"upstream_revision":w["baseline"]["upstream_revision"],"schema_sha256":w["baseline_schema"]["sha256"]})
                        && capture["item_count"].as_u64().is_some_and(|v| v > 0),
                    "replay cannot substitute synthetic history for a real capture",
                )?;
                let history_sha = text(&capture, "history_sha256")?;
                require(
                    run["inputs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["sha256"] == history_sha),
                    "recorded raw history is not pinned",
                )?;
                let _ = input_json(run, text(case, "oracle_sha256")?)?;
            }
        }
        "eval" => {
            require(
                raw["schema"] == "ai-platform/runtime-eval-smoke/v1"
                    && raw["result"] == "PASS"
                    && raw["candidate"] == writer
                    && raw["skipped"] == 0,
                "source-bound eval smoke required",
            )?;
            coverage(
                &raw["observations"],
                &[
                    "ordinary_turn",
                    "tool_roundtrip",
                    "approval_pause",
                    "cancelled_turn",
                    "compacted_resume",
                ],
                "scenario",
                "cases",
            )?;
            for c in raw["observations"].as_array().unwrap() {
                require(
                    c["passed"] == true && c["failures"] == json!([]),
                    "eval smoke case failed",
                )?;
                let _ = input_json(run, text(c, "observation_sha256")?)?;
            }
        }
        _ => return Err("unknown gate".into()),
    }
    Ok(())
}
fn coverage(cases: &Value, required: &[&str], key: &str, count: &str) -> Result<()> {
    let mut covered = BTreeSet::new();
    for c in cases.as_array().ok_or("gate observations absent")? {
        let label = text(c, key)?;
        require(
            covered.insert(label.to_owned()) && c[count].as_u64().is_some_and(|v| v > 0),
            "empty or duplicate gate scenario",
        )?;
    }
    require(
        required.iter().all(|v| covered.contains(*v)),
        "required gate scenario missing",
    )
}
fn compare(args: &Args) -> Result<Value> {
    let w = window(Path::new(args.get("--window")?))?;
    let lock = load(Path::new(args.get("--source-lock")?))?;
    require(
        lock["source"]["upstream_sha"] == w["tag"]["commit"],
        "diff lock targets another window",
    )?;
    let dir = Path::new(args.get("--schema-dir")?);
    let schema = crate::source::bundle_identity(dir, false)?;
    require(
        schema["sha256"] == lock["build"]["app_server_schema_sha256"],
        "diff schema not source-locked",
    )?;
    let diff = inventory::changed(
        &read_map(&w["baseline_schema_files"])?,
        &inventory::schema_files(dir)?,
    );
    let mut sboms = serde_json::Map::new();
    for (kind, option, key) in [
        ("runtime", "--sbom", "sbom_sha256"),
        ("worker", "--worker-sbom", "capability_worker_sbom_sha256"),
    ] {
        let path = Path::new(args.get(option)?);
        require(
            digest(path)? == lock["build"][key],
            "diff SBOM not source-locked",
        )?;
        let old: BTreeMap<String, Value> =
            serde_json::from_value(w["baseline_sboms"][kind]["components"].clone())?;
        sboms.insert(
            kind.into(),
            inventory::changed(&old, &inventory::sbom_components(path)?),
        );
    }
    let value = json!({"result":"pass","scope":"observed diff only; schema review and release gates still required","baseline_schema_sha256":w["baseline_schema"]["sha256"],"candidate_schema_sha256":schema["sha256"],"schema_diff":diff,"schema_diff_sha256":hash(&serde_json::to_vec(&diff)?),"sbom_diff":sboms});
    let output = Path::new(args.get("--output")?);
    external(args, output, &[dir])?;
    immutable(output, &value)?;
    Ok(value)
}
fn verify(args: &Args) -> Result<Value> {
    let window_path = Path::new(args.get("--window")?);
    let w = window(window_path)?;
    let package = Path::new(args.get("--package")?);
    let previous = Path::new(args.get("--baseline-package")?);
    let checkout = Path::new(args.get("--upstream")?);
    let output = Path::new(args.get("--output")?);
    external(args, output, &[package, previous, checkout, window_path])?;
    let lock_path = Path::new(args.get("--source-lock")?);
    let lock = load(lock_path)?;
    let candidate = identity(package)?;
    let mut findings = Vec::<String>::new();
    let observed = (|| -> Result<Value> {
        registered(args, &w, &digest(window_path)?)?;
        require(
            candidate["upstream_revision"] == w["tag"]["commit"]
                && identity(previous)? == w["baseline"],
            "window source identities changed",
        )?;
        require(
            pinned_tag(
                checkout,
                text(&w["tag"], "name")?,
                text(&candidate, "upstream_revision")?,
            )? == w["tag"],
            "upstream tag moved since opening",
        )?;
        let mut source_args = Args::default();
        source_args.positionals = vec!["source-candidate".into(), "verify".into()];
        source_args
            .values
            .insert("--repo-root".into(), args.root().to_string_lossy().into());
        source_args
            .values
            .insert("--package".into(), package.to_string_lossy().into());
        source_args
            .values
            .insert("--upstream".into(), checkout.to_string_lossy().into());
        let admission = crate::source::candidate(&source_args)?;
        crate::source::validate_lock(&args.root(), lock_path, false, None)?;
        require(
            lock["source"]["upstream_sha"] == candidate["upstream_revision"]
                && lock["build"]["cargo_lock_sha256"] == candidate["cargo_lock_sha256"],
            "candidate differs from source-lock",
        )?;
        let schema = Path::new(args.get("--schema-dir")?);
        let schema_identity = crate::source::bundle_identity(schema, false)?;
        require(
            schema_identity["sha256"] == lock["build"]["app_server_schema_sha256"],
            "compiled App Server schema differs from source-lock",
        )?;
        let old_schema = read_map(&w["baseline_schema_files"])?;
        let schema_diff = inventory::changed(&old_schema, &inventory::schema_files(schema)?);
        let review_path = Path::new(args.get("--schema-review")?);
        let review = load(review_path)?;
        require(
            review["schema"] == "agentforge/app-server-schema-review/v1"
                && review["result"] == "pass"
                && review["baseline_schema_sha256"] == w["baseline_schema"]["sha256"]
                && review["candidate_schema_sha256"] == schema_identity["sha256"]
                && review["diff_sha256"] == hash(&serde_json::to_vec(&schema_diff)?),
            "App Server schema diff lacks source-bound review",
        )?;
        let mut sboms = serde_json::Map::new();
        for (kind, option, key) in [
            ("runtime", "--sbom", "sbom_sha256"),
            ("worker", "--worker-sbom", "capability_worker_sbom_sha256"),
        ] {
            let file = Path::new(args.get(option)?);
            require(
                lock["build"][key] == digest(file)?,
                "candidate SBOM differs from source-lock",
            )?;
            let old: BTreeMap<String, Value> =
                serde_json::from_value(w["baseline_sboms"][kind]["components"].clone())?;
            sboms.insert(
                kind.into(),
                inventory::changed(&old, &inventory::sbom_components(file)?),
            );
        }
        Ok(
            json!({"source_admission":admission,"patch":inventory::metrics(package,previous,checkout)?,"app_server_schema_diff":schema_diff,"app_server_schema_review_sha256":digest(review_path)?,"sbom_diff":sboms}),
        )
    })();
    let observed = match observed {
        Ok(v) => v,
        Err(e) => {
            findings.push(e.to_string());
            Value::Null
        }
    };
    // Still report measured patch size when a different acceptance gate failed.
    let patch = inventory::metrics(package, previous, checkout)
        .unwrap_or_else(|_| json!({"result":"unavailable"}));
    let mut gates = serde_json::Map::new();
    let window_sha = digest(window_path)?;
    let lock_sha = digest(lock_path)?;
    for kind in ["history", "replay", "eval"] {
        let checked = (|| -> Result<Value> {
            let p = Path::new(args.get(&format!("--{kind}"))?);
            let run = load(p)?;
            validate_gate(kind, &run, &w, &candidate, &lock, &window_sha, &lock_sha)?;
            Ok(
                json!({"result":"pass","receipt_sha256":digest(p)?,"finished_at":run["finished_at"]}),
            )
        })();
        gates.insert(
            kind.into(),
            match checked {
                Ok(v) => v,
                Err(e) => {
                    findings.push(format!("{kind}: {e}"));
                    json!({"result":"blocked"})
                }
            },
        );
    }
    let now = chrono::Utc::now().timestamp_millis();
    let start = millis(text(&w, "started_at")?)?;
    if now < start {
        findings.push("wall clock precedes window opening".into());
    }
    // Green is observed only now, after source/schema/SBOM and all actual gates
    // have verified. Missing gates never get a manufactured duration-to-green.
    let previous_report = (|| -> Result<Option<Value>> {
        let Some(path) = args.values.get("--previous-report") else {
            return Ok(None);
        };
        let prior = load(Path::new(path))?;
        let rows = registered_windows(&args.root())?;
        let row = rows
            .iter()
            .find(|r| r["month"] == prior["month"])
            .ok_or("previous report is absent from sole ledger")?;
        require(
            row["report_sha256"] == digest(Path::new(path))?
                && row["window_sha256"] == prior["window_sha256"]
                && row["target_commit"] == prior["tag"]["commit"],
            "previous report differs from sole ledger pin",
        )?;
        Ok(Some(prior))
    })();
    let previous_report = match previous_report {
        Ok(v) => v,
        Err(e) => {
            findings.push(e.to_string());
            None
        }
    };
    let continuity = match continuity(
        &w,
        &candidate,
        previous_report.as_ref(),
        patch["modified_upstream_files"].as_u64(),
        findings.is_empty(),
    ) {
        Ok(v) => v,
        Err(e) => {
            findings.push(e.to_string());
            json!({"target_modified_upstream_files":15,"met":false,"result":"invalid_window_continuity"})
        }
    };
    let green = findings.is_empty();
    let value = json!({"schema":REPORT_SCHEMA,"result":if green{"pass"}else{"blocked"},"window_sha256":window_sha,"month":w["month"],"tag":w["tag"],"candidate":candidate,"baseline":w["baseline"],"started_at":w["started_at"],"observed_at":stamp(),"source_lock_sha256":lock_sha,"time_to_green_ms":if green{Some(now.saturating_sub(start)as u64)}else{None},"elapsed_since_open_ms":now.saturating_sub(start).max(0),"initial_candidate_source_unit_sha256":w["initial_candidate"]["source_unit_sha256"],"patch":patch,"observed":observed,"gates":gates,"findings":findings,"two_window_target":continuity,"program_ledger":"deploy/runbooks/native-enterprise-upgrade/loop-state.json","product_acceptance":false,"upstream_changes_applied":false});
    immutable(output, &value)?;
    Ok(value)
}
fn continuity(
    w: &Value,
    candidate: &Value,
    previous: Option<&Value>,
    files: Option<u64>,
    green: bool,
) -> Result<Value> {
    let Some(p) = previous else {
        return Ok(
            json!({"target_modified_upstream_files":15,"observed_windows":1,"result":"pending_second_real_month","met":false}),
        );
    };
    require(
        p["schema"] == REPORT_SCHEMA
            && p["result"] == "pass"
            && p["time_to_green_ms"].as_u64().is_some(),
        "previous window is not source-green",
    )?;
    let old = chrono::NaiveDate::parse_from_str(&format!("{}-01", text(p, "month")?), "%Y-%m-%d")?;
    let current =
        chrono::NaiveDate::parse_from_str(&format!("{}-01", text(w, "month")?), "%Y-%m-%d")?;
    require(
        old.checked_add_months(chrono::Months::new(1)) == Some(current)
            && p["candidate"]["upstream_revision"] == w["baseline"]["upstream_revision"]
            && p["candidate"]["upstream_tree"] == w["baseline"]["upstream_tree"]
            && p["tag"]["commit"] != candidate["upstream_revision"],
        "two windows must be adjacent real months and exact upstream-successors",
    )?;
    Ok(
        json!({"target_modified_upstream_files":15,"observed_windows":2,"previous_report_content_sha256":hash(&serde_json::to_vec(p)?),"previous_month":p["month"],"intervening_platform_source_changed":p["candidate"]["source_unit_sha256"]!=w["baseline"]["source_unit_sha256"],"result":if green&&files.is_some_and(|n|n<=15){"target_met_in_second_window"}else{"target_not_met"},"met":green&&files.is_some_and(|n|n<=15)}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    require(
        args.flags.is_empty() && args.positionals.len() == 2,
        "upstream-sync requires one operation and explicit valued options",
    )?;
    let op = args.positionals[1].as_str();
    let allowed: &[&str] = match op {
        "begin" => &[
            "--repo-root",
            "--month",
            "--tag",
            "--package",
            "--baseline-package",
            "--upstream",
            "--baseline-lock",
            "--baseline-source-receipt",
            "--baseline-schema",
            "--baseline-sbom",
            "--baseline-worker-sbom",
            "--output",
        ],
        "record-gate" => &[
            "--repo-root",
            "--window",
            "--package",
            "--source-lock",
            "--kind",
            "--spec",
            "--output",
        ],
        "verify" => &[
            "--repo-root",
            "--window",
            "--package",
            "--baseline-package",
            "--upstream",
            "--source-lock",
            "--schema-dir",
            "--schema-review",
            "--sbom",
            "--worker-sbom",
            "--history",
            "--replay",
            "--eval",
            "--previous-report",
            "--output",
        ],
        "compare" => &[
            "--repo-root",
            "--window",
            "--source-lock",
            "--schema-dir",
            "--sbom",
            "--worker-sbom",
            "--output",
        ],
        "inspect" => &[
            "--repo-root",
            "--package",
            "--baseline-package",
            "--upstream",
        ],
        _ => return Err("upstream-sync {begin|compare|record-gate|verify|inspect}".into()),
    };
    for (key, values) in &args.multiple {
        require(
            allowed.contains(&key.as_str()) && values.len() == 1,
            "unknown or duplicate upstream-sync option",
        )?;
    }
    match op {
        "begin" => anchor(args),
        "record-gate" => record(args),
        "verify" => verify(args),
        "compare" => compare(args),
        "inspect" => Ok(
            json!({"result":"pass","scope":"metrics only; no release acceptance","patch":inventory::metrics(Path::new(args.get("--package")?),Path::new(args.get("--baseline-package")?),Path::new(args.get("--upstream")?))?}),
        ),
        _ => unreachable!(),
    }
}
#[cfg(test)]
#[path = "upstream_sync_tests.rs"]
mod tests;
