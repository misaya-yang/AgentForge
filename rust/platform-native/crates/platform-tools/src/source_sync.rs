//! Exact generated platform source copies; never imports or modifies upstream.
use crate::*;
use std::{collections::BTreeSet, fs};
const SOURCE: &str =
    "rust/platform-native/crates/platform-service-primitives/src/workload_identity";
const TARGET: &str = "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/shared_workload_identity";
const MANIFEST: &str = "native-source.json";
const FILES: &[&str] = &[
    "der.rs",
    "http.rs",
    "listener.rs",
    "mod.rs",
    "secrets.rs",
    "tests.rs",
    "tls.rs",
];

fn regular(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    require(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= 2 * 1024 * 1024,
        "shared source must be a bounded regular file",
    )?;
    Ok(fs::read(path)?)
}
fn directory(path: &Path, expected: &BTreeSet<String>) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    require(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "shared source directory must be regular",
    )?;
    let actual = fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            require(
                entry.file_type()?.is_file() && !entry.file_type()?.is_symlink(),
                "shared source contains a non-regular entry",
            )?;
            Ok(entry
                .file_name()
                .into_string()
                .map_err(|_| "shared source filename is not UTF-8")?)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    require(&actual == expected, "shared source file inventory changed")
}
fn inventory(files: &[&str]) -> BTreeSet<String> {
    files.iter().map(|s| (*s).to_owned()).collect()
}

fn synchronize_files(
    root: &Path,
    source_name: &str,
    target_name: &str,
    files: &[&str],
    exact_source: bool,
    apply: bool,
) -> Result<Value> {
    let source = root.join(source_name);
    let target = root.join(target_name);
    if exact_source {
        directory(&source, &inventory(files))?;
    }
    let mut bytes = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for name in files {
        let content = regular(&source.join(name))?;
        hashes.insert((*name).to_owned(), hash(&content));
        bytes.insert(*name, content);
    }
    let manifest = json!({"schema":"agentforge/generated-platform-source/v1","source":source_name,"files_sha256":hashes});
    if target.exists() {
        let mut names = inventory(files);
        names.insert(MANIFEST.into());
        directory(&target, &names)?;
        let previous: Value = strict_json(&regular(&target.join(MANIFEST))?)?;
        require(
            previous["schema"] == manifest["schema"] && previous["source"] == source_name,
            "shared source provenance differs",
        )?;
        let recorded = previous["files_sha256"]
            .as_object()
            .ok_or("shared source hash inventory missing")?;
        require(
            recorded.keys().cloned().collect::<BTreeSet<_>>() == inventory(files),
            "shared source hash inventory differs",
        )?;
        for name in files {
            let actual = hash(&regular(&target.join(name))?);
            require(
                recorded[*name] == actual || hashes[*name] == actual,
                "generated shared source has independent edits; preserve and resolve them before synchronization",
            )?;
            if !apply {
                require(hashes[*name] == actual, "generated shared source is stale")?;
            }
        }
        if !apply {
            require(
                previous == manifest,
                "generated shared source provenance is stale",
            )?;
        }
    } else {
        require(
            apply && fs::symlink_metadata(&target).is_err(),
            "generated shared source is missing",
        )?;
        fs::create_dir(&target)?;
    }
    if apply {
        for (name, content) in bytes {
            fs::write(target.join(name), content)?;
        }
        receipt_output::write(&target.join(MANIFEST), &manifest)?;
    }
    Ok(
        json!({"result":"pass","source":source_name,"target":target_name,"file_count":files.len(),"source_sha256":hash(&serde_json::to_vec(&manifest)?),"applied":apply,"upstream_changed":false}),
    )
}

pub fn synchronize(root: &Path, apply: bool) -> Result<Value> {
    let mut result = synchronize_files(root, SOURCE, TARGET, FILES, true, apply)?;
    let adapter = "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/database_connection.rs";
    if root.join(adapter).exists() {
        result["data_connection"] = synchronize_files(
            root,
            "rust/platform-native/crates/platform-service-primitives/src/data_connection",
            "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/shared_data_connection",
            &["config.rs"],
            false,
            apply,
        )?;
    }
    Ok(result)
}

