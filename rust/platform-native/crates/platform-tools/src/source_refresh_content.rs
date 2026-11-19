//! Rebind a previously admitted dependency graph only when its inputs are unchanged.
//! This is source evidence reuse, never a Cargo run or image-build receipt.
use super::*;
#[cfg(test)]
#[path = "source_refresh_content_tests.rs"]
mod tests;

const OVERLAY: &str = "rust/agent-runtime-overlay";
const LOCK: &str = "deploy/agent-runtime-source/lock.json";

fn baseline_file(root: &Path, revision: &str, relative: &str) -> Result<Vec<u8>> {
    safe(root, relative)?;
    require(
        !Path::new(relative).components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with(".env"))
        }),
        "private configuration is not a dependency input",
    )?;
    git(root, &["show", &format!("{revision}:{relative}")])
}

fn unchanged_file(root: &Path, revision: &str, relative: &str) -> Result<Value> {
    let before = baseline_file(root, revision, relative)?;
    require(
        before == std::fs::read(safe(root, relative)?)?,
        format!("content-only refresh baseline differs: {relative}"),
    )?;
    strict_json(&before)
}

fn baseline_overlay(root: &Path, revision: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for record in git(
        root,
        &["ls-tree", "-rz", "--full-tree", revision, "--", OVERLAY],
    )?
    .split(|byte| *byte == 0)
    .filter(|record| !record.is_empty())
    {
        let record = std::str::from_utf8(record)?;
        let (metadata, path) = record
            .split_once('\t')
            .ok_or("malformed baseline Git tree")?;
        let metadata: Vec<_> = metadata.split_whitespace().collect();
        require(
            metadata.len() == 3
                && metadata[1] == "blob"
                && matches!(metadata[0], "100644" | "100755"),
            "content-only baseline requires regular public source files",
        )?;
        let relative = path
            .strip_prefix(&format!("{OVERLAY}/"))
            .ok_or("invalid overlay path")?;
        if relative != "manifest.json" {
            files.insert(relative.to_owned(), baseline_file(root, revision, path)?);
        }
    }
    require(!files.is_empty(), "baseline overlay is empty")?;
    Ok(files)
}

fn dependency_input(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml")
    ) || path.components().any(|part| part.as_os_str() == ".cargo")
}

fn verify_graph(sbom: &Value) -> Result<&str> {
    let root = text(&sbom["metadata"]["component"], "bom-ref")?;
    let components = sbom["components"]
        .as_array()
        .ok_or("SBOM components missing")?;
    let mut refs = BTreeSet::new();
    let mut workers = Vec::new();
    for component in components {
        let reference = text(component, "bom-ref")?;
        require(refs.insert(reference), "duplicate SBOM component")?;
        if component["name"] == "ai-platform-capability-worker" {
            require(
                component["type"] == "application",
                "invalid Worker SBOM root type",
            )?;
            workers.push(reference);
        }
    }
    require(
        workers == [root],
        "exactly one matching Worker SBOM root required",
    )?;
    let mut graph = BTreeMap::new();
    for node in sbom["dependencies"]
        .as_array()
        .ok_or("SBOM dependencies missing")?
    {
        let reference = text(node, "ref")?;
        let mut dependencies = BTreeSet::new();
        for dependency in node["dependsOn"].as_array().ok_or("SBOM edges missing")? {
            let dependency = dependency.as_str().ok_or("invalid SBOM edge")?;
            require(
                refs.contains(dependency) && dependencies.insert(dependency),
                "invalid SBOM edge",
            )?;
        }
        require(
            refs.contains(reference) && graph.insert(reference, dependencies).is_none(),
            "invalid SBOM node",
        )?;
    }
    require(
        graph.keys().copied().collect::<BTreeSet<_>>() == refs,
        "incomplete SBOM graph",
    )?;
    let mut seen = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(reference) = pending.pop() {
        if seen.insert(reference) {
            pending.extend(graph[reference].iter().copied());
        }
    }
    require(seen == refs, "unreachable Worker SBOM component")?;
    Ok(root)
}

