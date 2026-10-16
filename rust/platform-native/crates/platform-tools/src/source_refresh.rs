//! Source refresh invalidates every runnable image; it never supplies build evidence.
use super::*;
use std::io::Write;
const SCHEMA: &str = "database/migrations/096_agent_capability_executions.sql";
fn atomic(path: &Path, value: &Value) -> Result<()> {
    let mut value = value.clone();
    std::fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
    value.sort_all_objects();
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(format!("{}\n", serde_json::to_string_pretty(&value)?).as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        std::fs::File::open(path.parent().ok_or("missing parent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
fn invalidate(lock: &mut Value) {
    lock["release_state"] = json!("local_source_locked");
    let mut artifacts = lock["oci"]["artifacts"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (id, binary) in ARTIFACTS {
        let old = artifacts.get(*id).cloned().unwrap_or(Value::Null);
        artifacts.insert((*id).into(),json!({"binary":binary,"protocol":old["protocol"],"candidate_start_allowed":false,"image_ref":null,"image_digest":null,"platforms":old.get("platforms").cloned().unwrap_or(json!(["linux/arm64"]))}));
    }
    lock["oci"] = json!({"artifacts":artifacts});
}
pub fn refresh(args: &Args) -> Result<Value> {
    let root = args.root();
    let path = Path::new(args.get("--lock")?);
    let mut lock = object(path)?;
    require(
        lock["schema_version"] == LOCK_SCHEMA,
        "source lock must be upgraded before refresh",
    )?;
    let old = lock["build"].clone();
    let receipt_rel = text(&old, "source_receipt")?;
    let sbom_rel = text(&old, "sbom")?;
    let notice_rel = text(&lock["license"], "notice")?.to_owned();
    let receipt_path = safe(&root, receipt_rel)?;
    let receipt = object(&receipt_path)?;
    require(
        receipt["schema_version"] == RECEIPT_SCHEMA,
        "unsupported receipt",
    )?;
    let source = &receipt["source"];
    for key in ["upstream_sha", "fork_sha", "git_tree_sha"] {
        require(hex(text(source, key)?, 40), "invalid source SHA")?;
    }
    require(
        hex(text(&receipt["schema_bundle"], "sha256")?, 64),
        "invalid schema SHA",
    )?;
    let sbom_sha = digest(&safe(&root, sbom_rel)?)?;
    require(
        receipt["sbom"]["sha256"] == sbom_sha,
        "SBOM differs from receipt",
    )?;
    let fork = text(&lock["source"], "fork_repository")?.to_owned();
    lock["source"] = json!({"upstream_url":source["upstream_url"],"upstream_sha":source["upstream_sha"],"fork_repository":fork,"fork_sha":source["fork_sha"],"git_tree_sha":source["git_tree_sha"],"branch":source["branch"]});
    let tc = &receipt["toolchain"];
    lock["build"] = json!({"rustc":tc["rustc"],"cargo":tc["cargo"],"local_image_profile":old.get("local_image_profile").cloned().unwrap_or(json!("dev-small")),"rust_toolchain_sha256":tc["rust_toolchain_sha256"],"cargo_lock_sha256":tc["cargo_lock_sha256"],"app_server_schema_sha256":receipt["schema_bundle"]["sha256"],"app_server_schema_file_count":receipt["schema_bundle"]["file_count"],"capability_worker_schema_sha256":digest(&safe(&root,SCHEMA)?)?,"capability_worker_sbom":receipt["capability_worker_sbom"]["path"],"capability_worker_sbom_sha256":receipt["capability_worker_sbom"]["sha256"],"source_receipt":receipt_rel,"source_receipt_sha256":digest(&receipt_path)?,"sbom":sbom_rel,"sbom_sha256":sbom_sha});
    if old["overlay_manifest"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        lock["build"]["overlay_manifest"] = old["overlay_manifest"].clone();
        for (to, from) in [
            ("overlay_sha256", "sha256"),
            ("overlay_file_count", "file_count"),
            ("overlay_cargo_lock_sha256", "cargo_lock_sha256"),
        ] {
            lock["build"][to] = receipt["overlay"][from].clone();
        }
        if receipt["overlay"]["manifest_schema"].is_string() {
            for (to, from) in [
                ("overlay_manifest_schema", "manifest_schema"),
                ("overlay_manifest_sha256", "manifest_sha256"),
            ] {
                lock["build"][to] = receipt["overlay"][from].clone();
            }
        }
    }
    lock["license"] = json!({"spdx":"Apache-2.0","notice":notice_rel,"notice_sha256":digest(&safe(&root,&notice_rel)?)?,"upstream_license_sha256":receipt["license"]["license_sha256"],"upstream_notice_sha256":receipt["license"]["upstream_notice_sha256"]});
    invalidate(&mut lock);
    atomic(path, &lock)?;
    validate_lock(&root, path, false, None)
}
fn worker_sbom(
    metadata: &Value,
    overlay: &Value,
    schema: &str,
    lock: &str,
    source: &Value,
) -> Result<Value> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("metadata packages missing")?;
    let mut by_id = BTreeMap::new();
    let mut roots = Vec::new();
    for p in packages {
        let id = text(p, "id")?;
        require(by_id.insert(id, p).is_none(), "duplicate package ID")?;
        if p["name"] == "ai-platform-capability-worker" {
            roots.push(id);
        }
    }
    require(
        roots.len() == 1,
        "exactly one capability worker package required",
    )?;
    let mut nodes = BTreeMap::new();
    for n in metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("resolve nodes missing")?
    {
        require(
            nodes.insert(text(n, "id")?, n).is_none(),
            "duplicate resolve node",
        )?;
    }
    let mut closure = BTreeSet::new();
    let mut pending = vec![roots[0]];
    while let Some(id) = pending.pop() {
        if !closure.insert(id) {
            continue;
        }
        require(by_id.contains_key(id), "unknown resolved package")?;
        let n = nodes.get(id).ok_or("unknown resolved node")?;
        for dep in n["dependencies"]
            .as_array()
            .ok_or("dependencies must be array")?
        {
            pending.push(if dep.is_object() {
                text(dep, "pkg")?
            } else {
                dep.as_str().ok_or("invalid dependency")?
            });
        }
    }
    let mut refs = BTreeMap::new();
    for id in &closure {
        let p = by_id[id];
        let suffix = p["source"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| {
                format!(
                    "?source={:.16}",
                    format!("{:x}", Sha256::digest(s.as_bytes()))
                )
            })
            .unwrap_or_default();
        refs.insert(
            *id,
            format!(
                "pkg:cargo/{}@{}{suffix}",
                text(p, "name")?,
                text(p, "version")?
            ),
        );
    }
    require(
        refs.values().collect::<BTreeSet<_>>().len() == refs.len(),
        "duplicate deterministic SBOM refs",
    )?;
    let mut ordered: Vec<_> = closure.iter().copied().collect();
    ordered.sort_by_key(|id| &refs[id]);
    let mut components = Vec::new();
    let mut dependencies = Vec::new();
    for id in ordered {
        let p = by_id[id];
        let mut component = json!({"type":if p["name"]=="ai-platform-capability-worker" {"application"} else {"library"},"bom-ref":refs[id],"name":p["name"],"version":p["version"],"purl":refs[id]});
        if let Some(license) = p["license"].as_str().filter(|s| !s.is_empty()) {
            component["licenses"] = json!([{"expression":license}]);
        }
        components.push(component);
        let mut deps = BTreeSet::new();
        for dep in nodes[id]["dependencies"]
            .as_array()
            .ok_or("dependencies missing")?
        {
            let dep = if dep.is_object() {
                text(dep, "pkg")?
            } else {
                dep.as_str().ok_or("invalid dependency")?
            };
            if let Some(reference) = refs.get(dep) {
                deps.insert(reference);
            }
        }
        dependencies.push(json!({"ref":refs[id],"dependsOn":deps}));
    }
    // RFC 4122 UUIDv5 with URL namespace; independent of host paths/order.
    let mut h = sha1::Sha1::new();
    h.update([
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]);
    h.update(
        format!(
            "ai-platform-capability-worker:{}:{}:{schema}:{lock}",
            text(source, "fork_sha")?,
            text(overlay, "sha256")?
        )
        .as_bytes(),
    );
    let hash = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 15) | 80;
    bytes[8] = (bytes[8] & 63) | 128;
    Ok(
        json!({"bomFormat":"CycloneDX","specVersion":"1.5","serialNumber":format!("urn:uuid:{}",uuid::Uuid::from_bytes(bytes)),"version":1,"metadata":{"component":{"type":"application","bom-ref":refs[roots[0]],"name":"ai-platform-capability-worker","version":format!("{}+{}",text(source,"upstream_sha")?,&text(overlay,"sha256")?[..12]),"properties":[{"name":"ai-platform:overlay.sha256","value":overlay["sha256"]},{"name":"ai-platform:capability-schema.sha256","value":schema},{"name":"ai-platform:cargo-lock.sha256","value":lock}]}},"components":components,"dependencies":dependencies}),
    )
}
pub fn overlay(args: &Args) -> Result<Value> {
    let root = args.root();
    let path = Path::new(args.get("--lock")?);
    let mut lock = object(path)?;
    require(
        lock["schema_version"] == LOCK_SCHEMA,
        "unsupported source lock",
    )?;
    let receipt_path = safe(&root, text(&lock["build"], "source_receipt")?)?;
    let mut receipt = object(&receipt_path)?;
    require(
        receipt["schema_version"] == RECEIPT_SCHEMA,
        "unsupported receipt",
    )?;
    let requested = std::fs::canonicalize(args.get("--cargo-workspace")?)?;
    let cwd = if requested.is_file() {
        requested.parent().ok_or("missing workspace parent")?
    } else {
        &requested
    };
    let overlay_root = root.join("rust/agent-runtime-overlay");
    for rel in [
        "Cargo.toml",
        "Cargo.lock",
        "ai-platform-capability-contract/Cargo.toml",
        "ai-platform-capability-worker/Cargo.toml",
    ] {
        require(
            digest(&safe(cwd, rel)?)? == digest(&safe(&overlay_root.join("kernel-rs"), rel)?)?,
            format!("composed overlay differs: {rel}"),
        )?;
    }
    let metadata = if let Some(p) = args.values.get("--cargo-metadata") {
        let p = Path::new(p);
        require(
            digest(
                &p.parent()
                    .ok_or("missing metadata parent")?
                    .join("Cargo.lock"),
            )? == digest(&cwd.join("Cargo.lock"))?,
            "offline metadata lock mismatch",
        )?;
        object(p)?
    } else {
        let output = Command::new("cargo")
            .args(["metadata", "--locked", "--format-version", "1"])
            .current_dir(cwd)
            .output()?;
        require(output.status.success(), "cargo metadata failed")?;
        strict_json(&output.stdout)?
    };
    let identity = bundle_identity(&overlay_root, true)?;
    let cargo_lock = digest(&overlay_root.join("kernel-rs/Cargo.lock"))?;
    let schema = digest(&safe(&root, SCHEMA)?)?;
    let source = receipt["source"].clone();
    for key in ["upstream_sha", "fork_sha"] {
        require(hex(text(&source, key)?, 40), "invalid source SHA")?;
    }
    let manifest_path = overlay_root.join("manifest.json");
    let mut manifest = object(&manifest_path)?;
    for (k, v) in [
        ("upstream_sha", source["upstream_sha"].clone()),
        ("source_revision", source["fork_sha"].clone()),
        ("file_count", identity["file_count"].clone()),
        ("sha256", identity["sha256"].clone()),
        ("cargo_lock_sha256", json!(cargo_lock)),
    ] {
        manifest[k] = v;
    }
    let sbom_rel = lock["build"]["capability_worker_sbom"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("deploy/agent-runtime-source/capability-worker-sbom.cdx.json")
        .to_owned();
    let sbom_path = safe(&root, &sbom_rel)?;
    let sbom = worker_sbom(&metadata, &identity, &schema, &cargo_lock, &source)?;
    atomic(&manifest_path, &manifest)?;
    atomic(&sbom_path, &sbom)?;
    receipt["overlay"] = json!({"sha256":identity["sha256"],"file_count":identity["file_count"],"cargo_lock_sha256":cargo_lock,"source_revision":source["fork_sha"],"upstream_sha":source["upstream_sha"]});
    if manifest["schema_version"] == "ai-platform/agent-runtime-overlay/v2" {
        let sha = digest(&manifest_path)?;
        receipt["overlay"]["manifest_schema"] = manifest["schema_version"].clone();
        receipt["overlay"]["manifest_sha256"] = json!(sha);
        lock["build"]["overlay_manifest_schema"] = manifest["schema_version"].clone();
        lock["build"]["overlay_manifest_sha256"] = json!(sha);
    }
    let sbom_sha = digest(&sbom_path)?;
    receipt["capability_worker_schema_sha256"] = json!(schema);
    receipt["capability_worker_sbom"] = json!({"format":"CycloneDX-1.5","path":sbom_rel,"sha256":sbom_sha,"component_count":sbom["components"].as_array().ok_or("missing components")?.len()});
    atomic(&receipt_path, &receipt)?;
    for key in ["upstream_url", "upstream_sha", "fork_sha", "git_tree_sha"] {
        lock["source"][key] = source[key].clone();
    }
    for (k, v) in [
        (
            "overlay_manifest",
            json!("rust/agent-runtime-overlay/manifest.json"),
        ),
        ("overlay_sha256", identity["sha256"].clone()),
        ("overlay_file_count", identity["file_count"].clone()),
        ("overlay_cargo_lock_sha256", json!(cargo_lock)),
        ("capability_worker_schema_sha256", json!(schema)),
        ("capability_worker_sbom", json!(sbom_rel)),
        ("capability_worker_sbom_sha256", json!(sbom_sha)),
        ("source_receipt_sha256", json!(digest(&receipt_path)?)),
    ] {
        lock["build"][k] = v;
    }
    invalidate(&mut lock);
    atomic(path, &lock)?;
    validate_lock(&root, path, false, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph() -> Value {
        json!({"packages":[{"id":"worker","name":"ai-platform-capability-worker","version":"1.0","source":null,"license":"Apache-2.0"},{"id":"dep","name":"dep","version":"2.0","source":"registry+https://example.invalid"},{"id":"unused","name":"unused","version":"9.0"}],"resolve":{"nodes":[{"id":"worker","dependencies":["dep","dep"]},{"id":"dep","dependencies":[]},{"id":"unused","dependencies":[]}]}})
    }
    #[test]
    fn worker_dependency_closure_is_stable_and_excludes_unreachable_packages() {
        let overlay = json!({"sha256":"a".repeat(64)});
        let source = json!({"fork_sha":"b".repeat(40),"upstream_sha":"c".repeat(40)});
        let mut metadata = graph();
        let a = worker_sbom(&metadata, &overlay, "schema", "lock", &source).unwrap();
        metadata["packages"].as_array_mut().unwrap().reverse();
        metadata["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .reverse();
        let b = worker_sbom(&metadata, &overlay, "schema", "lock", &source).unwrap();
        assert_eq!(a, b);
        assert_eq!(a["components"].as_array().unwrap().len(), 2);
        assert_eq!(
            a["dependencies"][0]["dependsOn"].as_array().unwrap().len(),
            1
        );
        let uuid = uuid::Uuid::parse_str(
            a["serialNumber"]
                .as_str()
                .unwrap()
                .strip_prefix("urn:uuid:")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(uuid.get_version_num(), 5);
    }
    #[test]
    fn worker_dependency_graph_rejects_missing_and_colliding_packages() {
        let overlay = json!({"sha256":"a".repeat(64)});
        let source = json!({"fork_sha":"b".repeat(40),"upstream_sha":"c".repeat(40)});
        let mut m = graph();
        m["resolve"]["nodes"][0]["dependencies"] = json!(["absent"]);
        assert!(worker_sbom(&m, &overlay, "s", "l", &source).is_err());
        let mut m = graph();
        m["packages"].as_array_mut().unwrap().push(json!({"id":"other","name":"dep","version":"2.0","source":"registry+https://example.invalid"}));
        m["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"other","dependencies":[]}));
        m["resolve"]["nodes"][0]["dependencies"] = json!(["dep", "other"]);
        assert!(worker_sbom(&m, &overlay, "s", "l", &source).is_err());
    }
    #[test]
    fn refreshed_source_never_retains_runnable_images() {
        let mut lock = json!({"release_state":"published","oci":{"artifacts":{"agent_runtime":{"protocol":"test","candidate_start_allowed":true,"image_ref":"old","image_digest":"old","platforms":["linux/amd64"]}}}});
        invalidate(&mut lock);
        assert_eq!(lock["release_state"], "local_source_locked");
        assert_eq!(
            lock["oci"]["artifacts"]["agent_runtime"]["platforms"],
            json!(["linux/amd64"])
        );
        for (id, _) in ARTIFACTS {
            assert_eq!(
                lock["oci"]["artifacts"][id]["candidate_start_allowed"],
                false
            );
            assert!(lock["oci"]["artifacts"][id]["image_ref"].is_null());
        }
    }
}

fn serial(seed: &str) -> String {
    let mut h = sha1::Sha1::new();
    h.update([
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]);
    h.update(seed.as_bytes());
    let hash = h.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 15) | 80;
    bytes[8] = (bytes[8] & 63) | 128;
    format!("urn:uuid:{}", uuid::Uuid::from_bytes(bytes))
}
fn components(metadata: &Value) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for p in metadata["packages"].as_array().ok_or("packages missing")? {
        let name = text(p, "name")?;
        let version = text(p, "version")?;
        let reference = format!("pkg:cargo/{name}@{version}");
        let mut c = json!({"type":"library","bom-ref":reference,"name":name,"version":version,"purl":reference});
        if let Some(license) = p["license"].as_str().filter(|s| !s.is_empty()) {
            c["licenses"] = json!([{"expression":license}]);
        }
        if let Some(source) = p["source"].as_str().filter(|s| !s.is_empty()) {
            c["properties"] = json!([{"name":"cargo:source","value":source}]);
        }
        out.push(c);
    }
    out.sort_by(|a, b| {
        (
            a["name"].as_str(),
            a["version"].as_str(),
            a["bom-ref"].as_str(),
        )
            .cmp(&(
                b["name"].as_str(),
                b["version"].as_str(),
                b["bom-ref"].as_str(),
            ))
    });
    Ok(out)
}
fn command(cwd: &Path, name: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(name).args(args).current_dir(cwd).output()?;
    require(output.status.success(), format!("{name} command failed"))?;
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
pub fn generate(args: &Args) -> Result<Value> {
    let fork = std::fs::canonicalize(args.get("--fork")?)?;
    let schema = std::fs::canonicalize(args.get("--schema-dir")?)?;
    let receipt_path = Path::new(args.get("--receipt")?);
    let sbom_path = Path::new(args.get("--sbom")?);
    require(
        git(&fork, &["status", "--porcelain"])?.is_empty(),
        "source fork must be clean",
    )?;
    let git_text = |args: &[&str]| -> Result<String> {
        Ok(String::from_utf8(git(&fork, args)?)?.trim().into())
    };
    let fork_sha = git_text(&["rev-parse", "HEAD"])?;
    let upstream_sha = git_text(&["merge-base", "HEAD", "upstream/main"])?;
    let tree = git_text(&["rev-parse", "HEAD^{tree}"])?;
    let branch = git_text(&["branch", "--show-current"])?;
    let url = git_text(&["remote", "get-url", "upstream"])?;
    let cwd = fork.join("codex-rs");
    let metadata = strict_json(
        command(
            &cwd,
            "cargo",
            &["metadata", "--locked", "--format-version", "1"],
        )?
        .as_bytes(),
    )?;
    let cargo = command(&cwd, "cargo", &["--version"])?;
    let rustc = command(&cwd, "rustc", &["--version"])?;
    let components = components(&metadata)?;
    let sbom = json!({"bomFormat":"CycloneDX","specVersion":"1.5","serialNumber":serial(&format!("ai-platform-agent-runtime:{fork_sha}:{tree}")),"version":1,"metadata":{"component":{"type":"application","bom-ref":format!("urn:git:{fork_sha}"),"name":"ai-platform-agent-runtime-source","version":fork_sha,"properties":[{"name":"ai-platform:upstream.url","value":url},{"name":"ai-platform:upstream.sha","value":upstream_sha},{"name":"ai-platform:git.tree","value":tree}]}},"components":components});
    let schema = bundle_identity(&schema, false)?;
    let toolchain = json!({"cargo":cargo,"rustc":rustc,"rust_toolchain_sha256":digest(&cwd.join("rust-toolchain.toml"))?,"cargo_lock_sha256":digest(&cwd.join("Cargo.lock"))?});
    let license = json!({"spdx":"Apache-2.0","license_sha256":digest(&fork.join("LICENSE"))?,"upstream_notice_sha256":digest(&fork.join("NOTICE"))?});
    std::fs::create_dir_all(sbom_path.parent().ok_or("missing SBOM parent")?)?;
    atomic(sbom_path, &sbom)?;
    let receipt = json!({"schema_version":RECEIPT_SCHEMA,"source":{"upstream_url":url,"upstream_sha":upstream_sha,"fork_sha":fork_sha,"git_tree_sha":tree,"branch":branch},"toolchain":toolchain,"schema_bundle":schema,"license":license,"sbom":{"format":"CycloneDX-1.5","component_count":components.len(),"sha256":digest(sbom_path)?}});
    std::fs::create_dir_all(receipt_path.parent().ok_or("missing receipt parent")?)?;
    atomic(receipt_path, &receipt)?;
    Ok(json!({"result":"pass","receipt":receipt_path,"sbom":sbom_path}))
}

pub fn composed(args: &Args) -> Result<Value> {
    let canonical = |key| -> Result<PathBuf> { Ok(std::fs::canonicalize(args.get(key)?)?) };
    let package = canonical("--package")?;
    let checkout = canonical("--upstream")?;
    let composed = canonical("--composed")?;
    let root = std::fs::canonicalize(args.root())?;
    let export = canonical("--export-evidence")?;
    let schema_dir = canonical("--schema-dir")?;
    let output = crate::resolve_path(args.get("--output")?)?;
    require(!output.exists(), "source evidence output must not exist")?;
    for input in [
        &package,
        &checkout,
        &composed,
        &root,
        &schema_dir,
        export.parent().ok_or("missing export parent")?,
    ] {
        require(
            !output.starts_with(input),
            "evidence output must be external to inputs",
        )?;
    }
    let unit_path = package.join("source-unit.json");
    let unit = object(&unit_path)?;
    verify_payload(&package, &unit)?;
    let entries = upstream(&unit, &checkout)?;
    verify_composed(&unit, &entries, &composed)?;
    live_preimages(&package, &root)?;
    let gt = |args: &[&str]| -> Result<String> {
        Ok(String::from_utf8(git(&checkout, args)?)?.trim().into())
    };
    require(
        gt(&["status", "--porcelain", "--untracked-files=all"])?.is_empty(),
        "upstream must be clean",
    )?;
    let target = text(&unit, "target")?;
    require(
        gt(&["rev-parse", "HEAD"])? == target
            && gt(&["rev-parse", "HEAD^{tree}"])? == text(&unit, "target_tree")?,
        "upstream HEAD/tree differs",
    )?;
    require(
        gt(&["remote", "get-url", "origin"])? == "https://github.com/openai/codex.git",
        "official upstream origin required",
    )?;
    let evidence = object(&export)?;
    require(
        evidence["schema"] == "ai-platform/composed-source-export/v1",
        "unsupported export evidence",
    )?;
    let unit_sha = digest(&unit_path)?;
    require(
        evidence["source_unit_sha256_before"] == unit_sha
            && evidence["source_unit_sha256_after"] == unit_sha
            && evidence["target_commit"] == target,
        "export source unit mismatch",
    )?;
    let cwd = composed.join("codex-rs");
    let lock_sha = digest(&cwd.join("Cargo.lock"))?;
    require(
        evidence["resolved_lock_sha256"] == lock_sha && unit["resolved_lock_sha256"] == lock_sha,
        "resolved lock differs",
    )?;
    let exported = |name: &str| -> Result<PathBuf> {
        let record = &evidence["files"][name];
        let path = safe(
            export.parent().ok_or("missing parent")?,
            text(record, "path")?,
        )?;
        require(
            path.is_file() && digest(&path)? == text(record, "sha256")?,
            format!("export file changed: {name}"),
        )?;
        Ok(path)
    };
    let metadata_path = exported("cargo_metadata")?;
    require(
        digest(&exported("cargo_lock")?)? == lock_sha,
        "exported lock mismatch",
    )?;
    let schema_run = object(&exported("schema_command_receipt")?)?;
    let metadata_run = object(&exported("metadata_command_receipt")?)?;
    for run in [&schema_run, &metadata_run] {
        require(
            run["exit_code"] == 0
                && run["stop_reason"].is_null()
                && run["command"].is_array()
                && run["cwd"] == cwd.to_string_lossy().as_ref(),
            "export command not successful or wrong cwd",
        )?;
    }
    let toolchain: toml::Value =
        std::fs::read_to_string(cwd.join("rust-toolchain.toml"))?.parse()?;
    let channel = toolchain["toolchain"]["channel"]
        .as_str()
        .ok_or("toolchain channel missing")?;
    let schema_cmd = json!([
        format!("+{channel}"),
        "run",
        "--locked",
        "--offline",
        "-p",
        "codex-app-server-protocol",
        "--example",
        "ai_platform_export_schema",
        "--",
        schema_dir
    ]);
    let metadata_cmd = json!([
        format!("+{channel}"),
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1"
    ]);
    require(
        schema_run["command"].as_array().and_then(|a| a.get(1..))
            == schema_cmd.as_array().map(|a| a.as_slice()),
        "schema command must be exact compiled offline protocol export",
    )?;
    require(
        metadata_run["command"].as_array().and_then(|a| a.get(1..))
            == metadata_cmd.as_array().map(|a| a.as_slice()),
        "metadata command must be exact locked offline export",
    )?;
    require(
        metadata_run["stdout_sha256"] == digest(&metadata_path)?,
        "metadata stdout not hash bound",
    )?;
    let schema = bundle_identity(&schema_dir, false)?;
    require(
        evidence["schema_bundle"] == schema,
        "compiled schema bundle differs",
    )?;
    for path in walk(&schema_dir, &[], true)? {
        if path.extension().is_some_and(|s| s == "json") {
            strict_json(&std::fs::read(safe(&schema_dir, &path.to_string_lossy())?)?)?;
        }
    }
    let cargo = std::fs::read_to_string(exported("cargo_version")?)?
        .trim()
        .to_owned();
    let rustc = std::fs::read_to_string(exported("rustc_version")?)?
        .trim()
        .to_owned();
    require(
        cargo.starts_with(&format!("cargo {channel} "))
            && rustc.starts_with(&format!("rustc {channel} ")),
        "export toolchain differs",
    )?;
    let metadata = object(&metadata_path)?;
    require(
        std::fs::canonicalize(text(&metadata, "workspace_root")?)? == cwd,
        "metadata workspace differs",
    )?;
    let packages = metadata["packages"].as_array().ok_or("packages missing")?;
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("members missing")?;
    let names: BTreeSet<_> = packages
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
        .all(|n| names.contains(n)),
        "platform workspace members missing",
    )?;
    let cargo_lock: toml::Value = std::fs::read_to_string(cwd.join("Cargo.lock"))?.parse()?;
    let locked: BTreeSet<_> = cargo_lock["package"]
        .as_array()
        .ok_or("lock packages missing")?
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap_or(""),
                p["version"].as_str().unwrap_or(""),
                p.get("source").and_then(toml::Value::as_str),
            )
        })
        .collect();
    for p in packages {
        require(
            locked.contains(&(text(p, "name")?, text(p, "version")?, p["source"].as_str())),
            "metadata package absent from exact lock",
        )?;
        if p["source"].is_null() {
            let path = std::fs::canonicalize(text(p, "manifest_path")?)?;
            require(
                path.starts_with(&composed) && path.is_file(),
                "metadata path package outside source",
            )?;
        }
    }
    let source = json!({"upstream_url":"https://github.com/openai/codex.git","upstream_sha":target,"fork_sha":target,"git_tree_sha":unit["target_tree"],"branch":""});
    let overlay = bundle_identity(&package.join("payload"), true)?;
    let blobs: BTreeMap<_, _> = unit["records"]
        .as_array()
        .ok_or("records missing")?
        .iter()
        .map(|r| {
            (
                r["path"].as_str().unwrap_or(""),
                r["target_upstream_blob"].clone(),
            )
        })
        .collect();
    let manifest = json!({"schema_version":"ai-platform/agent-runtime-overlay/v2","upstream_sha":target,"source_revision":target,"file_count":overlay["file_count"],"sha256":overlay["sha256"],"cargo_lock_sha256":lock_sha,"upstream_blobs":blobs});
    let components = components(&metadata)?;
    let overlay_sha = text(&overlay, "sha256")?;
    let sbom = json!({"bomFormat":"CycloneDX","specVersion":"1.5","version":1,"serialNumber":serial(&format!("ai-platform-composed-source:{target}:{overlay_sha}:{lock_sha}")),"metadata":{"component":{"type":"application","name":"ai-platform-agent-runtime-source","bom-ref":format!("urn:git:{target}:overlay:{overlay_sha}"),"version":format!("{target}+{}",&overlay_sha[..12]),"properties":[{"name":"ai-platform:git.tree","value":unit["target_tree"]},{"name":"ai-platform:overlay.sha256","value":overlay_sha},{"name":"ai-platform:cargo-lock.sha256","value":lock_sha}]}},"components":components});
    let schema_sha = digest(&safe(&root, SCHEMA)?)?;
    let worker = worker_sbom(&metadata, &overlay, &schema_sha, &lock_sha, &source)?;
    let toolchain_sha = digest(&cwd.join("rust-toolchain.toml"))?;
    let license_sha = digest(&checkout.join("LICENSE"))?;
    let notice_sha = digest(&checkout.join("NOTICE"))?;
    let export_sha = digest(&export)?;
    let metadata_sha = digest(&metadata_path)?;
    std::fs::create_dir_all(&output)?;
    for (name, value) in [
        ("manifest.json", &manifest),
        ("sbom.cdx.json", &sbom),
        ("capability-worker-sbom.cdx.json", &worker),
    ] {
        atomic(&output.join(name), value)?;
    }
    let receipt = json!({"schema_version":RECEIPT_SCHEMA,"source":source,"toolchain":{"cargo":cargo,"rustc":rustc,"rust_toolchain_sha256":toolchain_sha,"cargo_lock_sha256":lock_sha},"schema_bundle":schema,"license":{"spdx":"Apache-2.0","license_sha256":license_sha,"upstream_notice_sha256":notice_sha},"sbom":{"format":"CycloneDX-1.5","component_count":components.len(),"sha256":digest(&output.join("sbom.cdx.json"))?},"overlay":{"sha256":overlay["sha256"],"file_count":overlay["file_count"],"upstream_sha":target,"source_revision":target,"cargo_lock_sha256":lock_sha,"manifest_schema":manifest["schema_version"],"manifest_sha256":digest(&output.join("manifest.json"))?},"capability_worker_schema_sha256":schema_sha,"capability_worker_sbom":{"format":"CycloneDX-1.5","path":"deploy/agent-runtime-source/capability-worker-sbom.cdx.json","sha256":digest(&output.join("capability-worker-sbom.cdx.json"))?,"component_count":worker["components"].as_array().ok_or("components missing")?.len()},"evidence_provenance":{"method":"verified immutable upstream plus source-only overlay; offline compiled protocol export","source_unit_sha256":unit_sha,"export_evidence_sha256":export_sha,"metadata_file_sha256":metadata_sha,"schema_generated_by_compiled_protocol_library":true,"runtime_compiled":false,"worker_compiled":false,"docker_image_verified":false}});
    atomic(&output.join("source-receipt.json"), &receipt)?;
    Ok(
        json!({"result":"pass","output":output,"runtime_compiled":false,"worker_compiled":false,"docker_image_verified":false}),
    )
}
