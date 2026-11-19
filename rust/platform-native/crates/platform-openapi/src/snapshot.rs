//! Independent native subprocess snapshot with bounded, explicit coverage evidence.
use crate::{gate, profiles};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return path.canonicalize().map_err(|e| e.to_string());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().ok_or("output has no filename")?;
    Ok(resolve(parent)?.join(name))
}
pub fn safe_output(path: &Path, root: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err("output must not be a symlink or hardlinked file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() != 1 {
                return Err("output must not be a symlink or hardlinked file".into());
            }
        }
    }
    if resolve(path)?.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
        return Err("native export output must be outside the repository".into());
    }
    Ok(())
}
fn produce(producer: &Path) -> Result<Vec<u8>, String> {
    let mut child = Command::new(producer)
        .arg("--native-only")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("producer stdout missing")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = vec![];
        stdout
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if started.elapsed() > Duration::from_secs(30) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("native producer timeout".into());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "producer reader failed")?
        .map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("native producer output is oversized".into());
    }
    if !status.success() {
        return Err("native producer failed".into());
    }
    Ok(bytes)
}
pub fn run(args: &[String]) -> u8 {
    match execute(args) {
        Ok(message) => {
            println!("{message}");
            0
        }
        Err(error) => {
            eprintln!("Native export failed: {error}");
            2
        }
    }
}
fn execute(args: &[String]) -> Result<String, String> {
    let mut producer = None;
    let mut output = None;
    let mut coverage = None;
    let mut previous = None;
    let mut root = PathBuf::from(".");
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        i += 1;
        let value = args
            .get(i)
            .ok_or_else(|| format!("missing value for {key}"))?;
        i += 1;
        match key.as_str() {
            "--producer" => producer = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--coverage-output" => coverage = Some(PathBuf::from(value)),
            "--previous" => previous = Some(PathBuf::from(value)),
            "--root" => root = value.into(),
            _ => return Err(format!("unknown snapshot argument: {key}")),
        }
    }
    let producer = producer
        .ok_or("--producer required")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let output = output.ok_or("--output required")?;
    let coverage = coverage.ok_or("--coverage-output required")?;
    safe_output(&output, &root)?;
    safe_output(&coverage, &root)?;
    if resolve(&output)? == resolve(&coverage)? {
        return Err("schema and coverage outputs must be distinct".into());
    }
    for path in [&output, &coverage] {
        if resolve(path)? == producer {
            return Err("output aliases producer executable".into());
        }
    }
    let first = produce(&producer)?;
    if first != produce(&producer)? {
        return Err("native producer is not byte-deterministic across processes".into());
    }
    let document: Value = serde_json::from_slice(&first).map_err(|e| e.to_string())?;
    let marker = &document["x-native-contract"];
    if marker["scope"] != "native-only"
        || marker["complete"] != false
        || marker["bounded_success_responses_declared"] != true
    {
        return Err("producer must declare bounded incomplete native-only scope".into());
    }
    let foundation = root.join("rust/platform-native/crates/platform-gateway-foundation/src");
    for (field, file) in [
        ("source_sha256", "native_contract.rs"),
        ("response_source_sha256", "native_responses.rs"),
    ] {
        if marker[field] != hash(&fs::read(foundation.join(file)).map_err(|e| e.to_string())?) {
            return Err(format!("stale source fingerprint: {file}"));
        }
    }
    let native = root
        .join("rust/platform-native")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    for (name, digest) in marker["common_source_sha256"]
        .as_object()
        .ok_or("missing common source fingerprints")?
    {
        let file = foundation
            .join(name)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !file.starts_with(&native)
            || digest != &json!(hash(&fs::read(file).map_err(|e| e.to_string())?))
        {
            return Err(format!("stale common source fingerprint: {name}"));
        }
    }
    if let Some(proofs) = document["x-native-public-diagnostics"]["source_sha256"].as_object() {
        let gateway = root.join("rust/platform-native/crates/platform-gateway/src");
        for (name, digest) in proofs {
            let file = gateway
                .join(name)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !file.starts_with(&native)
                || digest != &json!(hash(&fs::read(file).map_err(|e| e.to_string())?))
            {
                return Err(format!("stale public handler fingerprint: {name}"));
            }
        }
    }
    if let Some(proofs) = document["x-native-knowledge-owner"]["source_sha256"].as_object() {
        let owner = root.join("rust/platform-native/crates/platform-knowledge-service/src");
        for (name, digest) in proofs {
            let file = owner.join(name).canonicalize().map_err(|e| e.to_string())?;
            if !file.starts_with(&native)
                || digest.as_str()
                    != Some(
                        format!(
                            "sha256:{}",
                            hash(&fs::read(file).map_err(|e| e.to_string())?)
                        )
                        .as_str(),
                    )
            {
                return Err(format!("stale Knowledge handler fingerprint: {name}"));
            }
        }
    }
    if let Some(proofs) = document["x-native-route-registry"]["source_sha256"].as_object() {
        for (name, digest) in proofs {
            let file = native
                .join("crates")
                .join(name)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !file.starts_with(&native)
                || digest != &json!(hash(&fs::read(file).map_err(|e| e.to_string())?))
            {
                return Err(format!("stale native route registry fingerprint: {name}"));
            }
        }
    }
    if let Some(previous) = previous {
        let prior: Value = serde_json::from_slice(&fs::read(previous).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let drift = profiles::declaration_drift(&prior, &document)?;
        if !drift.is_empty() {
            return Err(format!(
                "native declaration drift requires review: {}",
                drift.join(", ")
            ));
        }
    }
    // Publication is read only after independent production; never copied into output.
    let published: Value = serde_json::from_slice(
        &fs::read(root.join("sdk/openapi.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let actual = gate::operation_ids(&document);
    let published = gate::operation_ids(&published);
    let missing: Vec<_> = published
        .keys()
        .filter(|k| !actual.contains_key(*k))
        .cloned()
        .collect();
    let candidate = format!(
        "{}\n",
        serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
    );
    let report = json!({"result_kind":"bounded-native-source-export","result":"pass","full_native_gate_satisfied":false,"native_runtime_verified":false,"native_paths":document["paths"].as_object().map_or(0,|p|p.len()),"native_operations":actual.len(),"published_operations":published.len(),"missing_common_operations":missing,"knowledge_owner_declarations_complete":false,"knowledge_owner_gap":"Native Knowledge service has no complete source-owned OpenAPI exporter yet; wildcard declarations remain opaque","determinism_processes":2,"producer_sha256":hash(&fs::read(producer).map_err(|e|e.to_string())?),"candidate_sha256":hash(candidate.as_bytes())});
    for path in [&output, &coverage] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    fs::write(output, candidate).map_err(|e| e.to_string())?;
    fs::write(
        coverage,
        format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "Exported {} declared native operations; full native contract remains incomplete.",
        actual.len()
    ))
}
