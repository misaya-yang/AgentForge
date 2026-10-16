//! Offline compatibility gate. Artifact equality never claims live verification.
use crate::{gate, profiles};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<(Value, Vec<u8>), String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok((value, bytes))
}
fn same_file(a: &Path, b: &Path) -> bool {
    if a.canonicalize().ok() == b.canonicalize().ok() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (fs::metadata(a), fs::metadata(b)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    false
}
pub fn run_gate(args: &[String]) -> u8 {
    let mut baseline = PathBuf::from("sdk/openapi.json");
    let mut evidence_path = PathBuf::from("tmp/gate-evidence/verify-openapi-contract.json");
    let mut ownership_path = PathBuf::from("sdk/openapi-profiles.json");
    let mut root = PathBuf::from(".");
    let mut profile = "published".to_owned();
    let mut candidate = None;
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        i += 1;
        let Some(value) = args.get(i) else {
            eprintln!("missing value for {key}");
            return 2;
        };
        i += 1;
        match key.as_str() {
            "--baseline" => baseline = value.into(),
            "--candidate" => candidate = Some(PathBuf::from(value)),
            "--evidence-out" => evidence_path = value.into(),
            "--ownership" => ownership_path = value.into(),
            "--root" => root = value.into(),
            "--profile" => profile = value.clone(),
            _ => {
                eprintln!("unknown gate argument: {key}");
                return 2;
            }
        }
    }
    let (evidence, code) = evaluate(
        &baseline,
        candidate.as_deref(),
        &ownership_path,
        &root,
        &profile,
    );
    let serialized = serde_json::to_string_pretty(&evidence).expect("JSON evidence");
    if let Some(parent) = evidence_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!("evidence directory: {e}");
                return 2;
            }
        }
    }
    if let Err(e) = fs::write(evidence_path, format!("{serialized}\n")) {
        eprintln!("evidence write: {e}");
        return 2;
    }
    println!("{serialized}");
    code
}
pub fn evaluate(
    baseline_path: &Path,
    candidate: Option<&Path>,
    ownership_path: &Path,
    root: &Path,
    profile: &str,
) -> (Value, u8) {
    let mut evidence = json!({"gate":"verify-openapi-contract","profile":profile,"baseline":baseline_path.to_string_lossy(),"result":"error","native_runtime_verified":false,"result_kind":if profile=="published"{"artifact-comparison-only"}else{"required-profile-verification"}});
    let result = (|| -> Result<u8, String> {
        if !profiles::PROFILES.contains(&profile) {
            return Err(format!("unknown OpenAPI profile: {profile}"));
        }
        let (published, bytes) = read(baseline_path)?;
        let ownership = profiles::load_ownership(&published, ownership_path, root)?;
        evidence["published_sha256"] = json!(digest(&bytes));
        evidence["published_stats"] = gate::stats(&published);
        let baseline = profiles::project(&published, profile, &ownership)?;
        if profile == "python-hybrid" {
            return Err("legacy application export is unavailable in the native-only tree; an artifact cannot establish actual application export verification".into());
        }
        let candidate=candidate.ok_or("native/published contract requires --candidate from an independent native/full-profile export; native verification is unverified")?;
        if same_file(candidate, baseline_path) {
            return Err("candidate cannot be the published baseline itself".into());
        }
        let (current, bytes) = read(candidate)?;
        if !current["paths"].as_object().is_some_and(|p| !p.is_empty()) {
            return Err("candidate has no OpenAPI paths".into());
        }
        evidence["tier"] = json!("L1-offline-candidate-comparison");
        evidence["candidate"] = json!(candidate.to_string_lossy());
        evidence["candidate_sha256"] = json!(digest(&bytes));
        evidence["determinism_hash_seeds"] = json!([]);
        let violations = gate::compare_specs(&baseline, &current);
        let failed = !violations.is_empty();
        evidence["baseline_stats"] = gate::stats(&baseline);
        evidence["current_stats"] = gate::stats(&current);
        evidence["operation_ids_checked"] = json!(gate::operation_ids(&current).len());
        evidence["violations"] = json!(violations);
        evidence["comparison_result"] = json!(if failed { "fail" } else { "pass" });
        // Forwarding metadata requires a complete independent owner export and proof.
        // Never treat declaration-only descriptors as runtime or full-profile evidence.
        if profile == "native" && !failed {
            return Err("artifact comparison passed, but required native verification is unavailable: no supported complete independent native producer/export evidence".into());
        }
        evidence["result"] = json!(if failed { "fail" } else { "pass" });
        Ok(if failed { 1 } else { 0 })
    })();
    match result {
        Ok(code) => (evidence, code),
        Err(error) => {
            evidence["error"] = json!(error);
            (evidence, 2)
        }
    }
}
