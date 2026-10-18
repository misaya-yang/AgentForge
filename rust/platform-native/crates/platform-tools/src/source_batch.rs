//! Source-only batch producer admission. The v1/24-command protocol stays strict.
//! Every receipt represents one real invocation; coverage comes from Cargo target
//! artifacts and complete unfiltered libtest inventories, never synthetic receipts.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

const REQUIRED_SERVICE_TESTS: &str = include_str!("source_batch/required-service-tests.tsv");
pub const UNIT_SCHEMA: &str = "ai-platform/runtime-source-unit/v2";
const PACKAGES: [&str; 6] = [
    "ai-platform-capability-contract",
    "ai-platform-capability-worker",
    "ai-platform-agent-runtime",
    "ai-platform-plugin-quiz",
    "ai-platform-python-compat",
    "ai-platform-office",
];
const BINS: [&str; 11] = [
    "ai-platform-agent-runtime",
    "ai-platform-history-gate",
    "ai-platform-capability-worker",
    "ai-platform-capability-worker-health",
    "ai-platform-plugin-packs",
    "ai-platform-python-supervisor",
    "ai-platform-python-sandbox",
    "ai-platform-python-sandbox-health",
    "ai-platform-sandbox-pool",
    "ai-platform-sandbox-attestor",
    "ai-platform-sandbox-measurement",
];
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| format!("missing {k}").into())
}
fn argv(operation: &str) -> Vec<String> {
    let mut args = vec!["cargo", "+1.95.0", operation, "--locked", "--offline"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match operation {
        "check" => args.extend(["--all-targets", "--keep-going"].map(str::to_owned)),
        "test" => args.extend(
            ["--lib", "--bins", "--no-fail-fast", "--message-format=json"].map(str::to_owned),
        ),
        "build" => args.extend(["--all-targets", "--message-format=json"].map(str::to_owned)),
        _ => unreachable!(),
    }
    args.extend(["--features", "ai-platform-plugin-quiz/executor"].map(str::to_owned));
    for package in PACKAGES {
        args.extend(["-p".to_owned(), package.to_owned()]);
    }
    args
}
fn expected_commands() -> BTreeMap<&'static str, Vec<String>> {
    let metadata = [
        "cargo",
        "+1.95.0",
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1",
    ]
    .map(str::to_owned)
    .to_vec();
    let mut list = argv("test");
    list.extend(["--", "--list"].map(str::to_owned));
    let mut docs = vec![
        "cargo",
        "+1.95.0",
        "test",
        "--locked",
        "--offline",
        "--doc",
        "--no-fail-fast",
        "--features",
        "ai-platform-plugin-quiz/executor",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for package in PACKAGES {
        docs.extend(["-p".to_owned(), package.to_owned()]);
    }
    BTreeMap::from([
        ("batch-metadata", metadata),
        ("batch-check", argv("check")),
        ("batch-list", list),
        ("batch-tests", argv("test")),
        ("batch-docs", docs),
        ("batch-bins", argv("build")),
    ])
}
fn binding(unit: &Value) -> Value {
    json!({"target_commit":unit["target"],"target_tree":unit["target_tree"],
        "candidate_source_sha256":unit["input_candidate_manifest_sha256"],"resolved_lock_sha256":unit["resolved_lock_sha256"]})
}
fn command(
    package: &Path,
    unit: &Value,
    stem: &str,
    expected: &[String],
    evidence_bound: bool,
) -> Result<Value> {
    let relative = format!("evidence/{stem}.json");
    let p = safe(package, &relative)?;
    let receipt = object(&p)?;
    require(
        receipt["exit_code"] == 0 && receipt["stop_reason"].is_null(),
        "batch command failed/interrupted",
    )?;
    require(
        receipt["command"] == json!(expected),
        "batch command coverage/flags changed",
    )?;
    require(
        receipt["source_binding"] == binding(unit),
        "batch command source binding mismatch",
    )?;
    require(
        receipt["cwd"] == "/source/codex-rs",
        "batch command cwd mismatch",
    )?;
    if evidence_bound {
        require(
            unit["evidence"][&relative] == digest(&p)?,
            "command receipt not unit-bound",
        )?;
    }
    for stream in ["stdout", "stderr"] {
        let path = safe(package, &format!("evidence/{stem}.{stream}"))?;
        require(
            receipt[format!("{stream}_sha256")] == digest(&path)?,
            "command stream hash mismatch",
        )?;
    }
    Ok(receipt)
}
#[derive(Clone, Debug)]
struct Target {
    package: String,
    name: String,
    kind: String,
    executable: String,
}
fn targets(metadata: &Value) -> Result<BTreeMap<(String, String, String), String>> {
    let mut result = BTreeMap::new();
    for name in PACKAGES {
        let matches = metadata["packages"]
            .as_array()
            .ok_or("metadata packages missing")?
            .iter()
            .filter(|p| p["name"] == name)
            .collect::<Vec<_>>();
        require(
            matches.len() == 1,
            "missing/duplicate required package metadata",
        )?;
        let p = matches[0];
        require(
            metadata["workspace_members"]
                .as_array()
                .is_some_and(|m| m.contains(&p["id"])),
            "required package is not workspace member",
        )?;
        for t in p["targets"].as_array().ok_or("metadata targets missing")? {
            let kinds = t["kind"].as_array().ok_or("target kind missing")?;
            if t["test"] != true {
                continue;
            }
            if name == "ai-platform-capability-contract" {
                require(
                    kinds.iter().any(|k| k == "lib" || k == "bin"),
                    "Contract full-package test target outside admitted lib/bin closure",
                )?;
            }
            for kind in ["lib", "bin"] {
                if kinds.iter().any(|k| k == kind) {
                    let key = (
                        name.to_owned(),
                        text(t, "name")?.to_owned(),
                        kind.to_owned(),
                    );
                    require(
                        result.insert(key, text(p, "id")?.to_owned()).is_none(),
                        "duplicate metadata target",
                    )?;
                }
            }
        }
    }
    for package in PACKAGES {
        require(
            result
                .keys()
                .any(|(name, _, kind)| name == package && kind == "lib"),
            "required package library tests disabled or absent",
        )?;
    }
    require(!result.is_empty(), "empty batch target set")?;
    Ok(result)
}
fn artifact_targets(stdout: &str, metadata: &Value) -> Result<BTreeMap<String, Target>> {
    let expected = targets(metadata)?;
    let mut seen = BTreeSet::new();
    let mut result = BTreeMap::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact"
            || value["profile"]["test"] != true
            || !value["executable"].is_string()
        {
            continue;
        }
        let id = text(&value, "package_id")?;
        let name = text(&value["target"], "name")?;
        let kinds = value["target"]["kind"]
            .as_array()
            .ok_or("artifact target kind missing")?;
        let found = expected.iter().find(|((_, n, k), pkg)| {
            n == name && pkg.as_str() == id && kinds.iter().any(|v| v == k)
        });
        let Some(((package, _, kind), _)) = found else {
            continue;
        };
        let executable = text(&value, "executable")?.to_owned();
        require(
            executable.starts_with("/cargo-target/debug/deps/")
                && Path::new(&executable)
                    .components()
                    .all(|c| !matches!(c, std::path::Component::ParentDir)),
            "test artifact escaped controlled target cache",
        )?;
        if package == "ai-platform-plugin-quiz" {
            require(
                value["features"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|f| f == "executor")),
                "quiz executor tests not compiled",
            )?;
        }
        require(
            seen.insert((package.clone(), name.to_owned(), kind.clone())),
            "duplicate compiled target artifact",
        )?;
        require(
            result
                .insert(
                    executable.clone(),
                    Target {
                        package: package.clone(),
                        name: name.to_owned(),
                        kind: kind.clone(),
                        executable,
                    },
                )
                .is_none(),
            "duplicate test executable",
        )?;
    }
    require(
        seen == expected.keys().cloned().collect(),
        "compiled artifact set lost required lib/bin targets",
    )?;
    Ok(result)
}
fn running(stderr: &str, artifacts: &BTreeMap<String, Target>) -> Result<Vec<Target>> {
    let re =
        regex::Regex::new(r"^\s*Running unittests .+ \((/cargo-target/debug/deps/[^)]+)\)\s*$")?;
    let mut result = vec![];
    let mut seen = BTreeSet::new();
    for line in stderr.lines() {
        if let Some(c) = re.captures(line) {
            let path = c[1].to_owned();
            require(seen.insert(path.clone()), "test executable ran twice")?;
            result.push(
                artifacts
                    .get(&path)
                    .ok_or("unexpected running test executable")?
                    .clone(),
            );
        }
    }
    require(
        seen == artifacts.keys().cloned().collect(),
        "Cargo did not run every compiled target",
    )?;
    Ok(result)
}
fn listed(stdout: &str) -> Result<Vec<BTreeSet<String>>> {
    let summary = regex::Regex::new(r"^(\d+) tests?, 0 benchmarks?$")?;
    let mut groups = vec![];
    let mut current = BTreeSet::new();
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if serde_json::from_str::<Value>(line)
            .ok()
            .is_some_and(|v| v["reason"].is_string())
        {
            continue;
        }
        if let Some(name) = line.strip_suffix(": test") {
            require(
                !name.is_empty() && current.insert(name.to_owned()),
                "duplicate listed test",
            )?;
        } else if let Some(c) = summary.captures(line) {
            require(
                current.len() == c[1].parse::<usize>()?,
                "list count mismatch",
            )?;
            groups.push(std::mem::take(&mut current));
        } else {
            return Err("unexpected batch list output".into());
        }
    }
    require(
        current.is_empty() && !groups.is_empty(),
        "incomplete batch list",
    )?;
    Ok(groups)
}
fn outcomes(stdout: &str) -> Result<Vec<(BTreeMap<String, bool>, u64, u64)>> {
    let begin = regex::Regex::new(r"^running (\d+) tests?$")?;
    let end = regex::Regex::new(
        r"^test result: ok\. (\d+) passed; 0 failed; (\d+) ignored; 0 measured; 0 filtered out; finished in .+$",
    )?;
    let mut groups = vec![];
    let mut current = None::<(u64, BTreeMap<String, bool>)>;
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(c) = begin.captures(line) {
            require(current.is_none(), "overlapping test target output")?;
            current = Some((c[1].parse()?, BTreeMap::new()));
        } else if let Some(c) = end.captures(line) {
            let (count, names) = current.take().ok_or("unannounced test result")?;
            let passed = c[1].parse::<u64>()?;
            let ignored = c[2].parse::<u64>()?;
            require(
                count == passed + ignored && count == names.len() as u64,
                "executed test count mismatch",
            )?;
            require(
                names.values().filter(|v| **v).count() as u64 == passed,
                "test outcome summary mismatch",
            )?;
            groups.push((names, passed, ignored));
        } else if let Some(line) = line.strip_prefix("test ") {
            let (raw, result) = line.rsplit_once(" ... ").ok_or("malformed test outcome")?;
            let name = raw.strip_suffix(" - should panic").unwrap_or(raw);
            let pass = if result == "ok" {
                true
            } else if result == "ignored" || result.starts_with("ignored, ") {
                false
            } else {
                return Err("test failed or unknown outcome".into());
            };
            require(
                current
                    .as_mut()
                    .ok_or("test result outside target")?
                    .1
                    .insert(name.to_owned(), pass)
                    .is_none(),
                "duplicate executed test",
            )?;
        }
    }
    require(
        current.is_none() && !groups.is_empty(),
        "incomplete executed target output",
    )?;
    Ok(groups)
}
fn final_commands() -> BTreeMap<&'static str, Vec<String>> {
    BTreeMap::from([
        (
            "metadata-command",
            vec![
                "cargo",
                "+1.95.0",
                "metadata",
                "--locked",
                "--offline",
                "--format-version",
                "1",
            ],
        ),
        (
            "schema-command",
            vec![
                "cargo",
                "+1.95.0",
                "run",
                "--locked",
                "--offline",
                "-p",
                "codex-app-server-protocol",
                "--example",
                "ai_platform_export_schema",
                "--",
                "/export/schema",
            ],
        ),
    ])
    .into_iter()
    .map(|(k, v)| (k, v.into_iter().map(str::to_owned).collect()))
    .collect()
}
fn verify_normal_artifacts(stdout: &str, metadata: &Value) -> Result<()> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("metadata packages missing")?;
    let mut owners = BTreeMap::new();
    for (index, name) in BINS.into_iter().enumerate() {
        let owner = if index < 2 {
            "ai-platform-agent-runtime"
        } else {
            "ai-platform-capability-worker"
        };
        let package = packages
            .iter()
            .find(|p| p["name"] == owner)
            .ok_or("normal ELF owning package absent")?;
        require(
            package["targets"].as_array().is_some_and(|targets| {
                targets
                    .iter()
                    .any(|t| t["name"] == name && t["kind"] == json!(["bin"]))
            }),
            "normal ELF target absent from owning package metadata",
        )?;
        owners.insert(name, text(package, "id")?);
    }
    let mut seen = BTreeSet::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != false {
            continue;
        }
        let Some(name) = v["target"]["name"].as_str() else {
            continue;
        };
        if !BINS.contains(&name) {
            continue;
        }
        require(
            v["package_id"] == owners[name]
                && v["target"]["kind"] == json!(["bin"])
                && v["executable"] == format!("/cargo-target/debug/{name}"),
            "normal ELF artifact path/kind mismatch",
        )?;
        require(
            seen.insert(name.to_owned()),
            "duplicate normal ELF artifact",
        )?;
    }
    require(
        seen == BINS.into_iter().map(str::to_owned).collect(),
        "normal build omitted required eleven ELF artifacts",
    )
}
fn metadata(package: &Path) -> Result<Value> {
    object(&package.join("evidence/resolved-metadata.json"))
}
fn summarize(
    package: &Path,
    unit: &Value,
    evidence_bound: bool,
    hash_executables: bool,
) -> Result<Value> {
    for (stem, expected) in expected_commands() {
        command(package, unit, stem, &expected, evidence_bound)?;
    }
    require(
        std::fs::read_to_string(package.join("evidence/required-service-tests.tsv"))?
            == REQUIRED_SERVICE_TESTS,
        "required service test policy changed",
    )?;
    let metadata = metadata(package)?;
    require(
        std::fs::read(package.join("evidence/resolved-metadata.json"))?
            == std::fs::read(package.join("evidence/batch-metadata.stdout"))?,
        "resolved metadata is not actual Cargo stdout",
    )?;
    verify_normal_artifacts(
        &std::fs::read_to_string(package.join("evidence/batch-bins.stdout"))?,
        &metadata,
    )?;
    let list = std::fs::read_to_string(package.join("evidence/batch-list.stdout"))?;
    let tests = std::fs::read_to_string(package.join("evidence/batch-tests.stdout"))?;
    let artifacts = artifact_targets(&list, &metadata)?;
    require(
        artifacts.keys().collect::<Vec<_>>()
            == artifact_targets(&tests, &metadata)?
                .keys()
                .collect::<Vec<_>>(),
        "list/run artifact targets differ",
    )?;
    let order = running(
        &std::fs::read_to_string(package.join("evidence/batch-list.stderr"))?,
        &artifacts,
    )?;
    let run_order = running(
        &std::fs::read_to_string(package.join("evidence/batch-tests.stderr"))?,
        &artifacts,
    )?;
    let lists = listed(&list)?;
    let results = outcomes(&tests)?;
    require(
        order.len() == lists.len()
            && order.len() == results.len()
            && order
                .iter()
                .map(|t| &t.executable)
                .eq(run_order.iter().map(|t| &t.executable)),
        "target output grouping/order mismatch",
    )?;
    let mut rows = vec![];
    let mut passed = 0;
    let mut ignored = 0;
    for ((target, names), (executed, p, i)) in order.iter().zip(lists).zip(results) {
        require(
            names == executed.keys().cloned().collect(),
            "test run filtered/replaced the full compiled inventory",
        )?;
        if target.package == "ai-platform-capability-contract" && target.kind == "lib" {
            require(
                i == 0
                    && crate::source::required_contract_tests()
                        .iter()
                        .all(|n| names.contains(*n)),
                "original strict Contract tests lost/ignored",
            )?;
        }
        let scope = match (target.package.as_str(), target.kind.as_str()) {
            ("ai-platform-agent-runtime", "lib") => "runtime-lib",
            ("ai-platform-agent-runtime", "bin") => "runtime-bin",
            ("ai-platform-capability-worker", "lib") => "worker-lib",
            ("ai-platform-capability-worker", "bin") => "worker-bin",
            ("ai-platform-plugin-quiz", "lib") => "plugin-lib",
            ("ai-platform-python-compat", "lib") => "compat-lib",
            ("ai-platform-office", "lib") => "office-lib",
            _ => "contract-lib",
        };
        for line in
            std::fs::read_to_string(package.join("evidence/required-service-tests.tsv"))?.lines()
        {
            let (required, leaf) = line
                .split_once('\t')
                .ok_or("required test policy malformed")?;
            if required == scope && target.kind == "lib" {
                require(
                    names.iter().any(|n| n.ends_with(&format!("::{leaf}"))),
                    "required lib test disappeared",
                )?;
            }
        }
        let sha = if hash_executables {
            digest(Path::new(&target.executable))?
        } else {
            String::new()
        };
        rows.push(json!({"package":target.package,"target":target.name,"kind":target.kind,"executable":target.executable,"executable_sha256":sha,"scope":scope,"listed":names,"passed":p,"ignored":i,"failed":0,"filtered_out":0}));
        passed += p;
        ignored += i;
    }
    // Bin-required leaves are checked across the package's complete bin group.
    for line in
        std::fs::read_to_string(package.join("evidence/required-service-tests.tsv"))?.lines()
    {
        let (scope, leaf) = line.split_once('\t').ok_or("test policy malformed")?;
        require(
            rows.iter().filter(|r| r["scope"] == scope).any(|r| {
                r["listed"].as_array().is_some_and(|names| {
                    names.iter().any(|n| {
                        n.as_str()
                            .is_some_and(|n| n.ends_with(&format!("::{leaf}")))
                    })
                })
            }),
            "required service test missing across complete targets",
        )?;
    }
    Ok(
        json!({"schema":"agentforge/complete-batch-test-inventory/v1","source_binding":binding(unit),"targets":rows,"passed":passed,"ignored":ignored,"failed":0,"filtered_out":0,"product_acceptance":false}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    require(
        args.positionals.len() == 2 && args.flags.is_empty(),
        "unexpected source-batch positional/flag",
    )?;
    let allowed = if args.positionals.get(1).is_some_and(|v| v == "summarize") {
        vec!["--package", "--template"]
    } else {
        vec![
            "--repo-root",
            "--package",
            "--upstream",
            "--composed-source",
            "--export-root",
            "--output",
        ]
    };
    require(
        args.multiple
            .iter()
            .all(|(key, values)| allowed.contains(&key.as_str()) && values.len() == 1),
        "unexpected or repeated source-batch option",
    )?;
    match args.positionals.get(1).map(String::as_str) {
        Some("summarize") => {
            let package = Path::new(args.get("--package")?);
            let unit = object(Path::new(args.get("--template")?))?;
            summarize(package, &unit, false, true)
        }
        Some("verify") => verify(args),
        _ => Err(
            "source-batch {summarize|verify} required; old source-candidate v1 remains separate"
                .into(),
        ),
    }
}
fn verify(args: &Args) -> Result<Value> {
    crate::source_sync::verify_if_present(&args.root())?;
    let package = Path::new(args.get("--package")?);
    let unit = object(&package.join("source-unit.json"))?;
    crate::source::verify_payload_integrity(package, &unit, UNIT_SCHEMA)?;
    for relative in [
        "evidence/batch-test-inventory.json",
        "evidence/required-service-tests.tsv",
        "evidence/built-binaries.json",
        "evidence/resolved-metadata.json",
        "evidence/collector-source.rs",
        "evidence/collector-steps.rs",
        "evidence/resource-budget.json",
        "evidence/native-validator-identity.json",
        "evidence/protocol-equivalence.json",
    ] {
        require(
            unit["evidence"][relative] == digest(&safe(package, relative)?)?,
            "required batch evidence not source-unit bound",
        )?;
    }
    let computed = summarize(package, &unit, true, false)?;
    let stored = object(&package.join("evidence/batch-test-inventory.json"))?;
    let mut normalized = stored.clone();
    for target in normalized["targets"]
        .as_array_mut()
        .ok_or("inventory targets missing")?
    {
        require(
            target["executable_sha256"].as_str().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            }),
            "test executable hash missing",
        )?;
        target["executable_sha256"] = json!("");
    }
    require(
        normalized == computed,
        "stored batch test inventory differs from actual command streams",
    )?;
    let entries = crate::source::upstream(&unit, Path::new(args.get("--upstream")?))?;
    crate::source::live_preimages(package, &args.root())?;
    let compared =
        crate::source::verify_composed(&unit, &entries, Path::new(args.get("--composed-source")?))?;
    let binaries = object(&package.join("evidence/built-binaries.json"))?;
    require(
        binaries["schema"] == "agentforge/source-bound-built-binaries/v1"
            && binaries["source_binding"] == binding(&unit)
            && binaries["images_built"] == false
            && binaries["services_started"] == false,
        "binary receipt identity mismatch",
    )?;
    let rows = binaries["binaries"].as_array().ok_or("bin set missing")?;
    require(
        rows.len() == 11
            && rows
                .iter()
                .filter_map(|r| r["name"].as_str())
                .collect::<BTreeSet<_>>()
                == BINS.into_iter().collect(),
        "required eleven ELF set changed",
    )?;
    for row in rows {
        require(
            row["bytes"].as_u64().is_some_and(|n| n > 0)
                && row["sha256"].as_str().is_some_and(|s| {
                    s.len() == 64
                        && s.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                }),
            "invalid ELF digest/size",
        )?;
    }
    let export = Path::new(args.get("--export-root")?);
    let evidence = object(&export.join("export-evidence.json"))?;
    require(
        evidence["schema"] == "ai-platform/composed-source-export/v3"
            && evidence["source_unit_sha256_before"] == digest(&package.join("source-unit.json"))?
            && evidence["source_unit_sha256_after"] == evidence["source_unit_sha256_before"]
            && evidence["resolved_lock_sha256"] == unit["resolved_lock_sha256"],
        "final export source identity mismatch",
    )?;
    let files = evidence["files"]
        .as_object()
        .ok_or("export files missing")?;
    let required = [
        ("source_inventory_before", "source-inventory-before.json"),
        ("source_inventory_after", "source-inventory-after.json"),
        ("schema_stdout", "schema.stdout"),
        ("schema_command_receipt", "schema-command.json"),
        ("metadata_command_receipt", "metadata-command.json"),
        ("cargo_metadata", "metadata.json"),
        ("cargo_lock", "Cargo.lock"),
        ("cargo_version", "cargo-version.txt"),
        ("rustc_version", "rustc-version.txt"),
    ];
    require(
        files.len() == required.len(),
        "export required file set changed",
    )?;
    for (key, path) in required {
        require(
            files.get(key).is_some_and(|r| r["path"] == path),
            "missing required export file",
        )?;
        require(
            digest(&safe(export, path)?)? == files[key]["sha256"],
            "export file digest mismatch",
        )?;
    }
    let before = std::fs::read(export.join("source-inventory-before.json"))?;
    require(
        before == std::fs::read(export.join("source-inventory-after.json"))?,
        "source inventory changed during execution",
    )?;
    require(
        crate::source::bundle_identity(&export.join("schema"), false)? == evidence["schema_bundle"],
        "schema bundle changed or empty",
    )?;
    require(
        digest(&export.join("Cargo.lock"))? == unit["resolved_lock_sha256"],
        "export lock changed",
    )?;
    let expected_binding = json!({"target_commit":unit["target"],"target_tree":unit["target_tree"],"source_unit_sha256":digest(&package.join("source-unit.json"))?,"resolved_lock_sha256":unit["resolved_lock_sha256"],"source_inventory_sha256":format!("{:x}",Sha256::digest(&before))});
    require(
        evidence["execution"]["source_binding"] == expected_binding
            && evidence["execution"]["container_source_root"] == "/source"
            && evidence["execution"]["container_cwd"] == "/source/codex-rs"
            && evidence["target_commit"] == unit["target"],
        "export execution context mismatch",
    )?;
    for (stem, expected) in final_commands() {
        let r = object(&export.join(format!("{stem}.json")))?;
        require(
            r["command"] == json!(expected)
                && r["cwd"] == "/source/codex-rs"
                && r["exit_code"] == 0
                && r["stop_reason"].is_null()
                && r["source_binding"] == expected_binding,
            "final schema/metadata command not source bound",
        )?;
        for stream in ["stdout", "stderr"] {
            require(
                r[format!("{stream}_sha256")]
                    == digest(&safe(export, &format!("{stem}.{stream}"))?)?,
                "final command stream changed",
            )?;
        }
    }
    require(
        std::fs::read(export.join("metadata.json"))?
            == std::fs::read(export.join("metadata-command.stdout"))?
            && std::fs::read(export.join("schema.stdout"))?
                == std::fs::read(export.join("schema-command.stdout"))?,
        "export stdout copy changed",
    )?;
    // Run every original composed-export/SBOM gate via a separate v3 path;
    // never rewrite a new receipt into the historical strict v1 protocol.
    let mut values = args.values.clone();
    values.insert("--composed".into(), args.get("--composed-source")?.into());
    values.insert(
        "--export-evidence".into(),
        export
            .join("export-evidence.json")
            .to_string_lossy()
            .into_owned(),
    );
    values.insert(
        "--schema-dir".into(),
        export.join("schema").to_string_lossy().into_owned(),
    );
    args.get("--output")?;
    let derived = crate::source::generate_composed_batch_receipt(&Args {
        values,
        ..Default::default()
    })?;
    Ok(
        json!({"schema":"agentforge/source-batch-admission/v1","success":true,"source_unit_sha256":digest(&package.join("source-unit.json"))?,"candidate_source_sha256":unit["input_candidate_manifest_sha256"],"resolved_lock_sha256":unit["resolved_lock_sha256"],"compared_composed_files":compared,"test_targets":stored["targets"].as_array().unwrap().len(),"passed":stored["passed"],"ignored":stored["ignored"],"failed":0,"filtered_out":0,"actual_cargo_invocations":8,"required_normal_elfs":11,"derived_source_evidence":derived,"production_admitted":false,"generated_release_identity":false}),
    )
}
#[cfg(test)]
mod tests;
