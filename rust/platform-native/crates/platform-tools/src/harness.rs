use crate::*;
use std::collections::BTreeSet;
const LINKS: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "docs/README.md",
    "docs/harness/README.md",
    "docs/harness/platform-architecture.md",
    "docs/harness/architecture.md",
    "docs/harness/commands.md",
    "docs/harness/workflow.md",
    "docs/harness/work-packages.md",
    "docs/harness/runtime-and-secrets.md",
    "docs/harness/integration-and-rollback.md",
    "docs/harness/repository-quality.md",
];
pub const PRUNED: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "dist",
    "build",
    "target",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".playwright",
    ".claude",
    "tmp",
    "temp",
    "logs",
    "uploads",
    "test-results",
    "playwright-report",
    "htmlcov",
    ".worktrees",
];
fn strings(v: &Value) -> Vec<&str> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
fn pattern(glob: &str, name: &str) -> bool {
    globset::Glob::new(glob).is_ok_and(|g| g.compile_matcher().is_match(name))
}
fn logical_lines(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut pending = String::new();
    for raw in s.lines() {
        let line = format!("{}{}", pending, raw.trim());
        pending.clear();
        if let Some(prefix) = line.strip_suffix('\\') {
            pending = format!("{} ", prefix.trim_end());
        } else if !line.is_empty() {
            out.push(line);
        }
    }
    if !pending.is_empty() {
        out.push(pending.trim_end().into());
    }
    out
}
fn preserves(line: &str) -> bool {
    if ["||", ";", "$(", "`"].iter().any(|s| line.contains(s))
        || regex::Regex::new(r"(?:^|\s)&(?:\s|$)")
            .unwrap()
            .is_match(line)
    {
        return false;
    }
    let pipes: Vec<_> = line.split('|').collect();
    pipes.len() == 1 || (pipes.len() == 2 && pipes[1].trim_start().starts_with("tee "))
}
pub(crate) fn runs_make(text: &str, target: &str) -> bool {
    if regex::Regex::new(r"\bset\s+\+(?:e|o\s+pipefail)\b")
        .unwrap()
        .is_match(text)
    {
        return false;
    }
    let pattern = regex::Regex::new(&format!(
        r"^\s*((?:[A-Za-z_][A-Za-z0-9_]*=\S+\s+)*)make\s+{}(?:\s|$)",
        regex::escape(target)
    ))
    .unwrap();
    let env = regex::Regex::new(r"(?:^|\s)(?:MAKEFLAGS|MFLAGS|GNUMAKEFLAGS)=").unwrap();
    let unsafe_flags=regex::Regex::new(r"(?:^|\s)(?:-n|--dry-run|--just-print|--reconnaissance|-t|--touch|-q|--question|-f|--file|-C|--directory|--eval)(?:=|\s|$)").unwrap();
    logical_lines(text).iter().any(|l| {
        pattern.captures(l).is_some_and(|m| !env.is_match(&m[1]))
            && preserves(l)
            && !unsafe_flags.is_match(l)
    })
}
pub(crate) fn runs_shell(text: &str, command: &str) -> bool {
    if text.contains("set +e") || text.contains("set +o pipefail") {
        return false;
    }
    let p = regex::Regex::new(&format!(r"^\s*{}(?:\s|$)", regex::escape(command))).unwrap();
    logical_lines(text)
        .iter()
        .any(|l| p.is_match(l) && preserves(l))
}
fn all_make(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            for (k, v) in o {
                if k == "make" {
                    if let Some(s) = v.as_str() {
                        out.insert(s.into());
                    }
                }
                all_make(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                all_make(v, out);
            }
        }
        _ => {}
    }
}
pub fn check(root: &Path) -> Result<Value> {
    let config: Value = serde_yaml::from_slice(&std::fs::read(root.join("harness.yml"))?)?;
    let make = std::fs::read_to_string(root.join("Makefile"))?;
    let targets: BTreeSet<_> = regex::Regex::new(r"(?m)^([A-Za-z0-9_.-]+):")
        .unwrap()
        .captures_iter(&make)
        .map(|c| c[1].to_owned())
        .collect();
    let workflow: Value =
        serde_yaml::from_slice(&std::fs::read(root.join(".github/workflows/ci.yml"))?)?;
    let mut errors = vec![];
    let mut warnings = vec![];
    if let Err(error) = compose_identity::source_contract(root) {
        errors.push(format!("Compose image identity: {error}"));
    }
    let docs_truth = docs_truth::check(root)?;
    errors.extend(strings(&docs_truth["errors"]).into_iter().map(str::to_owned));
    warnings.extend(strings(&docs_truth["warnings"]).into_iter().map(str::to_owned));
    let docs = strings(&config["required_docs"]);
    if docs.is_empty() {
        errors.push("harness.yml has no required_docs block".into());
    }
    for doc in &docs {
        if !std::fs::metadata(root.join(doc)).is_ok_and(|m| m.is_file() && m.len() > 0) {
            errors.push(format!("required doc is missing or empty: {doc}"));
        }
    }
    let budgets = config["budgets"].as_object().cloned().unwrap_or_default();
    if budgets.is_empty() {
        errors.push("harness.yml has no budgets block".into());
    }
    for (path, limit) in &budgets {
        match std::fs::read_to_string(root.join(path)) {
            Ok(text) => {
                if limit
                    .as_u64()
                    .is_none_or(|limit| text.lines().count() as u64 > limit)
                {
                    errors.push(format!("{path} exceeds instruction line budget"));
                }
            }
            Err(_) => errors.push(format!("budgeted file missing: {path}")),
        }
    }
    let link_re = regex::Regex::new(r"\[[^\]]*\]\(([^)]+)\)").unwrap();
    let mut n_links = 0;
    for path in LINKS {
        match std::fs::read_to_string(root.join(path)) {
            Ok(text) => {
                for m in link_re.captures_iter(&text) {
                    let target = &m[1];
                    if ["http://", "https://", "#", "mailto:"]
                        .iter()
                        .any(|s| target.starts_with(s))
                    {
                        continue;
                    }
                    n_links += 1;
                    if !root
                        .join(path)
                        .parent()
                        .unwrap()
                        .join(target.split('#').next().unwrap())
                        .exists()
                    {
                        errors.push(format!("{path}: broken link -> {target}"));
                    }
                }
            }
            Err(_) => errors.push(format!("link-checked doc missing: {path}")),
        }
    }
    let roots = strings(&config["workspace"]["spec_roots"]);
    let specs = strings(&config["workspace"]["spec_globs"]);
    let forbidden = strings(&config["workspace"]["root_forbidden"]);
    if roots.is_empty() || specs.is_empty() || forbidden.is_empty() {
        errors.push("harness.yml workspace block is incomplete".into());
    }
    let files = walk(root, PRUNED, false)?;
    let config_root = config["workspace"]["playwright_config_root"]
        .as_str()
        .unwrap_or("web");
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy();
        if specs.iter().any(|p| pattern(p, &name)) && !roots.iter().any(|r| file.starts_with(r)) {
            errors.push(format!(
                "test spec outside allowed roots: {}",
                file.display()
            ));
        }
        if pattern("playwright*.config.ts", &name) && !file.starts_with(config_root) {
            errors.push(format!(
                "Playwright config outside {config_root}/: {}",
                file.display()
            ));
        }
        if file.components().count() == 1 && forbidden.iter().any(|p| pattern(p, &name)) {
            errors.push(format!("stray file at repository root: {name}"));
        }
    }
    let structural = errors.len();
    let mut commands = BTreeSet::new();
    all_make(&config, &mut commands);
    for target in &commands {
        if !targets.contains(target) {
            errors.push(format!("Make target declared but missing: {target}"));
        }
    }
    let gates = config["gates"].as_object().cloned().unwrap_or_default();
    if gates.is_empty() {
        errors.push("harness.yml has no gates block".into());
    }
    for (name, fields) in &gates {
        let where_ = format!("gate '{name}'");
        let Some(fields) = fields.as_object() else {
            errors.push(format!("{where_}: gate must be object"));
            continue;
        };
        let allowed = [
            "make",
            "shell",
            "purpose",
            "triggers",
            "tier",
            "required_on",
            "resource",
            "skip",
            "timeout",
            "evidence",
            "ci_job",
        ];
        for key in fields.keys() {
            if !allowed.contains(&key.as_str()) {
                errors.push(format!("{where_}: unknown field {key}"));
            }
        }
        let get = |key: &str| fields.get(key).unwrap_or(&Value::Null);
        let text = |key: &str| get(key).as_str().unwrap_or("");
        let entrypoints = ["make", "shell"]
            .iter()
            .filter(|k| fields.contains_key(**k))
            .count();
        if entrypoints != 1 {
            errors.push(format!("{where_}: needs exactly one make/shell"));
        }
        if fields.contains_key("make") && !targets.contains(text("make")) {
            errors.push(format!("{where_}: unknown Make target"));
        }
        if strings(get("triggers")).is_empty() {
            errors.push(format!("{where_}: triggers must be nonempty list"));
        }
        for (key, valid) in [
            ("tier", &["L0", "L1", "L2", "L3"][..]),
            (
                "resource",
                &[
                    "offline",
                    "hosted-service",
                    "live-stack",
                    "provider",
                    "docker",
                ][..],
            ),
            ("skip", &["never", "live-stack", "toolchain"][..]),
        ] {
            if !valid.contains(&text(key)) {
                errors.push(format!("{where_}: invalid {key}"));
            }
        }
        let required = strings(get("required_on"));
        if required.is_empty()
            || required
                .iter()
                .any(|s| !["change", "release", "manual"].contains(s))
        {
            errors.push(format!("{where_}: invalid required_on"));
        }
        if !get("timeout").as_u64().is_some_and(|v| v > 0) {
            errors.push(format!("{where_}: timeout must be positive integer"));
        }
        if text("evidence").is_empty() {
            errors.push(format!("{where_}: evidence must be nonempty"));
        }
        let job = text("ci_job");
        if job == "manual" {
            if required.contains(&"change") {
                errors.push(format!("{where_}: change gate cannot use manual CI"));
            }
        } else if let Some(job_body) = workflow["jobs"].get(job) {
            let run_text = job_body["steps"]
                .as_array()
                .map(|steps| {
                    steps
                        .iter()
                        .filter_map(|s| s["run"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let executes = if fields.contains_key("make") {
                runs_make(&run_text, text("make"))
            } else {
                let commands: Vec<_> = text("shell")
                    .split("&&")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                !commands.is_empty() && commands.iter().all(|c| runs_shell(&run_text, c))
            };
            if entrypoints == 1 && !executes {
                errors.push(format!(
                    "{where_}: CI job {job} does not execute preserving entrypoint outcome"
                ));
            }
        } else {
            errors.push(format!("{where_}: missing/unknown CI job {job}"));
        }
    }
    let program_root = root.join("deploy/runbooks");
    let mut programs = 0;
    if program_root.is_dir() {
        for entry in std::fs::read_dir(program_root)? {
            let directory = entry?.path();
            let state = directory.join("loop-state.json");
            if !state.is_file() {
                continue;
            }
            programs += 1;
            match object(&state) {
                Ok(v) => {
                    let schema = v["schema_version"].as_str().unwrap_or("");
                    if let Some(required) =
                        config["programs"]["required_files_by_schema"].get(schema)
                    {
                        for name in strings(required) {
                            if !directory.join(name).is_file() {
                                warnings.push(format!(
                                    "program {} missing {name}",
                                    directory.display()
                                ));
                            }
                        }
                    } else {
                        warnings.push(format!(
                            "program {} has unknown schema",
                            directory.display()
                        ));
                    }
                }
                Err(_) => errors.push(format!(
                    "program {} loop-state unreadable",
                    directory.display()
                )),
            }
        }
    }
    let receipt = json!({"gate":"harness-check","tier":"L0","result":if errors.is_empty(){"pass"}else{"fail"},"structural":{"required_docs":docs.len(),"budgeted_files":budgets.len(),"links_resolved":n_links,"workspace_files_scanned":files.len(),"errors":structural},"semantic":{"make_targets_referenced":commands.len(),"gate_schemas_validated":gates.len(),"programs_inspected":programs,"errors":errors.len()-structural},"errors":errors,"warnings":warnings});
    let _ = write_json(&root.join("tmp/gate-evidence/harness-check.json"), &receipt);
    Ok(receipt)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outcome_preserving_ci_only() {
        assert!(runs_make("make lint", "lint"));
        assert!(runs_make("X=1 make lint | tee log", "lint"));
        for s in [
            "# make lint",
            "echo make lint",
            "make lint || true",
            "make lint -n",
            "MAKEFLAGS=-n make lint",
            "make lint &",
            "set +e\nmake lint",
            "make lint; true",
        ] {
            assert!(!runs_make(s, "lint"), "{s}");
        }
    }
}
