//! Strict self-repository references and validated compatibility copies for actionlint.
use std::fs;
use std::path::{Component, Path, PathBuf};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn no_symlinks(root: &Path, target: &Path) -> Result<()> {
    let relative = target.strip_prefix(root)?;
    let mut path = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err("non-normal self-reference component".into());
        }
        path.push(component);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err("symlink self-repository target refused".into());
        }
    }
    Ok(())
}

pub fn validate_ref(root: &Path, value: &str, workflow: bool) -> Result<String> {
    let relative = value
        .strip_prefix("$/")
        .ok_or("self-reference requires $/")?;
    let valid = if workflow {
        relative
            .strip_prefix(".github/workflows/")
            .is_some_and(|v| {
                v.strip_suffix(".yml")
                    .or_else(|| v.strip_suffix(".yaml"))
                    .is_some_and(name)
            })
    } else {
        relative.strip_prefix(".github/actions/").is_some_and(name)
    };
    if !valid {
        return Err(format!("invalid self-repository reference or target kind: {value:?}").into());
    }
    let root = root.canonicalize()?;
    let target = root.join(relative);
    no_symlinks(&root, &target)?;
    let exists = if workflow {
        target.is_file()
    } else {
        ["action.yml", "action.yaml"].iter().any(|file| {
            let metadata = target.join(file);
            metadata.is_file() && no_symlinks(&root, &metadata).is_ok()
        })
    };
    if !exists {
        return Err("missing regular self-repository target".into());
    }
    Ok(format!("./{relative}"))
}

pub fn validate_workflow(root: &Path, text: &str) -> Result<String> {
    let mut result = String::new();
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content.contains("uses:") && content.contains("./.github/") {
            return Err("local uses must use immutable running-commit $/ syntax".into());
        }
        if content.contains("uses:") && content.contains("$/") {
            let (prefix, value, workflow) = if let Some(v) = content.strip_prefix("    uses: ") {
                ("    uses: ", v, true)
            } else if let Some(v) = content.strip_prefix("      - uses: ") {
                ("      - uses: ", v, false)
            } else {
                return Err("unsupported self-reference layout".into());
            };
            let translated = validate_ref(root, value, workflow)?;
            result.push_str(prefix);
            result.push_str(&translated);
            result.push_str(&line[content.len()..]);
        } else {
            result.push_str(line);
        }
    }
    Ok(result)
}

fn regular_files(root: &Path, path: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
    no_symlinks(root, path)?;
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            regular_files(root, &entry?.path(), result)?;
        }
    } else if path.is_file() {
        result.push(path.to_path_buf());
    } else {
        return Err("non-regular .github entry refused".into());
    }
    Ok(())
}

