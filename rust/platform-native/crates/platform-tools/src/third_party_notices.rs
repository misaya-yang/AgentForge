//! Deterministic dependency SBOM/notice inventory; never approves a license.
// SPDX-License-Identifier: Apache-2.0
use crate::*;
use std::collections::{BTreeMap, BTreeSet};
mod materials;
mod owned;
use materials::*;
mod sources;
use sources::Sources;

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing dependency {key}").into())
}
fn add_component(
    components: &mut BTreeMap<String, Value>,
    id: String,
    name: &str,
    version: &str,
    license: Value,
    root: &Path,
    scope: &str,
    repository: &Path,
    sources: &Sources,
) -> Result<()> {
    let mut files = BTreeMap::new();
    notice_files(root, root, 0, &mut files)?;
    let manifest = root.join(if scope == "cargo" {
        "Cargo.toml"
    } else {
        "package.json"
    });
    sources.supplement(
        repository, &id, name, version, &license, &manifest, &mut files,
    )?;
    let status = if license.is_null()
        || license.as_str().is_some_and(|s| s.trim().is_empty())
        || files.is_empty()
    {
        "review_required_missing_declaration_or_text"
    } else {
        "collected_not_approved"
    };
    let manifest_value: Value = if scope == "cargo" {
        serde_json::to_value(toml::from_str::<toml::Value>(&std::fs::read_to_string(
            &manifest,
        )?)?)?
    } else {
        object(&manifest)?
    };
    let vcs_path = root.join(".cargo_vcs_info.json");
    let vcs = if vcs_path.is_file() {
        Some(object(&vcs_path)?)
    } else {
        None
    };
    let package_source = json!({"repository": if scope == "cargo" { &manifest_value["package"]["repository"] } else { &manifest_value["repository"] }, "authors":if scope == "cargo" { &manifest_value["package"]["authors"] } else { &manifest_value["author"] },"vcs":vcs});
    let component = json!({"package_source":package_source,"package_manifest_sha256":[hash(&std::fs::read(manifest)?)],"id":id,"name":name,"version":version,"ecosystem":scope,"declared_license":license,"notice_files":files,"status":status});
    if let Some(old) = components.get_mut(&id) {
        let mut comparison = old.clone();
        comparison["package_manifest_sha256"] = component["package_manifest_sha256"].clone();
        require(
            comparison == component,
            "same package identity has conflicting license contents",
        )?;
        let hashes: BTreeSet<String> = old["package_manifest_sha256"]
            .as_array()
            .ok_or("manifest hashes missing")?
            .iter()
            .chain(
                component["package_manifest_sha256"]
                    .as_array()
                    .ok_or("manifest hashes missing")?,
            )
            .map(|v| v.as_str().ok_or("manifest hash invalid").map(str::to_owned))
            .collect::<std::result::Result<_, _>>()?;
        old["package_manifest_sha256"] = json!(hashes);
    } else {
        components.insert(id, component);
    }
    Ok(())
}
fn node_packages(
    node_modules: &Path,
    seen: &mut BTreeSet<PathBuf>,
    roots: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    require(
        node_modules.is_dir(),
        format!("missing installed node_modules: {}", node_modules.display()),
    )?;
    let canonical = node_modules.canonicalize()?;
    if !seen.insert(canonical.clone()) {
        return Ok(());
    }
    require(
        seen.len() <= 100_000,
        "JavaScript dependency graph exceeds limit",
    )?;
    for entry in std::fs::read_dir(&canonical)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name == ".pnpm" {
            for stored in std::fs::read_dir(&path)? {
                let nested = stored?.path().join("node_modules");
                if nested.is_dir() {
                    node_packages(&nested, seen, roots)?;
                }
            }
        } else if name.starts_with('@') && path.is_dir() {
            for scoped in std::fs::read_dir(path)? {
                let p = scoped?.path();
                if p.join("package.json").is_file() {
                    roots.insert(p.canonicalize()?);
                    if p.join("node_modules").is_dir() {
                        node_packages(&p.join("node_modules"), seen, roots)?;
                    }
                }
            }
        } else if !name.starts_with('.') && path.join("package.json").is_file() {
            roots.insert(path.canonicalize()?);
            if path.join("node_modules").is_dir() {
                node_packages(&path.join("node_modules"), seen, roots)?;
            }
        }
    }
    Ok(())
}
fn owned_path_package(root: &Path, package: &Value) -> Result<bool> {
    let manifest = Path::new(text(package, "manifest_path")?).canonicalize()?;
    let relative = manifest
        .strip_prefix(root)?
        .to_string_lossy()
        .replace('\\', "/");
    let owned = matches!(
        relative.as_str(),
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/Cargo.toml"
            | "agent-plugins/ai-quiz/native/Cargo.toml"
            | "agent-plugins/ai-quiz/native/gateway/Cargo.toml"
    );
    if owned {
        require(
            package["license"] == "Apache-2.0",
            "owned path dependency license differs from Apache-2.0",
        )?;
    }
    Ok(owned)
}
/// Inputs are existing, real locked source data. Missing source/text produces
/// review-required status or an error, never an invented permission grant.
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root().canonicalize()?;
    let metadata_path = Path::new(args.get("--cargo-metadata")?);
    let metadata = object(metadata_path)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata packages missing")?;
    let owned = if args.flag("--check-all") {
        Some(owned::check(&root)?)
    } else {
        None
    };
    let sources = Sources::load(&root)?;
    let mut components = BTreeMap::new();
    let workspace: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .map(|xs| xs.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for package in packages {
        // Workspace/platform path packages are owned. Vendored third-party
        // patches remain individual components with their original notices.
        if package["source"].is_null()
            && (workspace.contains(package["id"].as_str().unwrap_or(""))
                || owned_path_package(&root, package)?)
        {
            continue;
        }
        let name = text(package, "name")?;
        let version = text(package, "version")?;
        let manifest = Path::new(text(package, "manifest_path")?);
        let package_root = manifest
            .parent()
            .ok_or("dependency manifest has no parent")?;
        let source = if let Some(source) = package["source"].as_str() {
            source.to_owned()
        } else {
            format!(
                "path:{}",
                package_root
                    .canonicalize()?
                    .strip_prefix(&root)?
                    .to_str()
                    .ok_or("non-Unicode path dependency")?
                    .replace('\\', "/")
            )
        };
        add_component(
            &mut components,
            format!("cargo:{name}@{version}#{source}"),
            name,
            version,
            package["license"].clone(),
            package_root,
            "cargo",
            &root,
            &sources,
        )?;
    }
    let mut roots = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut scopes = vec!["web", "sdk/cli"];
    if root.join("apps/external-a2a-agent/package.json").is_file() {
        scopes.push("apps/external-a2a-agent");
    }
    for scope in &scopes {
        node_packages(
            &root.join(scope).join("node_modules"),
            &mut seen,
            &mut roots,
        )?;
    }
    for package_root in roots {
        let package = object(&package_root.join("package.json"))?;
        let name = text(&package, "name")?;
        let version = text(&package, "version")?;
        let license = package
            .get("license")
            .cloned()
            .or_else(|| package.get("licenses").cloned())
            .unwrap_or(Value::Null);
        add_component(
            &mut components,
            format!("npm:{name}@{version}"),
            name,
            version,
            license,
            &package_root,
            "npm",
            &root,
            &sources,
        )?;
    }
    require(!components.is_empty(), "dependency inventory is empty")?;
    let mut inputs = BTreeMap::new();
    for path in [
        "rust/platform-native/Cargo.lock",
        "web/pnpm-lock.yaml",
        "sdk/cli/package-lock.json",
    ] {
        let p = root.join(path);
        require(
            p.is_file(),
            format!("locked dependency input missing: {path}"),
        )?;
        inputs.insert(path, hash(&std::fs::read(p)?));
    }
    if scopes.contains(&"apps/external-a2a-agent") {
        let path = "apps/external-a2a-agent/package-lock.json";
        require(
            root.join(path).is_file(),
            "external A2A dependency lock missing",
        )?;
        inputs.insert(path, hash(&std::fs::read(root.join(path))?));
    }
    if let Some(digest) = &sources.digest {
        inputs.insert(
            "docs/security/third-party-license-sources.json",
            digest.clone(),
        );
    }
    let review = components
        .values()
        .filter(|c| c["status"] != "collected_not_approved")
        .count();
    let missing: Vec<Value> = components
        .values()
        .filter(|c| c["status"] != "collected_not_approved")
        .map(
            |c| json!({"id":c["id"],"declared_license":c["declared_license"],"status":c["status"]}),
        )
        .collect();
    let inventory = json!({"schema":"agentforge/third-party-notices/v1","license_policy":"not_evaluated; run cargo-deny and retain explicit approval records","scope":"native cargo metadata plus installed JavaScript workspace packages, including development dependencies; Runtime upstream and OS/image packages require separate distribution SBOM","javascript_scopes":scopes,"inputs":inputs,"supplemental_source_count":sources.entries.len(),"owned_declarations":owned,"components":components});
    let out = PathBuf::from(args.get("--output")?);
    require(
        !out.exists(),
        "notice output already exists; refuse to overwrite",
    )?;
    // Construct all content before creating outputs, so missing dependencies
    // cannot masquerade as a successful complete notice bundle.
    let mut notice = String::from(
        "AgentForge third-party notices\nGenerated from actual dependency metadata and installed source licenses.\nCollection is not a license-policy approval. See inventory.json for scope.\n\n",
    );
    for (id, c) in &components {
        notice.push_str(&format!(
            "=== {id} ===\nDeclared license: {}\nStatus: {}\n",
            c["declared_license"], c["status"]
        ));
        for (path, file) in c["notice_files"]
            .as_object()
            .ok_or("notice inventory invalid")?
        {
            notice.push_str(&format!(
                "--- {path} (SHA256 {}) ---\n{}\n",
                file["sha256"].as_str().unwrap_or(""),
                file["text"].as_str().unwrap_or("")
            ));
        }
        notice.push('\n');
    }
    if let Some(expected) = args.values.get("--check-bundle") {
        let expected = Path::new(expected);
        require(
            std::fs::read(expected.join("inventory.json"))?
                == (serde_json::to_string_pretty(&inventory)? + "\n").as_bytes()
                && std::fs::read(expected.join("THIRD_PARTY_NOTICES.txt"))? == notice.as_bytes(),
            "checked-in notice bundle is stale; regenerate from current locked sources",
        )?;
    }
    require(
        !args.flag("--require-zero") || review == 0,
        format!(
            "{review} components require declaration/text review; no complete bundle emitted: {}",
            serde_json::to_string(&missing)?
        ),
    )?;
    std::fs::create_dir_all(&out)?;
    std::fs::write(
        out.join("inventory.json"),
        serde_json::to_string_pretty(&inventory)? + "\n",
    )?;
    std::fs::write(out.join("THIRD_PARTY_NOTICES.txt"), notice)?;
    let requests: Vec<Value> = components
        .values()
        .filter(|c| c["status"] != "collected_not_approved")
        .cloned()
        .collect();
    std::fs::write(
        out.join("SOURCE_REQUESTS.json"),
        serde_json::to_string_pretty(
            &json!({"schema":"agentforge/notice-source-requests/v1","components":requests}),
        )? + "\n",
    )?;
    Ok(
        json!({"result":"generated_not_approved","component_count":components.len(),"missing_declaration_or_text_count":review,"review_required":missing,"output":out}),
    )
}
#[cfg(test)]
#[path = "third_party_notices/tests.rs"]
mod closure_tests;
