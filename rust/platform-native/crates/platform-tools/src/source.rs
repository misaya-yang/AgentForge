use crate::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::PermissionsExt,
    process::Command,
};
const LOCK_SCHEMA: &str = "ai-platform/agent-runtime-source-lock/v2";
const RECEIPT_SCHEMA: &str = "ai-platform/agent-runtime-source-receipt/v1";
#[path = "source_images.rs"]
mod images;
use images::ARTIFACTS;
pub use images::{record_image, verify_images};
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing string {key}").into())
}
fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn file_matches(root: &Path, path: &Value, expected: &Value) -> Result<()> {
    let p = path.as_str().ok_or("missing artifact path")?;
    require(
        digest(&safe(root, p)?)? == expected.as_str().ok_or("missing digest")?,
        format!("artifact digest mismatch: {p}"),
    )
}
pub fn bundle_identity(root: &Path, overlay: bool) -> Result<Value> {
    let paths = walk(root, &[], true)?;
    let files: Vec<_> = paths
        .into_iter()
        .filter(|p| !overlay || p.file_name().is_none_or(|n| n != "manifest.json"))
        .collect();
    require(!files.is_empty(), "source bundle empty")?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    for p in &files {
        let full = safe(root, &p.to_string_lossy())?;
        let bytes = std::fs::read(full)?;
        hash.update(p.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(&bytes);
        hash.update([0]);
        total += bytes.len() as u64;
    }
    let mut result = json!({"sha256":format!("{:x}",hash.finalize()),"file_count":files.len()});
    if !overlay {
        result["total_bytes"] = json!(total);
    }
    Ok(result)
}
pub fn validate_lock(
    root: &Path,
    path: &Path,
    require_runnable: bool,
    required: Option<&str>,
) -> Result<Value> {
    let lock = object(path)?;
    require(
        lock["schema_version"] == LOCK_SCHEMA,
        "unsupported source lock schema",
    )?;
    let state = text(&lock, "release_state")?;
    require(
        matches!(
            state,
            "local_source_locked" | "local_image_locked" | "published"
        ),
        "invalid release_state",
    )?;
    for name in ["source", "build", "license", "oci"] {
        require(lock[name].is_object(), format!("{name} must be object"))?;
    }
    let source = &lock["source"];
    let build = &lock["build"];
    let license = &lock["license"];
    for name in ["upstream_sha", "fork_sha"] {
        require(hex(text(source, name)?, 40), "source SHA shape invalid")?;
    }
    require(
        source["upstream_url"] == "https://github.com/openai/codex.git",
        "canonical upstream required",
    )?;
    text(source, "fork_repository")?;
    let receipt = object(&safe(root, text(build, "source_receipt")?)?)?;
    require(
        receipt["schema_version"] == RECEIPT_SCHEMA,
        "unsupported source receipt",
    )?;
    if build
        .get("overlay_manifest")
        .is_some_and(|v| !v.is_null() && v != "")
    {
        require(
            build["overlay_manifest"] == "rust/agent-runtime-overlay/manifest.json",
            "unexpected overlay manifest path",
        )?;
        let overlay_root = root.join("rust/agent-runtime-overlay");
        let actual = bundle_identity(&overlay_root, true)?;
        let manifest = object(&overlay_root.join("manifest.json"))?;
        let overlay = &receipt["overlay"];
        if manifest["schema_version"] == "ai-platform/agent-runtime-overlay/v2"
            || build["overlay_manifest_schema"] == "ai-platform/agent-runtime-overlay/v2"
        {
            let sha = digest(&overlay_root.join("manifest.json"))?;
            require(
                manifest["schema_version"] == "ai-platform/agent-runtime-overlay/v2"
                    && build["overlay_manifest_schema"] == manifest["schema_version"]
                    && overlay["manifest_schema"] == manifest["schema_version"]
                    && build["overlay_manifest_sha256"] == sha
                    && overlay["manifest_sha256"] == sha,
                "overlay upstream manifest not pinned",
            )?;
        }
        for key in ["sha256", "file_count"] {
            require(
                manifest[key] == actual[key] && overlay[key] == actual[key],
                "overlay inventory digest/count mismatch",
            )?;
        }
        require(
            manifest["source_revision"] == source["fork_sha"]
                && manifest["upstream_sha"] == source["upstream_sha"],
            "overlay source identity mismatch",
        )?;
        let sha = digest(&overlay_root.join("kernel-rs/Cargo.lock"))?;
        require(
            manifest["cargo_lock_sha256"] == sha
                && overlay["cargo_lock_sha256"] == sha
                && build["overlay_cargo_lock_sha256"] == sha,
            "overlay lock mismatch",
        )?;
        require(
            build["overlay_sha256"] == actual["sha256"]
                && build["overlay_file_count"] == actual["file_count"],
            "build overlay identity mismatch",
        )?;
    }
    for key in ["upstream_sha", "fork_sha", "git_tree_sha", "upstream_url"] {
        require(
            receipt["source"][key] == source[key],
            format!("source receipt mismatch: {key}"),
        )?;
    }
    require(
        hex(text(&receipt["source"], "git_tree_sha")?, 40),
        "git tree SHA invalid",
    )?;
    for (path, key) in [
        ("source_receipt", "source_receipt_sha256"),
        ("sbom", "sbom_sha256"),
        ("capability_worker_sbom", "capability_worker_sbom_sha256"),
    ] {
        require(hex(text(build, key)?, 64), "artifact hash invalid")?;
        file_matches(root, &build[path], &build[key])?;
    }
    require(
        receipt["sbom"]["sha256"] == build["sbom_sha256"],
        "receipt SBOM mismatch",
    )?;
    let schema = digest(&root.join("database/migrations/096_agent_capability_executions.sql"))?;
    require(
        build["capability_worker_schema_sha256"] == schema
            && receipt["capability_worker_schema_sha256"] == schema,
        "capability schema mismatch",
    )?;
    require(
        receipt["capability_worker_sbom"]["sha256"] == build["capability_worker_sbom_sha256"]
            && receipt["capability_worker_sbom"]["path"] == build["capability_worker_sbom"],
        "capability SBOM receipt mismatch",
    )?;
    let sbom = object(&safe(root, text(build, "capability_worker_sbom")?)?)?;
    let component = &sbom["metadata"]["component"];
    if let Some(sha) = build["overlay_sha256"].as_str() {
        let property = component["properties"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == "ai-platform:overlay.sha256"))
            .unwrap_or(&Value::Null);
        require(
            property["value"] == sha
                && component["version"]
                    == format!(
                        "{}+{}",
                        text(source, "upstream_sha")?,
                        &sha[..sha.len().min(12)]
                    ),
            "capability SBOM describes different overlay",
        )?;
    }
    require(
        hex(text(&receipt["schema_bundle"], "sha256")?, 64)
            && receipt["schema_bundle"]["file_count"]
                .as_u64()
                .is_some_and(|n| n > 0),
        "invalid schema bundle",
    )?;
    require(
        receipt["schema_bundle"]["sha256"] == build["app_server_schema_sha256"]
            && receipt["schema_bundle"]["file_count"] == build["app_server_schema_file_count"],
        "schema receipt mismatch",
    )?;
    for key in [
        "cargo",
        "rustc",
        "rust_toolchain_sha256",
        "cargo_lock_sha256",
    ] {
        require(
            receipt["toolchain"][key] == build[key],
            format!("toolchain receipt mismatch {key}"),
        )?;
    }
    require(license["spdx"] == "Apache-2.0", "Apache license required")?;
    require(
        hex(text(license, "notice_sha256")?, 64),
        "notice hash invalid",
    )?;
    file_matches(root, &license["notice"], &license["notice_sha256"])?;
    require(
        receipt["license"]["license_sha256"] == license["upstream_license_sha256"]
            && receipt["license"]["upstream_notice_sha256"] == license["upstream_notice_sha256"],
        "upstream license receipt mismatch",
    )?;
    images::validate_artifacts(&lock, require_runnable, required)
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    require(out.status.success(), "Git source verification failed")?;
    Ok(out.stdout)
}
fn inventory(root: &Path) -> Result<BTreeSet<String>> {
    Ok(walk(root, &[".git"], true)?
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}
#[path = "source_test_inventory.rs"]
mod test_inventory;
fn verify_payload(package: &Path, unit: &Value) -> Result<()> {
    require(
        unit["schema"] == "ai-platform/runtime-source-unit/v1",
        "source unit schema unsupported",
    )?;
    for key in [
        "candidate_start_allowed",
        "generated_release_identity",
        "release_lock_refresh_ready",
    ] {
        require(unit[key] == false, "source-only safety flag changed")?;
    }
    let records = unit["records"].as_array().ok_or("source records missing")?;
    let paths: BTreeSet<_> = records
        .iter()
        .map(|r| text(r, "path").map(str::to_owned))
        .collect::<Result<_>>()?;
    require(
        paths.len() == records.len()
            && Some(paths.len() as u64) == unit["source_file_count"].as_u64(),
        "source record count mismatch",
    )?;
    let payload = package.join("payload/kernel-rs");
    require(inventory(&payload)? == paths, "payload inventory mismatch")?;
    for record in records {
        let path = safe(&payload, text(record, "path")?)?;
        require(
            digest(&path)? == record["proposed_sha256"],
            "payload digest mismatch",
        )?;
        let mode = if std::fs::metadata(path)?.permissions().mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        };
        require(
            record["file_mode"] == mode,
            "payload executable mode mismatch",
        )?;
    }
    require(
        digest(&payload.join("Cargo.lock"))? == unit["resolved_lock_sha256"],
        "resolved lock mismatch",
    )?;
    require(
        digest(&package.join("live-preimages.json"))? == unit["live_preimages_sha256"],
        "live preimages changed",
    )?;
    for (path, expected) in unit["evidence"].as_object().ok_or("evidence map missing")? {
        require(
            digest(&safe(package, path)?)? == expected.as_str().ok_or("evidence digest missing")?,
            "evidence changed",
        )?;
    }
    let candidate = object(&package.join("evidence/candidate-source.json"))?;
    require(
        digest(&package.join("evidence/candidate-source.json"))?
            == unit["input_candidate_manifest_sha256"],
        "candidate manifest identity mismatch",
    )?;
    require(
        candidate["target"] == unit["target"] && candidate["target_tree"] == unit["target_tree"],
        "candidate target mismatch",
    )?;
    let indexed = |records: &Value, omit_lock: bool| -> Result<BTreeMap<String, Value>> {
        records
            .as_array()
            .ok_or("records missing")?
            .iter()
            .filter(|r| !omit_lock || r["path"] != "Cargo.lock")
            .map(|r| Ok((text(r, "path")?.into(), r.clone())))
            .collect()
    };
    require(
        indexed(&unit["records"], true)? == indexed(&candidate["records"], false)?,
        "candidate records mismatch",
    )?;
    let receipt = object(&package.join("evidence/build-receipt.json"))?;
    require(
        receipt["resolved_lock_sha256"] == unit["resolved_lock_sha256"]
            && receipt["source_identity"]["candidate_source_sha256"]
                == unit["input_candidate_manifest_sha256"]
            && receipt["target_commit"] == unit["target"],
        "compiler identity mismatch",
    )?;
    for (file, operation, krate) in [
        (
            "worker-check.json",
            "check",
            "ai-platform-capability-worker",
        ),
        (
            "runtime-check-retry.json",
            "check",
            "ai-platform-agent-runtime",
        ),
        (
            "contract-tests.json",
            "test",
            "ai-platform-capability-contract",
        ),
    ] {
        let result = object(&package.join("evidence").join(file))?;
        require(
            result["exit_code"] == 0 && result["stop_reason"].is_null(),
            "unsuccessful compiler evidence",
        )?;
        let command = result["command"]
            .as_array()
            .ok_or("compiler command missing")?;
        let mut expected = vec![
            json!("+1.95.0"),
            json!(operation),
            json!("--locked"),
            json!("-p"),
            json!(krate),
        ];
        if operation == "check" {
            expected.push(json!("--all-targets"));
        }
        require(
            command.get(1..) == Some(expected.as_slice()),
            "compiler command mismatch",
        )?;
    }
    test_inventory::verify(package, unit, &receipt)?;
    let metadata = object(&package.join("evidence/resolved-metadata.json"))?;
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("workspace members missing")?;
    let names: BTreeSet<_> = metadata["packages"]
        .as_array()
        .ok_or("metadata packages missing")?
        .iter()
        .filter(|p| members.contains(&p["id"]))
        .filter_map(|p| p["name"].as_str())
        .collect();
    require(
        [
            "ai-platform-agent-runtime",
            "ai-platform-capability-worker",
            "ai-platform-capability-contract",
            "ai-platform-office",
        ]
        .iter()
        .all(|s| names.contains(s)),
        "metadata lacks platform crates",
    )?;
    Ok(())
}
fn upstream(unit: &Value, checkout: &Path) -> Result<BTreeMap<String, (String, String)>> {
    let target = text(unit, "target")?;
    require(hex(target, 40), "invalid target commit")?;
    let tree = git(checkout, &["rev-parse", &format!("{target}^{{tree}}")])?;
    require(
        String::from_utf8(tree)?.trim() == text(unit, "target_tree")?,
        "upstream target tree mismatch",
    )?;
    let mut entries: BTreeMap<String, (String, String)> = BTreeMap::new();
    for row in git(checkout, &["ls-tree", "-rz", "--full-tree", target])?
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
    {
        let row = std::str::from_utf8(row)?;
        let (info, path) = row.split_once('\t').ok_or("malformed Git tree")?;
        let fields: Vec<_> = info.split_whitespace().collect();
        require(
            fields.len() == 3 && fields[1] == "blob",
            "Git link/nonblob not supported",
        )?;
        require(
            !path.starts_with('/') && path.split('/').all(|s| !matches!(s, "" | ".." | ".")),
            "unsafe upstream path",
        )?;
        entries.insert(path.into(), (fields[0].into(), fields[2].into()));
    }
    for record in unit["records"].as_array().ok_or("records missing")? {
        let path = format!("codex-rs/{}", text(record, "path")?);
        let entry = entries.get(&path);
        require(
            entry.map(|e| json!(e.1)).unwrap_or(Value::Null) == record["target_upstream_blob"],
            "upstream blob mismatch",
        )?;
        if let Some((_, blob)) = entry {
            require(
                hash(&git(checkout, &["cat-file", "blob", blob])?)
                    == record["target_preimage_sha256"],
                "upstream preimage mismatch",
            )?;
        }
    }
    Ok(entries)
}
fn verify_composed(
    unit: &Value,
    entries: &BTreeMap<String, (String, String)>,
    root: &Path,
) -> Result<usize> {
    let overlay: BTreeMap<_, _> = unit["records"]
        .as_array()
        .ok_or("records missing")?
        .iter()
        .map(|r| Ok((format!("codex-rs/{}", text(r, "path")?), r)))
        .collect::<Result<_>>()?;
    let expected: BTreeSet<_> = entries.keys().chain(overlay.keys()).cloned().collect();
    require(
        inventory(root)? == expected,
        "composed source inventory mismatch",
    )?;
    for relative in &expected {
        let upstream = entries.get(relative);
        let item = overlay.get(relative);
        let expected_mode = item
            .and_then(|r| r["file_mode"].as_str())
            .or_else(|| upstream.map(|e| e.0.as_str()))
            .ok_or("source mode missing")?;
        let path = if expected_mode == "120000" {
            let p = root.join(relative);
            require(
                p.parent()
                    .ok_or("missing parent")?
                    .canonicalize()?
                    .starts_with(root.canonicalize()?)
                    && p.is_symlink(),
                "upstream symlink parent unsafe",
            )?;
            p
        } else {
            safe(root, relative)?
        };
        let actual_mode = if path.is_symlink() {
            "120000"
        } else if std::fs::metadata(&path)?.permissions().mode() & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        };
        require(actual_mode == expected_mode, "source file mode mismatch")?;
        if let Some(record) = item {
            require(
                digest(&path)? == record["proposed_sha256"],
                "composed overlay content mismatch",
            )?;
        } else {
            let data = if path.is_symlink() {
                std::fs::read_link(&path)?
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec()
            } else {
                std::fs::read(&path)?
            };
            let mut sha = sha1::Sha1::new();
            sha.update(format!("blob {}\0", data.len()).as_bytes());
            sha.update(data);
            require(
                format!("{:x}", sha.finalize()) == upstream.unwrap().1,
                "composed upstream blob mismatch",
            )?;
        }
    }
    Ok(expected.len())
}
fn live_preimages(package: &Path, root: &Path) -> Result<()> {
    let preimages = object(&package.join("live-preimages.json"))?;
    let records = preimages["records"].as_array().ok_or("preimages missing")?;
    let mut expected = BTreeSet::new();
    let prefix = "rust/agent-runtime-overlay/kernel-rs/";
    for record in records {
        let relative = text(record, "path")?;
        let path = safe(root, relative)?;
        let actual = if path.is_file() {
            json!(digest(&path)?)
        } else {
            Value::Null
        };
        require(
            actual == record["sha256"],
            "live source preimage changed; never overwrite",
        )?;
        if path.exists() {
            require(
                record["mode"]
                    == format!(
                        "0o{:o}",
                        std::fs::metadata(&path)?.permissions().mode() & 0o777
                    ),
                "live source mode changed",
            )?;
        }
        if relative.starts_with(prefix) && !record["sha256"].is_null() {
            expected.insert(relative.to_owned());
        }
    }
    let actual: BTreeSet<_> = inventory(&root.join(prefix))?
        .into_iter()
        .map(|p| format!("{prefix}{p}"))
        .collect();
    require(expected == actual, "live overlay inventory changed")
}
pub fn candidate(args: &Args) -> Result<Value> {
    let package = Path::new(args.get("--package")?);
    let checkout = Path::new(args.get("--upstream")?);
    let unit = object(&package.join("source-unit.json"))?;
    verify_payload(package, &unit)?;
    let entries = upstream(&unit, checkout)?;
    live_preimages(package, &args.root())?;
    let operation = args
        .positionals
        .get(1)
        .map(String::as_str)
        .unwrap_or("verify");
    require(
        matches!(operation, "verify" | "assemble"),
        "unknown source-candidate operation",
    )?;
    let mut compared = 0;
    if operation == "assemble" {
        let destination = Path::new(args.get("--output")?);
        require(
            !destination.exists(),
            "composition output must not already exist",
        )?;
        std::fs::create_dir_all(destination)?;
        let mut archive = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["archive", "--format=tar", text(&unit, "target")?])
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let extraction = Command::new("tar")
            .args(["-xf", "-", "--no-same-owner", "-C"])
            .arg(destination)
            .stdin(archive.stdout.take().ok_or("archive stdout missing")?)
            .status()?;
        require(
            extraction.success() && archive.wait()?.success(),
            "upstream source assembly failed",
        )?;
        for record in unit["records"].as_array().unwrap() {
            let relative = text(record, "path")?;
            let from = safe(&package.join("payload/kernel-rs"), relative)?;
            let to = safe(destination, &format!("codex-rs/{relative}"))?;
            std::fs::create_dir_all(to.parent().unwrap())?;
            std::fs::copy(from, &to)?;
            std::fs::set_permissions(
                to,
                std::fs::Permissions::from_mode(if record["file_mode"] == "100755" {
                    0o755
                } else {
                    0o644
                }),
            )?;
        }
        compared = verify_composed(&unit, &entries, destination)?;
    } else if let Some(root) = args.values.get("--composed-source") {
        compared = verify_composed(&unit, &entries, Path::new(root))?;
    }
    Ok(
        json!({"source_validation":"passed","source_files":unit["source_file_count"],"composed_files_compared":compared,"release_admission":false,"live_pin_changed":false,"candidate_start_allowed":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_hash_shape() {
        assert!(hex(&"a".repeat(64), 64));
        assert!(!hex(&"A".repeat(64), 64));
        assert!(!hex("../", 40));
    }
}
pub fn verify_overlay(args: &Args) -> Result<Value> {
    let root = args.root();
    let directory = args
        .values
        .get("--overlay-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("rust/agent-runtime-overlay"));
    let manifest = object(&directory.join("manifest.json"))?;
    let identity = bundle_identity(&directory, true)?;
    require(
        identity["sha256"] == manifest["sha256"]
            && identity["file_count"] == manifest["file_count"],
        "overlay manifest differs from actual files",
    )?;
    if let Some(sha) = args.values.get("--upstream-sha") {
        require(
            manifest["upstream_sha"] == *sha,
            "overlay upstream differs from receipt",
        )?;
    }
    if let Some(revision) = args.values.get("--source-revision") {
        require(
            manifest["source_revision"] == *revision,
            "overlay revision differs from receipt",
        )?;
    }
    if let Some(source) = args.values.get("--source") {
        let checkout = Path::new(source);
        let sha = text(&manifest, "upstream_sha")?;
        require(hex(sha, 40), "invalid upstream SHA")?;
        require(
            String::from_utf8(git(checkout, &["rev-parse", &format!("{sha}^{{commit}}")])?)?.trim()
                == sha,
            "upstream commit mismatch",
        )?;
        let declared = manifest["upstream_blobs"]
            .as_object()
            .ok_or("verified v2 overlay preimages required")?;
        let files = walk(&directory.join("kernel-rs"), &[], true)?;
        let actual: BTreeSet<_> = files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        require(
            actual == declared.keys().cloned().collect(),
            "overlay preimage inventory mismatch",
        )?;
        let mut tree = BTreeMap::new();
        for row in git(
            checkout,
            &["ls-tree", "-rz", "--full-tree", sha, "--", "codex-rs"],
        )?
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        {
            let row = std::str::from_utf8(row)?;
            let (meta, path) = row.split_once('\t').ok_or("malformed Git tree")?;
            let fields: Vec<_> = meta.split_whitespace().collect();
            require(
                fields.len() == 3 && fields[1] == "blob",
                "unsupported upstream object",
            )?;
            let relative = path
                .strip_prefix("codex-rs/")
                .ok_or("unsafe upstream tree")?;
            tree.insert(relative.to_owned(), fields[2].to_owned());
        }
        for relative in actual {
            safe(&directory.join("kernel-rs"), &relative)?;
            require(
                declared[&relative] == tree.get(&relative).map(|s| json!(s)).unwrap_or(Value::Null),
                format!("overlay upstream blob mismatch: {relative}"),
            )?;
        }
    }
    Ok(
        json!({"result":"pass","overlay":identity,"upstream_sha":manifest["upstream_sha"],"upstream_preimages_verified":args.values.contains_key("--source")}),
    )
}

#[path = "source_refresh.rs"]
mod refresh;
pub use refresh::{
    composed as generate_composed_receipt, generate as generate_receipt,
    overlay as refresh_overlay, refresh as refresh_source_lock,
};