/// Validate everything before creating a new, external compatibility copy.
/// Checked-in workflows are never rewritten; existing destinations are refused.
pub fn check(root: &Path, output: Option<&Path>) -> Result<usize> {
    let root = root.canonicalize()?;
    let mut files = Vec::new();
    regular_files(&root, &root.join(".github"), &mut files)?;
    let mut translated = Vec::new();
    for path in &files {
        if path.parent() == Some(root.join(".github/workflows").as_path())
            && matches!(
                path.extension().and_then(|v| v.to_str()),
                Some("yml" | "yaml")
            )
        {
            translated.push((
                path.clone(),
                validate_workflow(&root, &fs::read_to_string(path)?)?,
            ));
        }
    }
    if translated.is_empty() {
        return Err("no workflow inputs".into());
    }
    if let Some(output) = output {
        if fs::symlink_metadata(output).is_ok() {
            return Err("actionlint copy destination must not exist".into());
        }
        let parent = output
            .parent()
            .ok_or("copy destination parent missing")?
            .canonicalize()?;
        if parent.starts_with(&root) || !output.is_absolute() {
            return Err("actionlint copy must be an absolute external path".into());
        }
        fs::create_dir(output)?;
        for path in files {
            let dest = output.join(path.strip_prefix(&root)?);
            fs::create_dir_all(dest.parent().ok_or("copy parent missing")?)?;
            if let Some((_, text)) = translated.iter().find(|(p, _)| p == &path) {
                fs::write(dest, text)?;
            } else {
                fs::copy(path, dest)?;
            }
        }
    }
    Ok(translated.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "workflow-self-ref-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join(".github/actions/setup-native")).unwrap();
            fs::create_dir_all(root.join(".github/workflows")).unwrap();
            fs::write(
                root.join(".github/actions/setup-native/action.yml"),
                "name: fixture",
            )
            .unwrap();
            fs::write(root.join(".github/workflows/verify.yml"), "name: fixture").unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn valid_action_and_workflow() {
        let f = Fixture::new();
        assert_eq!(
            validate_ref(&f.0, "$/.github/actions/setup-native", false).unwrap(),
            "./.github/actions/setup-native"
        );
        assert_eq!(
            validate_ref(&f.0, "$/.github/workflows/verify.yml", true).unwrap(),
            "./.github/workflows/verify.yml"
        );
    }
    #[test]
    fn bad_refs() {
        let f = Fixture::new();
        for value in [
            "$/.github/actions/setup-native@main",
            "$/.github/actions/../setup-native",
            "$/.github/actions/missing",
            "$/.github/actions/setup-native\nrun: evil",
            "$/other/setup-native",
            "./.github/actions/setup-native",
        ] {
            assert!(validate_ref(&f.0, value, false).is_err(), "{value}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlink_target_and_metadata() {
        let f = Fixture::new();
        let metadata = f.0.join(".github/actions/setup-native/action.yml");
        fs::remove_file(&metadata).unwrap();
        std::os::unix::fs::symlink(f.0.join(".github/workflows/verify.yml"), metadata).unwrap();
        assert!(validate_ref(&f.0, "$/.github/actions/setup-native", false).is_err());
        std::os::unix::fs::symlink(
            f.0.join(".github/actions/setup-native"),
            f.0.join(".github/actions/linked"),
        )
        .unwrap();
        assert!(validate_ref(&f.0, "$/.github/actions/linked", false).is_err());
    }
    #[test]
    fn kind_and_layout() {
        let f = Fixture::new();
        assert!(validate_ref(&f.0, "$/.github/workflows/verify.yml", false).is_err());
        for text in [
            "      - uses: \"$/.github/actions/setup-native\"",
            "      - uses: ./.github/actions/setup-native",
            "    - uses: $/.github/actions/setup-native",
        ] {
            assert!(validate_workflow(&f.0, text).is_err());
        }
        assert_eq!(
            validate_workflow(&f.0, "      - uses: $/.github/actions/setup-native\n").unwrap(),
            "      - uses: ./.github/actions/setup-native\n"
        );
    }
    #[test]
    fn compatibility_copy_is_external_new_and_validated_before_writing() {
        let f = Fixture::new();
        let output = f.0.with_extension("lint-copy");
        let internal = f.0.join("lint-copy");
        assert!(check(&f.0, Some(&internal)).is_err());
        fs::write(
            f.0.join(".github/workflows/verify.yml"),
            "      - uses: $/.github/actions/missing\n",
        )
        .unwrap();
        assert!(check(&f.0, Some(&output)).is_err());
        assert!(!output.exists());
        fs::write(
            f.0.join(".github/workflows/verify.yml"),
            "      - uses: $/.github/actions/setup-native\n",
        )
        .unwrap();
        assert_eq!(check(&f.0, Some(&output)).unwrap(), 1);
        assert!(
            fs::read_to_string(output.join(".github/workflows/verify.yml"))
                .unwrap()
                .contains("uses: ./")
        );
        assert!(fs::read_to_string(f.0.join(".github/workflows/verify.yml"))
            .unwrap()
            .contains("uses: $/"));
        assert!(check(&f.0, Some(&output)).is_err());
        fs::remove_dir_all(output).unwrap();
    }
}