/// Historical source fixtures without the platform transport remain valid.
/// Any current overlay containing the transport must carry its complete inputs.
pub fn verify_if_present(root: &Path) -> Result<()> {
    crate::plugin_source::verify_if_present(root)?;
    if ["workload_identity.rs", "database_connection.rs"]
        .iter()
        .any(|name| {
            root.join("rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src")
                .join(name)
                .exists()
        })
    {
        synchronize(root, false)?;
    }
    Ok(())
}

pub fn run(args: &Args) -> Result<Value> {
    require(
        args.positionals.len() == 2 && args.flags.iter().all(|f| f == "--apply"),
        "unexpected shared-source argument",
    )?;
    for (name, values) in &args.multiple {
        require(
            name == "--repo-root" && values.len() == 1,
            "unexpected shared-source option",
        )?;
    }
    synchronize(&args.root(), args.flag("--apply"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("workload-source-sync-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(root.join(SOURCE)).unwrap();
            fs::create_dir_all(root.join(TARGET).parent().unwrap()).unwrap();
            for name in FILES {
                fs::write(
                    root.join(SOURCE).join(name),
                    format!("// canonical {name}\n"),
                )
                .unwrap();
            }
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn data_adapter_requires_exact_current_config_even_when_other_data_sources_change() {
        let root = Fixture::new();
        let contract = root
            .0
            .join("rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src");
        fs::write(
            contract.join("database_connection.rs"),
            "// includes shared Data config",
        )
        .unwrap();
        let source = root
            .0
            .join("rust/platform-native/crates/platform-service-primitives/src/data_connection");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("config.rs"), "// canonical Data roles and TLS").unwrap();
        fs::write(source.join("append.rs"), "// separate Data domain input").unwrap();
        assert!(synchronize(&root.0, false).is_err());
        synchronize(&root.0, true).unwrap();
        verify_if_present(&root.0).unwrap();
        fs::write(
            source.join("append.rs"),
            "// unrelated append implementation",
        )
        .unwrap();
        verify_if_present(&root.0).unwrap();
        fs::write(
            source.join("config.rs"),
            "// changed authoritative Data config",
        )
        .unwrap();
        assert!(verify_if_present(&root.0).is_err());
        synchronize(&root.0, true).unwrap();
        let target = contract.join("shared_data_connection/config.rs");
        fs::write(&target, "// independent edit").unwrap();
        assert!(synchronize(&root.0, true).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "// independent edit");
    }
    #[test]
    fn detects_canonical_drift_then_refreshes_only_generated_files() {
        let root = Fixture::new();
        assert!(synchronize(&root.0, false).is_err());
        synchronize(&root.0, true).unwrap();
        synchronize(&root.0, false).unwrap();
        fs::write(root.0.join(SOURCE).join("tls.rs"), "// canonical change\n").unwrap();
        assert!(synchronize(&root.0, false).is_err());
        synchronize(&root.0, true).unwrap();
        synchronize(&root.0, false).unwrap();
        fs::write(root.0.join(TARGET).join("tls.rs"), "// independent work\n").unwrap();
        assert!(synchronize(&root.0, true).is_err());
        assert_eq!(
            fs::read_to_string(root.0.join(TARGET).join("tls.rs")).unwrap(),
            "// independent work\n"
        );
    }
    #[test]
    fn refuses_unregistered_inputs_and_outputs() {
        let root = Fixture::new();
        synchronize(&root.0, true).unwrap();
        fs::write(root.0.join(TARGET).join("unexpected.rs"), "preserved").unwrap();
        assert!(synchronize(&root.0, true).is_err());
        fs::remove_file(root.0.join(TARGET).join("unexpected.rs")).unwrap();
        fs::write(root.0.join(SOURCE).join("unregistered.rs"), "source").unwrap();
        assert!(synchronize(&root.0, true).is_err());
    }
}
