//! Offline, non-mutating KB release evidence admission, ported from the T0 gate.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
fn digest(p: &Path) -> Option<String> {
    fs::read(p).ok().map(|b| format!("{:x}", Sha256::digest(b)))
}
fn sha(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
fn nonempty(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}
fn load(p: &Path) -> Result<Value, String> {
    let v: Value = serde_json::from_slice(&fs::read(p).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if v.is_object() {
        Ok(v)
    } else {
        Err("top-level JSON value must be an object".into())
    }
}
fn reason(r: &mut Vec<Value>, code: &str, message: impl Into<String>) {
    r.push(json!({"code":code,"message":message.into()}));
}
fn review(v: &Value, label: &str) -> Vec<String> {
    if !v.is_object() {
        return vec![format!("{label} review must be an object")];
    }
    let mut r = vec![];
    if v["status"] != "approved" {
        r.push(format!("{label} review.status must be 'approved'"));
    }
    for f in ["reviewer", "reviewed_at"] {
        if !nonempty(&v[f]) {
            r.push(format!("{label} review.{f} must be non-empty"));
        }
    }
    r
}
fn resolved(p: &Path) -> PathBuf {
    if let Ok(p) = p.canonicalize() {
        return p;
    }
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => {
                out.push(c.as_os_str());
                if let Ok(actual) = out.canonicalize() {
                    out = actual;
                }
            }
        }
    }
    out
}
fn repo_path(root: &Path, v: &Value, label: &str) -> Result<PathBuf, String> {
    let s = v
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{label} must be a non-empty repository-relative path"))?;
    if Path::new(s).is_absolute() {
        return Err(format!("{label} must be repository-relative"));
    }
    let p = resolved(&root.join(s));
    if !p.starts_with(resolved(root)) {
        return Err(format!("{label} escapes the repository root"));
    }
    Ok(p)
}
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
fn string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        _ => v.to_string(),
    }
}
fn short(v: &[String]) -> String {
    format!(
        "{}{}",
        v.iter().take(10).cloned().collect::<Vec<_>>().join(", "),
        if v.len() > 10 {
            format!(" (+{} more)", v.len() - 10)
        } else {
            String::new()
        }
    )
}
pub fn evaluate(
    root: &Path,
    manifest_path: &Path,
    cases_path: &Path,
    pointer_path: &Path,
) -> Value {
    let mut r = vec![];
    let mut manifest = Value::Null;
    let mut md = None;
    if !manifest_path.is_file() {
        reason(
            &mut r,
            "manifest_missing",
            format!("manifest not found: {}", manifest_path.display()),
        );
    } else {
        match load(manifest_path) {
            Err(e) => reason(
                &mut r,
                "manifest_invalid",
                format!("cannot read manifest: {e}"),
            ),
            Ok(m) => {
                manifest = m;
                md = digest(manifest_path);
                if !nonempty(&manifest["version"]) {
                    reason(
                        &mut r,
                        "manifest_version_missing",
                        "manifest.version must be non-empty",
                    );
                }
                match manifest["files"].as_object().filter(|o| !o.is_empty()) {
                    None => reason(
                        &mut r,
                        "manifest_files_missing",
                        "manifest.files must be non-empty",
                    ),
                    Some(files) => {
                        let mut found = false;
                        let mut entries = files.iter().collect::<Vec<_>>();
                        entries.sort_by_key(|(k, _)| *k);
                        for (name, spec) in entries {
                            if !spec.is_object() {
                                reason(
                                    &mut r,
                                    "manifest_entry_invalid",
                                    format!("invalid manifest entry '{name}'"),
                                );
                                continue;
                            }
                            if !sha(&spec["sha256"]) {
                                reason(
                                    &mut r,
                                    "manifest_digest_invalid",
                                    format!(
                                        "manifest entry '{name}' has no lowercase SHA-256 digest"
                                    ),
                                );
                                continue;
                            }
                            let target =
                                resolved(&manifest_path.parent().unwrap_or(root).join(name));
                            if !target.starts_with(resolved(root)) {
                                reason(
                                    &mut r,
                                    "manifest_path_escape",
                                    format!("manifest entry '{name}' escapes the repository root"),
                                );
                                continue;
                            }
                            if target == resolved(cases_path) {
                                found = true
                            }
                            if !target.is_file() {
                                reason(
                                    &mut r,
                                    "manifest_file_missing",
                                    format!("manifest entry '{name}' is missing"),
                                );
                                continue;
                            }
                            if digest(&target).as_deref() != spec["sha256"].as_str() {
                                reason(
                                    &mut r,
                                    "manifest_hash_mismatch",
                                    format!("manifest hash mismatch for '{name}'"),
                                );
                            }
                        }
                        if !found {
                            reason(
                                &mut r,
                                "cases_not_manifested",
                                format!(
                                    "golden cases file is not listed in {}",
                                    manifest_path.display()
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
    let mut cases = vec![];
    let mut counts = json!({});
    let parsed = if !cases_path.is_file() {
        reason(
            &mut r,
            "cases_missing",
            format!("golden cases not found: {}", cases_path.display()),
        );
        false
    } else {
        match fs::read_to_string(cases_path)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                s.lines()
                    .enumerate()
                    .filter(|(_, l)| !l.trim().is_empty())
                    .map(|(i, l)| {
                        let v: Value = serde_json::from_str(l).map_err(|e| e.to_string())?;
                        if v.is_object() {
                            Ok(v)
                        } else {
                            Err(format!("line {} must be an object", i + 1))
                        }
                    })
                    .collect::<Result<Vec<_>, String>>()
            }) {
            Ok(v) => {
                cases = v;
                true
            }
            Err(e) => {
                reason(
                    &mut r,
                    "cases_invalid",
                    format!("cannot read golden cases: {e}"),
                );
                false
            }
        }
    };
    if parsed {
        let n = cases.len();
        if !(200..=400).contains(&n) {
            reason(
                &mut r,
                "case_count_out_of_range",
                format!("case_count={n}; release requires 200..400"),
            );
        }
        let missing = cases
            .iter()
            .enumerate()
            .filter(|(_, v)| !truth(&v["case_id"]))
            .map(|(i, _)| (i + 1).to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            reason(
                &mut r,
                "case_id_missing",
                format!(
                    "rows without case_id: {}",
                    missing
                        .iter()
                        .take(10)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        let mut ids = BTreeMap::new();
        for c in &cases {
            if truth(&c["case_id"]) {
                *ids.entry(string(&c["case_id"])).or_insert(0) += 1;
            }
        }
        let dup = ids
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(s, _)| s)
            .take(10)
            .collect::<Vec<_>>();
        if !dup.is_empty() {
            reason(
                &mut r,
                "duplicate_case_ids",
                format!("duplicate case_ids: {}", dup.join(", ")),
            );
        }
        let (mut real, mut synthetic) = (0, 0);
        let (mut provenance, mut reviews, mut versions) = (vec![], vec![], vec![]);
        for (i, c) in cases.iter().enumerate() {
            let id = if truth(&c["case_id"]) {
                string(&c["case_id"])
            } else {
                format!("row-{}", i + 1)
            };
            let m = &c["metadata"];
            if !m.is_object() {
                provenance.push(id.clone());
                reviews.push(id.clone());
                versions.push(id);
                continue;
            }
            let p = &m["provenance"];
            if !nonempty(&p["source_ref"])
                || !matches!(p["kind"].as_str(), Some("real" | "synthetic"))
            {
                provenance.push(id.clone());
            } else if p["kind"] == "real" {
                real += 1
            } else {
                synthetic += 1
            }
            if !review(&m["review"], &id).is_empty() {
                reviews.push(id.clone());
            }
            if truth(&manifest["version"]) && m["version"] != manifest["version"] {
                versions.push(id);
            }
        }
        for(code,prefix,items)in [("case_provenance_incomplete","every case requires metadata.provenance.kind=real|synthetic and a non-empty source_ref; invalid: ".to_string(),provenance),("case_review_incomplete","every case requires approved metadata.review with reviewer/reviewed_at; invalid: ".to_string(),reviews),("case_version_mismatch",format!("case metadata.version must match manifest.version='{}'; invalid: ",manifest["version"].as_str().unwrap_or("")),versions)]{if !items.is_empty(){reason(&mut r,code,format!("{prefix}{}",short(&items)));}}
        counts = json!({"real":real,"synthetic":synthetic});
        if real + synthetic != n {
            reason(
                &mut r,
                "source_mix_unverifiable",
                format!(
                    "only {}/{n} cases have valid provenance source kinds",
                    real + synthetic
                ),
            );
        } else if n > 0 && !(0.4..=0.6).contains(&(real as f64 / n as f64)) {
            reason(
                &mut r,
                "source_mix_out_of_range",
                format!(
                    "real={real}, synthetic={synthetic}; real share {:.1}% must be 40%..60%",
                    real as f64 / n as f64 * 100.
                ),
            );
        }
    }
    let dataset = pointer(
        root,
        pointer_path,
        manifest_path,
        &manifest,
        md.as_deref(),
        cases_path,
        &cases,
        &mut r,
    );
    json!({"status":if r.is_empty(){"PASS"}else{"BLOCKED"},"case_count":cases.len(),"source_counts":counts,"manifest":manifest_path,"manifest_sha256":md,"release_pointer":pointer_path,"dataset_id":dataset,"reasons":r})
}
fn pointer(
    root: &Path,
    path: &Path,
    mp: &Path,
    m: &Value,
    md: Option<&str>,
    cp: &Path,
    cases: &[Value],
    r: &mut Vec<Value>,
) -> Value {
    if !path.is_file() {
        reason(
            r,
            "release_pointer_missing",
            format!("release pointer not found: {}", path.display()),
        );
        return Value::Null;
    }
    let p = match load(path) {
        Ok(p) => p,
        Err(e) => {
            reason(
                r,
                "release_pointer_invalid",
                format!("cannot read release pointer: {e}"),
            );
            return Value::Null;
        }
    };
    if p["schema_version"] != "kb-release-evidence/v1" {
        reason(
            r,
            "release_pointer_schema",
            "schema_version must be 'kb-release-evidence/v1'",
        );
    }
    if !nonempty(&p["release_key"]) {
        reason(
            r,
            "release_pointer_key",
            "release pointer release_key must be non-empty",
        );
    }
    for e in review(&p["review"], "release pointer") {
        reason(r, "release_pointer_review", e);
    }
    let dataset = if nonempty(&p["dataset_id"]) {
        p["dataset_id"].clone()
    } else {
        reason(
            r,
            "release_pointer_dataset",
            "release pointer dataset_id must be non-empty",
        );
        Value::Null
    };
    if truth(m) && p["golden_version"] != m["version"] {
        reason(
            r,
            "release_pointer_version",
            "release pointer golden_version mismatches manifest",
        );
    }
    if md.is_some() && p["golden_manifest_sha256"].as_str() != md {
        reason(
            r,
            "release_pointer_manifest_hash",
            "release pointer manifest SHA-256 mismatches",
        );
    }
    match repo_path(root, &p["golden_manifest"], "golden_manifest") {
        Ok(v) if v != resolved(mp) => reason(
            r,
            "release_pointer_manifest",
            "release pointer cites a different manifest",
        ),
        Err(e) => reason(r, "release_pointer_manifest", e),
        _ => {}
    }
    let bp = match repo_path(root, &p["baseline_report"], "baseline_report") {
        Ok(v) => v,
        Err(e) => {
            reason(r, "baseline_report_path", e);
            return dataset;
        }
    };
    if !bp.is_file() {
        reason(
            r,
            "baseline_report_missing",
            format!("baseline report not found: {}", bp.display()),
        );
        return dataset;
    }
    if p["baseline_report_sha256"].as_str() != digest(&bp).as_deref() {
        reason(
            r,
            "baseline_report_hash",
            "release pointer baseline report SHA-256 mismatches",
        );
    }
    let b = match load(&bp) {
        Ok(v) => v,
        Err(e) => {
            reason(
                r,
                "baseline_report_invalid",
                format!("cannot read baseline report: {e}"),
            );
            return dataset;
        }
    };
    let e = &b["release_evidence"];
    if !e.is_object() {
        reason(
            r,
            "baseline_release_evidence_missing",
            "baseline requires a release_evidence object",
        );
        return dataset;
    }
    if e["schema_version"] != "kb-baseline-evidence/v1" {
        reason(
            r,
            "baseline_schema",
            "baseline schema_version must be 'kb-baseline-evidence/v1'",
        );
    }
    for err in review(&e["review"], "baseline") {
        reason(r, "baseline_review", err);
    }
    if e["dataset_id"] != dataset {
        reason(
            r,
            "baseline_dataset_binding",
            "baseline dataset_id mismatches release pointer",
        );
    }
    if truth(m) && e["golden_version"] != m["version"] {
        reason(
            r,
            "baseline_version",
            "baseline golden_version mismatches manifest",
        );
    }
    if md.is_some() && e["golden_manifest_sha256"].as_str() != md {
        reason(
            r,
            "baseline_manifest_hash",
            "baseline manifest SHA-256 mismatches",
        );
    }
    let cd = digest(cp);
    if cd.is_none() || b["provenance"]["expectations"]["sha256"].as_str() != cd.as_deref() {
        reason(
            r,
            "baseline_expectations_hash",
            "baseline expectations hash mismatches cases",
        );
    }
    if !sha(&b["provenance"]["observations"]["sha256"]) {
        reason(
            r,
            "baseline_observations_hash",
            "baseline observations SHA-256 is missing",
        );
    }
    distribution(
        &b,
        cases
            .iter()
            .filter(|c| c["track"] == "retrieval_only")
            .count(),
        r,
    );
    dataset
}
pub fn distribution(b: &Value, expected: usize, r: &mut Vec<Value>) {
    let Some(metrics) = b["retrieval"]["all"]["metrics"]
        .as_object()
        .filter(|m| !m.is_empty())
    else {
        reason(
            r,
            "baseline_distribution_missing",
            "baseline must contain retrieval.all.metrics distributions",
        );
        return;
    };
    let mut counts = vec![];
    for (k, v) in metrics {
        if !v.is_object() {
            reason(
                r,
                "baseline_metrics_invalid",
                format!("retrieval metrics at k={k} is not an object"),
            );
            continue;
        }
        let names = ["hit_rate", "mrr", "ndcg_at_k", "recall_at_k"];
        let missing = names
            .iter()
            .filter(|n| v.get(**n).is_none())
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            reason(
                r,
                "baseline_metrics_incomplete",
                format!("retrieval metrics at k={k} missing {}", missing.join(", ")),
            );
        }
        let invalid = names
            .iter()
            .filter(|n| {
                v.get(**n).is_some()
                    && !v[**n]
                        .as_f64()
                        .is_some_and(|f| f.is_finite() && (0.0..=1.0).contains(&f))
            })
            .copied()
            .collect::<Vec<_>>();
        if !invalid.is_empty() {
            reason(
                r,
                "baseline_metrics_invalid",
                format!(
                    "retrieval metrics at k={k} must be finite numbers in [0, 1]: {}",
                    invalid.join(", ")
                ),
            );
        }
        if let Some(n) = v["num_queries"].as_i64() {
            counts.push(n);
        }
    }
    counts.sort();
    counts.dedup();
    if counts.last().copied() != Some(expected as i64) {
        reason(
            r,
            "baseline_case_binding_mismatch",
            format!(
                "baseline num_queries must equal retrieval case count {expected}; observed {}",
                if counts.is_empty() {
                    "none".into()
                } else {
                    format!("{counts:?}")
                }
            ),
        );
    }
}
pub fn run(args: &[String]) -> u8 {
    let root = std::env::current_dir().unwrap_or_default();
    let (mut root, mut manifest, mut cases, mut pointer) = (
        root.clone(),
        root.join("tests/fixtures/eval/rag/golden/manifest.json"),
        root.join("tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl"),
        root.join("reports/kb-eval-baseline/release-pointer.json"),
    );
    let mut i = 0;
    while i < args.len() {
        if i + 1 >= args.len() {
            eprintln!("missing value for {}", args[i]);
            return 2;
        }
        let p = PathBuf::from(&args[i + 1]);
        match args[i].as_str() {
            "--repo-root" => root = p,
            "--manifest" => manifest = p,
            "--cases" => cases = p,
            "--release-pointer" => pointer = p,
            _ => {
                eprintln!("unknown argument: {}", args[i]);
                return 2;
            }
        }
        i += 2;
    }
    let report = evaluate(
        &resolved(&root),
        &resolved(&manifest),
        &resolved(&cases),
        &resolved(&pointer),
    );
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if report["status"] == "PASS" { 0 } else { 1 }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_reject_boolean_missing_and_out_of_range() {
        let mut r = vec![];
        distribution(
            &json!({"retrieval":{"all":{"metrics":{"5":{"hit_rate":true,"mrr":1.1,"ndcg_at_k":0.5,"num_queries":false}}}}}),
            4,
            &mut r,
        );
        assert_eq!(
            r.iter()
                .map(|v| v["code"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "baseline_metrics_incomplete",
                "baseline_metrics_invalid",
                "baseline_case_binding_mismatch"
            ]
        );
    }
    #[test]
    fn traversal_and_review_are_fail_closed() {
        assert!(
            repo_path(
                Path::new("/tmp"),
                &json!("../etc/passwd"),
                "baseline_report"
            )
            .is_err()
        );
        assert_eq!(review(&json!({"status":"pending"}), "baseline").len(), 3);
        assert!(!sha(&json!("A".repeat(64))));
    }
}
