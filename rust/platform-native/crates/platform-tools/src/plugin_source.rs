//! Package canonical compiled plugins into the independently composed workspace.
use crate::*;
use std::{collections::BTreeSet, fs};

const OVERLAY: &str = "rust/agent-runtime-overlay/kernel-rs";
const PLUGIN: &str = "agent-plugins/ai-quiz/native";
const COMPAT: &str = "rust/platform-native/crates/platform-python-compat";
const PLUGIN_FILES: &[&str] = &[
    "Cargo.toml",
    "capability.json",
    "src/lib.rs",
    "src/quiz_grading.rs",
    "src/executor.rs",
];
const COMPAT_FILES: &[&str] = &["Cargo.toml", "src/lib.rs", "src/unicode.rs"];
const RECEIPT: &str = "platform-source.json";

fn bytes(path: &Path) -> Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    require(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= 2 * 1024 * 1024,
        "plugin input must be a bounded regular file",
    )?;
    Ok(fs::read(path)?)
}
fn replace_once(text: &mut String, old: &str, new: &str) -> Result<()> {
    require(
        text.matches(old).count() == 1,
        "plugin manifest source binding changed",
    )?;
    *text = text.replacen(old, new, 1);
    Ok(())
}
fn manifest(source: &[u8], plugin: bool) -> Result<Vec<u8>> {
    let mut text = String::from_utf8(source.to_vec())?;
    if plugin {
        replace_once(
            &mut text,
            "\n[patch.crates-io]\nopenidconnect = { path = \"../../../rust/platform-native/vendor/openidconnect-4.0.1\" }\n",
            "",
        )?;
        replace_once(
            &mut text,
            "[workspace]\nmembers = [\"gateway\"]\nresolver = \"2\"\n",
            "",
        )?;
        replace_once(
            &mut text,
            "path = \"../../../rust/platform-native/crates/platform-python-compat\"",
            "path = \"../ai-platform-python-compat\"",
        )?;
        replace_once(
            &mut text,
            "path = \"../../../rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract\"",
            "path = \"../ai-platform-capability-contract\"",
        )?;
        replace_once(
            &mut text,
            "sqlx = { version = \"0.8\", default-features = false, features = [\"runtime-tokio-rustls\",\"postgres\",\"chrono\",\"json\",\"uuid\"], optional = true }",
            "sqlx = { workspace = true, features = [\"postgres\",\"chrono\",\"json\",\"uuid\"], optional = true }",
        )?;
    }
    Ok(text.into_bytes())
}
fn package(root: &Path, source: &str, name: &str, files: &[&str], apply: bool) -> Result<Value> {
    let source_root = safe(root, source)?;
    let target = root.join(OVERLAY).join(name);
    let mut inputs = BTreeMap::new();
    let mut outputs = BTreeMap::new();
    let mut payload = BTreeMap::new();
    for relative in files {
        let input = bytes(&safe(&source_root, relative)?)?;
        let output = if *relative == "Cargo.toml" {
            manifest(&input, source == PLUGIN)?
        } else {
            input.clone()
        };
        inputs.insert(*relative, hash(&input));
        outputs.insert(*relative, hash(&output));
        payload.insert(*relative, output);
    }
    let expected = json!({"schema":"agentforge/composed-plugin-source/v1","source":source,
        "source_files_sha256":inputs,"composed_files_sha256":outputs,
        "manifest_binding":"sibling platform dependencies; executor uses composed workspace SQLx"});
    if target.exists() {
        let metadata = fs::symlink_metadata(&target)?;
        require(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "plugin target directory must be regular",
        )?;
        let actual: BTreeSet<_> = walk(&target, &[], true)?
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let names: BTreeSet<_> = files
            .iter()
            .copied()
            .chain([RECEIPT])
            .map(str::to_owned)
            .collect();
        require(actual == names, "generated plugin source inventory changed")?;
        let previous: Value = strict_json(&bytes(&safe(&target, RECEIPT)?)?)?;
        require(
            previous["schema"] == expected["schema"] && previous["source"] == source,
            "generated plugin provenance differs",
        )?;
        let recorded = previous["composed_files_sha256"]
            .as_object()
            .ok_or("plugin hashes missing")?;
        require(
            recorded.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == files.iter().copied().collect(),
            "generated plugin hash inventory differs",
        )?;
        for relative in files {
            let digest = hash(&bytes(&safe(&target, relative)?)?);
            require(
                recorded[*relative] == digest || outputs[relative] == digest,
                "generated plugin contains independent edits; preserve them before synchronization",
            )?;
            if !apply {
                require(
                    outputs[relative] == digest,
                    "generated plugin source is stale",
                )?;
            }
        }
        if !apply {
            require(previous == expected, "generated plugin provenance is stale")?;
        }
    } else {
        require(
            apply && fs::symlink_metadata(&target).is_err(),
            "generated plugin source missing",
        )?;
        fs::create_dir(&target)?;
    }
    if apply {
        for (relative, content) in payload {
            let destination = target.join(relative);
            let parent = destination.parent().ok_or("plugin source parent missing")?;
            match fs::symlink_metadata(parent) {
                Ok(metadata) => require(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    format!(
                        "plugin source payload parent is not a regular directory: {}",
                        parent.display()
                    ),
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir_all(parent).map_err(|error| {
                        format!(
                            "plugin source create payload directory {}: {error}",
                            parent.display()
                        )
                    })?
                }
                Err(error) => {
                    return Err(format!(
                        "plugin source inspect payload directory {}: {error}",
                        parent.display()
                    )
                    .into());
                }
            }
            fs::write(&destination, content).map_err(|error| {
                format!(
                    "plugin source write payload {}: {error}",
                    destination.display()
                )
            })?;
        }
        receipt_output::write(&target.join(RECEIPT), &expected).map_err(|error| {
            format!(
                "plugin source publish receipt {}: {error}",
                target.join(RECEIPT).display()
            )
        })?;
    }
    Ok(
        json!({"crate":name,"files":files.len(),"source_manifest_sha256":hash(&serde_json::to_vec(&expected)?)}),
    )
}
pub fn synchronize(root: &Path, apply: bool) -> Result<Value> {
    Ok(
        json!({"result":"pass","upstream_changed":false,"applied":apply,"packages":[
            package(root, COMPAT, "ai-platform-python-compat", COMPAT_FILES, apply)?,
            package(root, PLUGIN, "ai-platform-plugin-quiz", PLUGIN_FILES, apply)?
        ]}),
    )
}
pub fn verify_if_present(root: &Path) -> Result<()> {
    if root.join(PLUGIN).exists() {
        synchronize(root, false)?;
    }
    Ok(())
}
pub fn run(args: &Args) -> Result<Value> {
    require(
        args.positionals.len() == 2 && args.flags.iter().all(|f| f == "--apply"),
        "unexpected plugin sync argument",
    )?;
    for (name, values) in &args.multiple {
        require(
            name == "--repo-root" && values.len() == 1,
            "unexpected plugin sync option",
        )?;
    }
    synchronize(&args.root(), args.flag("--apply"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executor_uses_one_sqlx_and_no_gateway_or_checkout_paths() {
        let source = include_bytes!("../../../../../agent-plugins/ai-quiz/native/Cargo.toml");
        let output = String::from_utf8(manifest(source, true).unwrap()).unwrap();
        assert!(output.contains("sqlx = { workspace = true"));
        assert!(output.contains("path = \"../ai-platform-capability-contract\""));
        assert!(output.contains("path = \"../ai-platform-python-compat\""));
        assert!(!output.contains("../../../"));
        assert!(!output.contains("gateway"));
        assert!(manifest(b"sqlx = {}", true).is_err());
    }
    #[test]
    fn packaging_detects_source_drift_and_preserves_independent_edits() {
        let root = std::env::temp_dir().join(format!("plugin-source-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(OVERLAY)).unwrap();
        for (source, files) in [(PLUGIN, PLUGIN_FILES), (COMPAT, COMPAT_FILES)] {
            for relative in files {
                let path = root.join(source).join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let content = if *relative == "Cargo.toml" && source == PLUGIN {
                    include_bytes!("../../../../../agent-plugins/ai-quiz/native/Cargo.toml")
                        .to_vec()
                } else {
                    b"// source".to_vec()
                };
                fs::write(path, content).unwrap();
            }
        }
        assert!(synchronize(&root, false).is_err());
        synchronize(&root, true).unwrap();
        synchronize(&root, false).unwrap();
        fs::write(root.join(PLUGIN).join("src/lib.rs"), b"// updated").unwrap();
        assert!(synchronize(&root, false).is_err());
        synchronize(&root, true).unwrap();
        let target = root
            .join(OVERLAY)
            .join("ai-platform-plugin-quiz/src/lib.rs");
        fs::write(&target, b"// independent edit").unwrap();
        assert!(synchronize(&root, true).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"// independent edit");
        fs::remove_dir_all(root).unwrap();
    }
}
