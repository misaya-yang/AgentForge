//! Immutable native/TypeScript LOC ledger. Backend language changes require a new ledger.
use crate::*;
use std::{collections::BTreeSet, process::Command};
const PRUNE: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".playwright",
    ".claude",
    ".worktrees",
    "tmp",
    "temp",
    "logs",
    "uploads",
    "test-results",
    "playwright-report",
    "htmlcov",
    "target",
];
fn lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.split_inclusive([
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
            '\u{2028}', '\u{2029}',
        ])
        .count()
            - text.as_bytes().windows(2).filter(|w| *w == b"\r\n").count()
    }
}
fn evaluate(
    ledger: &BTreeMap<String, u64>,
    current: &BTreeMap<String, u64>,
    threshold: u64,
    kind: &str,
) -> (Vec<Value>, Vec<Value>) {
    let mut violations = vec![];
    let mut notes = vec![];
    for (file, base) in ledger {
        match current.get(file){None=>notes.push(json!({"file":file,"kind":kind,"note":format!("deleted since baseline (was {base} lines)")})),Some(now)if now>base=>violations.push(json!({"file":file,"kind":kind,"issue":"oversized file grew","baseline_lines":base,"current_lines":now})),Some(now)if now<base=>notes.push(json!({"file":file,"kind":kind,"note":format!("shrank {base} -> {now}")})),_=>{}}
    }
    for (file, count) in current {
        if !ledger.contains_key(file) && *count > threshold {
            violations.push(json!({"file":file,"kind":kind,"issue":"new file over threshold","baseline_lines":null,"current_lines":count,"threshold":threshold}));
        }
    }
    (violations, notes)
}
pub fn run(args: &Args) -> Result<Value> {
    if args.flag("--selftest") {
        let (v, n) = evaluate(
            &BTreeMap::from([("grown".into(), 12), ("gone".into(), 13)]),
            &BTreeMap::from([("grown".into(), 13), ("new".into(), 11)]),
            10,
            "rust",
        );
        require(v.len() == 2 && n.len() == 1, "LOC selftest failed")?;
        return Ok(json!({"result":"pass","selftest":"loc-no-growth"}));
    }
    let root = args
        .values
        .get("--root")
        .map(PathBuf::from)
        .unwrap_or_else(|| args.root());
    let baseline_path = args
        .values
        .get("--baseline")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join("docs/architecture/baselines/native-rust-v2/loc-baseline.json")
        });
    let baseline = object(&baseline_path)?;
    let mut value = json!({"gate":"loc-no-growth","tier":"L0","baseline":baseline_path,"base_git_sha":baseline["base_git_sha"]});
    let result = (|| -> Result<()> {
        require(
            matches!(
                baseline["schema"].as_str(),
                Some("ai-gateway/baseline/loc-baseline/v1" | "ai-gateway/baseline/loc-baseline/v2")
            ),
            "baseline schema invalid",
        )?;
        require(
            baseline["thresholds"]["rust_new_file_max"] == 800
                && baseline["thresholds"]["typescript_new_file_max"] == 500,
            "native ledger must explicitly pin Rust800 and TypeScript500",
        )?;
        let sha = baseline["base_git_sha"]
            .as_str()
            .ok_or("base SHA missing")?;
        require(
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "invalid base SHA",
        )?;
        let git = |params: &[&str]| -> Result<String> {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(params)
                .output()?;
            require(
                out.status.success(),
                "immutable baseline Git identity unavailable",
            )?;
            Ok(String::from_utf8(out.stdout)?)
        };
        require(
            git(&["rev-parse", "--verify", &format!("{sha}^{{commit}}")])?.trim() == sha,
            "base commit mismatch",
        )?;
        git(&["merge-base", "--is-ancestor", sha, "HEAD"])?;
        let mut seen = BTreeSet::new();
        let mut ledgers = BTreeMap::new();
        for (key, suffixes, threshold) in [
            ("oversized_rust", vec!["rs"], 800),
            ("oversized_typescript", vec!["ts", "tsx"], 500),
        ] {
            let mut ledger = BTreeMap::new();
            for row in baseline[key]["files"]
                .as_array()
                .ok_or("native LOC ledger missing")?
            {
                let file = row["file"].as_str().ok_or("ledger file missing")?;
                safe(&root, file)?;
                require(
                    !file.chars().any(|c| c.is_control())
                        && seen.insert(file.to_owned())
                        && Path::new(file)
                            .extension()
                            .is_some_and(|s| suffixes.iter().any(|x| s == *x)),
                    "invalid/duplicate LOC path",
                )?;
                let count = row["lines"]
                    .as_u64()
                    .filter(|n| *n > threshold)
                    .ok_or("ledger must record genuinely oversized file")?;
                require(
                    lines(&git(&["show", &format!("{sha}:{file}")])?) as u64 == count,
                    "ledger baseline count differs from immutable Git blob",
                )?;
                ledger.insert(file.to_owned(), count);
            }
            ledgers.insert(key, ledger);
        }
        let mut rust = BTreeMap::new();
        let mut ts = BTreeMap::new();
        for directory in [
            "rust", "src", "apps", "packages", "scripts", "database", "web", "sdk", "tests",
        ] {
            let path = root.join(directory);
            if !path.exists() {
                continue;
            }
            require(
                path.is_dir() && !path.is_symlink(),
                "scan root must be real directory",
            )?;
            for relative in walk(&path, PRUNE, true)? {
                let file = path.join(&relative);
                require(!file.is_symlink(), "symlink inside LOC scan")?;
                let target = match file.extension().and_then(|s| s.to_str()) {
                    Some("rs") => &mut rust,
                    Some("ts" | "tsx") => &mut ts,
                    _ => continue,
                };
                target.insert(
                    file.strip_prefix(&root)?.to_string_lossy().into_owned(),
                    lines(&std::fs::read_to_string(file)?) as u64,
                );
            }
        }
        require(
            !rust.is_empty() && !ts.is_empty(),
            "native and TypeScript scans must both be nonempty",
        )?;
        let (mut violations, mut notes) = evaluate(&ledgers["oversized_rust"], &rust, 800, "rust");
        let (v, n) = evaluate(&ledgers["oversized_typescript"], &ts, 500, "typescript");
        violations.extend(v);
        notes.extend(n);
        violations.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        notes.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        value["provenance"] =
            json!({"result":"pass","base_git_sha":sha,"constrained_files_checked":seen.len()});
        value["scanned"] = json!({"rust_files":rust.len(),"typescript_files":ts.len()});
        value["result"] = json!(if violations.is_empty() {
            "pass"
        } else {
            "fail"
        });
        value["violations"] = json!(violations);
        value["informational"] = json!(notes);
        Ok(())
    })();
    if let Err(e) = result {
        value["result"] = json!("error");
        value["provenance"] = json!({"result":"fail","error":e.to_string()});
    }
    let output = args
        .values
        .get("--evidence-out")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("tmp/gate-evidence/loc-no-growth.json"));
    write_json(&output, &value)?;
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_splitlines_equivalence() {
        for (text, n) in [
            ("", 0),
            ("a", 1),
            ("a\n", 1),
            ("a\r\nb\r\n", 2),
            ("\n\n", 2),
            ("a\u{2028}b", 2),
        ] {
            assert_eq!(lines(text), n);
        }
    }
    #[test]
    fn growth_new_oversize_delete() {
        let (v, n) = evaluate(
            &BTreeMap::from([("old".into(), 900)]),
            &BTreeMap::from([("new".into(), 801)]),
            800,
            "rust",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(n.len(), 1);
    }
}