pub(super) fn reuse_worker_sbom(
    root: &Path,
    revision: &str,
    lock_path: &Path,
    lock: &Value,
    overlay: &Value,
    schema: &str,
    cargo_lock: &str,
) -> Result<Value> {
    require(
        hex(revision, 40),
        "content-only baseline requires a full immutable Git commit SHA",
    )?;
    require(
        String::from_utf8(git(
            root,
            &["rev-parse", &format!("{revision}^{{commit}}")],
        )?)?
        .trim()
            == revision,
        "content-only baseline must identify a commit",
    )?;
    require(
        lock_path.canonicalize()? == root.join(LOCK).canonicalize()?,
        "unexpected content-only lock path",
    )?;
    // Pin the complete original lock, including platform, compiler, source and
    // build policy; none may be changed while reusing its dependency graph.
    require(
        unchanged_file(root, revision, LOCK)? == *lock,
        "baseline lock differs",
    )?;
    let receipt = unchanged_file(root, revision, text(&lock["build"], "source_receipt")?)?;
    let manifest = unchanged_file(root, revision, "rust/agent-runtime-overlay/manifest.json")?;
    let sbom_path = text(&lock["build"], "capability_worker_sbom")?;
    let sbom = unchanged_file(root, revision, sbom_path)?;
    require(
        receipt["source"] == lock["source"]
            || ["upstream_sha", "fork_sha", "git_tree_sha", "upstream_url"]
                .iter()
                .all(|key| receipt["source"][key] == lock["source"][key]),
        "baseline source identity differs",
    )?;
    require(
        digest(&safe(root, text(&lock["build"], "source_receipt")?)?)?
            == lock["build"]["source_receipt_sha256"]
            && digest(&safe(root, sbom_path)?)? == lock["build"]["capability_worker_sbom_sha256"]
            && lock["build"]["capability_worker_sbom_sha256"]
                == receipt["capability_worker_sbom"]["sha256"]
            && digest(&root.join(OVERLAY).join("manifest.json"))?
                == lock["build"]["overlay_manifest_sha256"]
            && lock["build"]["overlay_manifest_sha256"] == receipt["overlay"]["manifest_sha256"],
        "baseline source evidence digest mismatch",
    )?;
    let before = baseline_overlay(root, revision)?;
    let mut hash = Sha256::new();
    let mut ordered: Vec<_> = before.iter().collect();
    // Match bundle_identity's PathBuf component order, including a module.rs
    // adjacent to its module/ directory; string ordering differs there.
    ordered.sort_by(|(left, _), (right, _)| Path::new(left).cmp(Path::new(right)));
    for (relative, bytes) in ordered {
        hash.update(relative.as_bytes());
        hash.update([0]);
        hash.update(bytes);
        hash.update([0]);
    }
    let baseline_sha = format!("{:x}", hash.finalize());
    require(
        manifest["sha256"] == baseline_sha
            && lock["build"]["overlay_sha256"] == baseline_sha
            && receipt["overlay"]["sha256"] == baseline_sha
            && manifest["file_count"] == before.len()
            && receipt["overlay"]["file_count"] == before.len()
            && lock["build"]["overlay_file_count"] == before.len(),
        "baseline overlay identity mismatch",
    )?;
    let kernel = root.join(OVERLAY);
    let after = walk(&kernel, &[], true)?
        .into_iter()
        .filter(|path| dependency_input(path))
        .collect::<BTreeSet<_>>();
    let definitions = before
        .keys()
        .filter(|path| dependency_input(Path::new(path)))
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    require(
        after == definitions,
        "dependency input inventory changed; full Cargo metadata required",
    )?;
    for relative in definitions {
        require(
            std::fs::read(safe(&kernel, &relative.to_string_lossy())?)?
                == before[&relative.to_string_lossy().into_owned()],
            "dependency input changed; full Cargo metadata required",
        )?;
    }
    require(
        lock["build"]["overlay_cargo_lock_sha256"] == cargo_lock
            && manifest["cargo_lock_sha256"] == cargo_lock
            && receipt["overlay"]["cargo_lock_sha256"] == cargo_lock,
        "dependency lock identity changed",
    )?;
    let reference = verify_graph(&sbom)?;
    let mut original = bind_worker_sbom(
        reference,
        sbom["components"].clone(),
        sbom["dependencies"].clone(),
        &json!({"sha256":baseline_sha}),
        text(&lock["build"], "capability_worker_schema_sha256")?,
        cargo_lock,
        &lock["source"],
    )?;
    // Older producers used another UUIDv5 seed. Its exact bytes are already
    // pinned by the baseline Git blob and SBOM digest; check the complete source
    // metadata rather than reinterpreting that opaque historical identifier.
    let serial = uuid::Uuid::parse_str(
        text(&sbom, "serialNumber")?
            .strip_prefix("urn:uuid:")
            .ok_or("invalid baseline SBOM UUID")?,
    )?;
    require(
        serial.get_version_num() == 5 && serial.get_variant() == uuid::Variant::RFC4122,
        "invalid baseline SBOM UUID",
    )?;
    original["serialNumber"] = sbom["serialNumber"].clone();
    require(
        original == sbom,
        "baseline SBOM metadata differs from its source lock",
    )?;
    require(
        receipt["capability_worker_sbom"]["component_count"]
            == sbom["components"].as_array().unwrap().len(),
        "baseline SBOM component count mismatch",
    )?;
    bind_worker_sbom(
        reference,
        sbom["components"].clone(),
        sbom["dependencies"].clone(),
        overlay,
        schema,
        cargo_lock,
        &lock["source"],
    )
}
