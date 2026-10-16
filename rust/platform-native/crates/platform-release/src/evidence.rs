use crate::*;
use serde_json::json;
use std::collections::BTreeSet;
const SCENARIOS: &[(&str, &str, &str)] = &[
    ("offline-release-suite", "L1", "release"),
    ("database-fresh-upgrade-role-matrix", "L2", "release"),
    ("multi-arch-artifact-and-version-agreement", "L3", "release"),
    ("docker-source-image-identity", "L3", "release"),
    ("assistant-live-journeys", "L3", "release"),
    ("knowledge-live-journeys", "L3", "release"),
    ("degraded-failure-recovery", "L3", "release"),
    ("compact-scale-topology", "L3", "release"),
    ("fresh-environment-quickstart", "L3", "release"),
    ("current-frozen-current", "L3", "rollback"),
    ("database-recovery-classes", "L3", "rollback"),
    ("final-review-and-closeout", "L3", "release"),
];
pub fn receipt(
    v: &Value,
    gate: Option<&str>,
    release: Option<&str>,
    source: Option<&str>,
    pass: bool,
) -> Result<Value> {
    require(
        v["schema_version"] == "ai-platform/integration-gate-receipt/v1",
        "unsupported integration receipt schema",
    )?;
    let g = text(v, "gate")?;
    let result = text(v, "result")?;
    require(gate.is_none_or(|x| x == g), "receipt gate mismatch")?;
    require(
        ["blocked", "dry-run", "fail", "pass"].contains(&result),
        "invalid receipt result",
    )?;
    require(
        release.is_none_or(|x| v["release_id"] == x),
        "receipt release mismatch",
    )?;
    require(
        source.is_none_or(|x| v["source_git_sha"] == x),
        "receipt source mismatch",
    )?;
    let skips = v["unexpected_skips"].as_u64().ok_or("invalid skip count")?;
    let steps = array(v, "steps")?;
    require(
        !pass || (result == "pass" && skips == 0 && !steps.is_empty()),
        "receipt not zero-skip pass",
    )?;
    for s in steps {
        let c = array(s, "command")?;
        require(!c.is_empty(), "empty receipt command")?;
        let exit = s["exit_code"].as_i64().ok_or("invalid exit code")?;
        let skipped = s["skip_markers"].as_u64().ok_or("invalid step skips")?;
        require(hex(text(s, "output_sha256")?, 64), "invalid output hash")?;
        require(!pass || (exit == 0 && skipped == 0), "receipt step failed")?;
    }
    Ok(json!({"gate":g,"result":result,"steps":steps.len(),"unexpected_skips":skips}))
}
fn commands(v: &Value) -> Result<Vec<Vec<String>>> {
    let rows = array(v, "commands")?;
    require(!rows.is_empty(), "scenario has no commands")?;
    rows.iter()
        .map(|row| {
            let a = row.as_array().ok_or("invalid command")?;
            require(!a.is_empty(), "empty command")?;
            a.iter()
                .map(|p| {
                    let s = p.as_str().ok_or("nontext command")?;
                    require(!s.is_empty(), "empty argument")?;
                    Ok(s.to_owned())
                })
                .collect()
        })
        .collect()
}
fn entrypoint(root: &Path, c: &[String], targets: &BTreeSet<String>) -> Result<()> {
    match c[0].as_str() {
        "make" => require(
            c.len() > 1 && targets.contains(&c[1]),
            "unknown Make entrypoint",
        ),
        "pnpm" => {
            require(
                c.len() >= 6 && c[1] == "-C" && c[3..6] == ["exec", "playwright", "test"],
                "unsupported pnpm entrypoint",
            )?;
            let project = safe(root, &c[2])?;
            require(
                project.join("package.json").is_file(),
                "missing frontend project",
            )?;
            let i = c
                .iter()
                .position(|s| s == "--config")
                .ok_or("missing Playwright config")?;
            let config = c.get(i + 1).ok_or("missing config argument")?;
            require(safe(&project, config)?.is_file(), "missing config")?;
            for f in c.iter().filter(|s| s.ends_with(".spec.ts")) {
                require(safe(&project, f)?.is_file(), "missing Playwright spec")?;
            }
            Ok(())
        }
        "database-authority" | "platform-release" | "platform-tools" | "platform-eval" => {
            require(c.len() > 1, "missing native subcommand")?;
            Ok(())
        }
        _ => Err(format!("unsupported release executable: {}", c[0]).into()),
    }
}
pub fn matrix(
    root: &Path,
    v: &Value,
    level: &str,
    release: Option<&str>,
    source: Option<&str>,
) -> Result<Value> {
    require(
        ["draft", "candidate"].contains(&level),
        "unknown evidence level",
    )?;
    require(
        v["schema_version"] == "ai-platform/release-rollback-matrix/v1",
        "unsupported matrix schema",
    )?;
    let status = text(v, "status")?;
    let scenarios = array(v, "scenarios")?;
    let make = std::fs::read_to_string(root.join("Makefile"))?;
    let re = regex::Regex::new(r"(?m)^([A-Za-z0-9_.\-]+):")?;
    let targets = re.captures_iter(&make).map(|c| c[1].to_string()).collect();
    let mut seen = BTreeSet::new();
    let mut statuses = BTreeSet::new();
    let mut passed = 0;
    for s in scenarios {
        let id = text(s, "id")?;
        require(seen.insert(id), "duplicate scenario")?;
        let expected = SCENARIOS
            .iter()
            .find(|(i, _, _)| *i == id)
            .ok_or("unknown scenario")?;
        require(
            s["tier"] == expected.1 && s["category"] == expected.2 && s["required"] == true,
            "scenario contract drift",
        )?;
        let cs = commands(s)?;
        for c in &cs {
            entrypoint(root, c, &targets)?;
        }
        let state = text(s, "status")?;
        require(
            ["NOT_RUN", "BLOCKED", "FAIL", "PASS"].contains(&state),
            "invalid scenario status",
        )?;
        statuses.insert(state);
        let paths = array(s, "evidence")?
            .iter()
            .map(|x| {
                let raw = x.as_str().ok_or("invalid evidence path")?;
                require(raw.starts_with("reports/"), "evidence not durable")?;
                safe(root, raw)
            })
            .collect::<Result<Vec<_>>>()?;
        if ["NOT_RUN", "BLOCKED"].contains(&state) {
            text(s, "blocker")?;
            require(paths.is_empty(), "unexecuted scenario carries evidence")?;
        } else {
            require(
                s["blocker"].is_null() || s["blocker"] == "",
                "executed scenario retains blocker",
            )?;
        }
        if state == "PASS" {
            require(!paths.is_empty(), "PASS lacks evidence")?;
            let mr = text(v, "release_id")?;
            let ms = text(v, "source_git_sha")?;
            require(hex(ms, 40), "invalid source identity")?;
            let mut executed = BTreeSet::new();
            for p in paths {
                require(
                    !p.is_symlink() && p.is_file(),
                    "missing or symlinked evidence",
                )?;
                let r = load(&p)?;
                receipt(&r, None, Some(mr), Some(ms), true)?;
                for step in array(&r, "steps")? {
                    executed.insert(serde_json::to_string(&step["command"])?);
                }
            }
            for c in &cs {
                require(
                    executed.contains(&serde_json::to_string(c)?),
                    "PASS receipt omits command",
                )?;
            }
            passed += 1;
        } else if state == "FAIL" {
            require(!paths.is_empty(), "FAIL lacks evidence")?;
        }
    }
    require(seen.len() == SCENARIOS.len(), "required scenario set drift")?;
    let derived = ["FAIL", "BLOCKED", "NOT_RUN"]
        .into_iter()
        .find(|s| statuses.contains(s))
        .unwrap_or("PASS");
    require(status == derived, "matrix aggregate drift")?;
    require(
        level != "candidate" || status == "PASS",
        "candidate blocked by matrix",
    )?;
    if status == "PASS" || level == "candidate" {
        text(v, "release_id")?;
        require(
            hex(text(v, "source_git_sha")?, 40),
            "missing exact source identity",
        )?;
    }
    require(
        release.is_none_or(|s| v["release_id"] == s),
        "matrix release drift",
    )?;
    require(
        source.is_none_or(|s| v["source_git_sha"] == s),
        "matrix source drift",
    )?;
    Ok(
        json!({"status":status,"scenarios":scenarios.len(),"passed":passed,"not_passed":scenarios.len()-passed}),
    )
}
pub fn retirement(root: &Path, v: &Value) -> Result<Value> {
    require(
        v["schema_version"] == "ai-platform/historical-plan-retirement/v1",
        "unsupported retirement schema",
    )?;
    let successor = text(v, "authoritative_successor")?;
    let p = safe(root, successor)?;
    require(p.is_file() && !p.is_symlink(), "invalid successor")?;
    let entries = array(v, "entries")?;
    require(!entries.is_empty(), "empty retirement")?;
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut superseded = 0;
    for e in entries {
        let id = text(e, "id")?;
        require(ids.insert(id), "duplicate retirement id")?;
        let rel = text(e, "path")?;
        let p = safe(root, rel)?;
        require(
            paths.insert(rel)
                && rel != successor
                && !p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("prd"),
            "invalid retirement path",
        )?;
        let lifecycle = text(e, "lifecycle")?;
        require(
            ["superseded", "archived"].contains(&lifecycle),
            "plan not retired",
        )?;
        require(e["successor"] == successor, "successor drift")?;
        text(e, "reason")?;
        let marker = text(e, "document_marker")?;
        require(p.is_file() && !p.is_symlink(), "missing retired plan")?;
        require(
            std::fs::read_to_string(p)?.contains(marker),
            "retirement marker absent",
        )?;
        if lifecycle == "superseded" {
            superseded += 1;
        }
    }
    Ok(json!({"entries":entries.len(),"superseded":superseded,"archived":entries.len()-superseded}))
}
pub fn schemas(root: &Path) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for (name, file) in [
        ("release_matrix", "release-rollback-matrix-v1.schema.json"),
        (
            "integration_receipt",
            "integration-gate-receipt-v1.schema.json",
        ),
        ("retirement", "historical-plan-retirement-v1.schema.json"),
    ] {
        let p = root.join("deploy/release/schemas").join(file);
        let s = load(&p)?;
        require(
            s["$schema"] == "https://json-schema.org/draft/2020-12/schema" && s["type"] == "object",
            "invalid schema document",
        )?;
        out.insert(name.into(), digest(&p)?.into());
    }
    Ok(out.into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dry_run_is_not_pass() {
        let r = json!({"schema_version":"ai-platform/integration-gate-receipt/v1","gate":"x","result":"dry-run","unexpected_skips":0,"steps":[]});
        assert!(receipt(&r, None, None, None, false).is_ok());
        assert!(receipt(&r, None, None, None, true).is_err());
    }
    #[test]
    fn receipt_checks_identity_and_skips() {
        let mut r = json!({"schema_version":"ai-platform/integration-gate-receipt/v1","gate":"x","release_id":"r","source_git_sha":"s","result":"pass","unexpected_skips":0,"steps":[{"command":["make","test"],"exit_code":0,"skip_markers":0,"output_sha256":"a".repeat(64)}]});
        assert!(receipt(&r, Some("x"), Some("r"), Some("s"), true).is_ok());
        assert!(receipt(&r, Some("y"), None, None, true).is_err());
        r["steps"][0]["skip_markers"] = 1.into();
        assert!(receipt(&r, None, None, None, true).is_err());
    }
}
