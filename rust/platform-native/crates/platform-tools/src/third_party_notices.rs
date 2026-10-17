//! Deterministic dependency SBOM/notice inventory; never approves a license.
// SPDX-License-Identifier: Apache-2.0
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing dependency {key}").into())
}
fn notice_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let extension = upper.rsplit('.').next().unwrap_or("");
    if [
        "RS", "C", "CPP", "H", "JS", "TS", "TSX", "PY", "WASM", "MAP",
    ]
    .contains(&extension)
    {
        return false;
    }
    upper.contains("LICENSE")
        || upper.contains("LICENCE")
        || ["NOTICE", "COPYING", "COPYRIGHT", "OFL", "UFL", "AUTHORS"]
            .iter()
            .any(|prefix| {
                upper == *prefix
                    || upper.starts_with(&format!("{prefix}."))
                    || upper.starts_with(&format!("{prefix}-"))
            })
}
fn notice_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    files: &mut BTreeMap<String, Value>,
) -> Result<()> {
    require(depth <= 32, "dependency notice tree exceeds depth limit")?;
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let ty = entry.file_type()?;
        let path = entry.path();
        // Never traverse dependency symlinks or nested dependency graphs here.
        if ty.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if ty.is_dir() && !matches!(name.as_str(), "node_modules" | ".git" | "target") {
            notice_files(root, &path, depth + 1, files)?;
        } else if ty.is_file() && notice_name(&name) {
            require(
                entry.metadata()?.len() <= 4 * 1024 * 1024,
                "dependency notice exceeds 4MiB",
            )?;
            let raw = std::fs::read(&path)?;
            let content = std::str::from_utf8(&raw)
                .map_err(|_| "dependency notice is not UTF-8; review required")?;
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-Unicode dependency notice path")?
                .replace('\\', "/");
            files.insert(relative, json!({"sha256":hash(&raw),"text":content}));
        }
    }
    Ok(())
}
fn add_component(
    components: &mut BTreeMap<String, Value>,
    id: String,
    name: &str,
    version: &str,
    license: Value,
    root: &Path,
    scope: &str,
) -> Result<()> {
    let mut files = BTreeMap::new();
    notice_files(root, root, 0, &mut files)?;
    let status = if license.is_null() || files.is_empty() {
        "review_required_missing_declaration_or_text"
    } else {
        "collected_not_approved"
    };
    let component = json!({"id":id,"name":name,"version":version,"ecosystem":scope,"declared_license":license,"notice_files":files,"status":status});
    if let Some(old) = components.insert(id, component.clone()) {
        require(
            old == component,
            "same package identity has conflicting license contents",
        )?;
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
/// Inputs are existing, real locked source data. Missing source/text produces
/// review-required status or an error, never an invented permission grant.
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root().canonicalize()?;
    let metadata_path = Path::new(args.get("--cargo-metadata")?);
    let metadata = object(metadata_path)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata packages missing")?;
    let mut components = BTreeMap::new();
    for package in packages {
        // Workspace/path packages are owned by the project or handled through
        // their upstream NOTICE. Registry/Git packages keep individual terms.
        if package["source"].is_null() {
            continue;
        }
        let name = text(package, "name")?;
        let version = text(package, "version")?;
        let manifest = Path::new(text(package, "manifest_path")?);
        let package_root = manifest
            .parent()
            .ok_or("dependency manifest has no parent")?;
        add_component(
            &mut components,
            format!("cargo:{}@{}#{}", name, version, text(package, "source")?),
            name,
            version,
            package["license"].clone(),
            package_root,
            "cargo",
        )?;
    }
    let mut roots = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for scope in ["web", "sdk/cli"] {
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
    let review = components
        .values()
        .filter(|c| c["status"] != "collected_not_approved")
        .count();
    let inventory = json!({"schema":"agentforge/third-party-notices/v1","license_policy":"not_evaluated; run cargo-deny and retain explicit approval records","scope":"native cargo metadata plus installed web and CLI packages, including development dependencies; Runtime upstream and OS/image packages require separate distribution SBOM","inputs":inputs,"components":components});
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
    std::fs::create_dir_all(&out)?;
    std::fs::write(
        out.join("inventory.json"),
        serde_json::to_string_pretty(&inventory)? + "\n",
    )?;
    std::fs::write(out.join("THIRD_PARTY_NOTICES.txt"), notice)?;
    Ok(
        json!({"result":"generated_not_approved","component_count":components.len(),"missing_declaration_or_text_count":review,"output":out}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_license_material_without_matching_implementation_files() {
        for name in [
            "LICENSE",
            "LICENSE-MIT",
            "LICENSE.Apache-2.0",
            "NOTICE.md",
            "OFL.txt",
            "COPYING.LESSER",
            "UFL.txt",
            "emoji-icon-font-mit-license.txt",
        ] {
            assert!(notice_name(name));
        }
        for name in ["license.rs", "main.rs", "package.json", "node_modules"] {
            assert!(!notice_name(name));
        }
    }
}

#[cfg(test)]
mod deterministic_tests {
    use super::*;
    #[test]
    fn notice_inventory_is_deterministic_and_never_approves_unknown_terms() {
        let root =
            std::env::temp_dir().join(format!("agentforge-notice-test-{}", uuid::Uuid::new_v4()));
        for path in [
            "rust/platform-native",
            "web/node_modules/demo",
            "sdk/cli/node_modules/demo",
        ] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        for path in [
            "rust/platform-native/Cargo.lock",
            "web/pnpm-lock.yaml",
            "sdk/cli/package-lock.json",
        ] {
            std::fs::write(root.join(path), b"fixture lock").unwrap();
        }
        for scope in ["web", "sdk/cli"] {
            let package = root.join(scope).join("node_modules/demo");
            std::fs::write(
                package.join("package.json"),
                r#"{"name":"demo","version":"1.0.0","license":"LicenseRef-Needs-Review"}"#,
            )
            .unwrap();
            std::fs::write(package.join("LICENSE"), "Third-party rights retained.\n").unwrap();
        }
        let metadata = root.join("cargo.json");
        std::fs::write(&metadata, r#"{"packages":[]}"#).unwrap();
        let mut args = Args::default();
        args.values
            .insert("--repo-root".into(), root.to_string_lossy().into_owned());
        args.values.insert(
            "--cargo-metadata".into(),
            metadata.to_string_lossy().into_owned(),
        );
        for output in ["one", "two"] {
            args.values.insert(
                "--output".into(),
                root.join(output).to_string_lossy().into_owned(),
            );
            assert_eq!(run(&args).unwrap()["result"], "generated_not_approved");
        }
        for name in ["inventory.json", "THIRD_PARTY_NOTICES.txt"] {
            assert_eq!(
                std::fs::read(root.join("one").join(name)).unwrap(),
                std::fs::read(root.join("two").join(name)).unwrap()
            );
        }
        let inventory = object(&root.join("one/inventory.json")).unwrap();
        assert_eq!(
            inventory["components"]["npm:demo@1.0.0"]["status"],
            "collected_not_approved"
        );
        assert!(run(&args).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
