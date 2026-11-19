//! Explicit language-migration evidence for the seven retired freeze producers.
//! This descriptor is compiled in; runtime files cannot expand the exception.
use crate::{
    AuthorityError, Result,
    manifest::{BaselineManifest, file_sha256},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
    process::Command,
};

#[derive(Deserialize)]
struct Descriptor {
    schema: String,
    baseline_id: String,
    baseline_manifest_sha256: String,
    source_git_sha: String,
    retired: Vec<Retired>,
}
#[derive(Deserialize)]
struct Retired {
    path: String,
    historical_sha256: String,
    replacements: BTreeMap<String, String>,
}
fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !path.split('/').any(|c| c.is_empty() || c == ".")
}
pub(crate) fn verified_retired_paths(
    path: &Path,
    baseline: &BaselineManifest,
    root: &Path,
) -> Result<BTreeSet<String>> {
    let descriptor: Descriptor = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../database/authority/native-producer-migration-v1.json"
    )))
    .map_err(|_| refused("invalid compiled producer migration descriptor"))?;
    if descriptor.schema != "native-authority-producer-migration/v1"
        || descriptor.baseline_id != baseline.baseline_id
        || descriptor.source_git_sha != baseline.source_git_sha
        || descriptor.baseline_manifest_sha256 != file_sha256(path)?
    {
        return Ok(BTreeSet::new());
    }
    verify(&descriptor, root)
}
fn verify(descriptor: &Descriptor, root: &Path) -> Result<BTreeSet<String>> {
    let root = root.canonicalize()?;
    let mut allowed = BTreeSet::new();
    for retired in &descriptor.retired {
        if !safe(&retired.path) || retired.replacements.is_empty() {
            return Err(refused("unsafe producer migration descriptor"));
        }
        // Present files (including symlinks) are always governed by the original Git diff.
        match std::fs::symlink_metadata(root.join(&retired.path)) {
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let blob = Command::new("git")
            .args([
                "show",
                &format!("{}:{}", descriptor.source_git_sha, retired.path),
            ])
            .current_dir(&root)
            .output()?;
        if !blob.status.success()
            || format!("{:x}", Sha256::digest(&blob.stdout)) != retired.historical_sha256
        {
            return Err(refused(
                "retired producer historical Git blob is unavailable or differs",
            ));
        }
        for (replacement, expected) in &retired.replacements {
            if !safe(replacement) {
                return Err(refused("unsafe native replacement path"));
            }
            let native = root.join(replacement).canonicalize()?;
            if !native.starts_with(&root) || file_sha256(&native)? != *expected {
                return Err(refused(
                    "native producer replacement differs from migration descriptor",
                ));
            }
        }
        allowed.insert(retired.path.clone());
    }
    Ok(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().into()
    }
    #[test]
    fn compiled_retirement_allowlist_binds_exact_baseline_and_native_sources() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let d: Descriptor = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../database/authority/native-producer-migration-v1.json"
        )))
        .unwrap();
        assert_eq!(d.baseline_id, "2026_08_post_kb_v1");
        assert_eq!(d.source_git_sha, "386344c6c15e92547cabe32b07fad304afbf4a98");
        assert_eq!(
            d.baseline_manifest_sha256,
            file_sha256(&root.join("database/baselines/2026_08_post_kb_v1/manifest.json")).unwrap()
        );
        let expected = BTreeSet::from([
            "database/authority/fingerprint.py",
            "database/authority/fingerprint_catalog.py",
            "database/authority/fingerprint_values.py",
            "scripts/inventory/database_policy.py",
            "scripts/inventory/generate_database_grants.py",
            "scripts/database/render_baseline_contract.py",
            "scripts/database/freeze_baseline.py",
        ]);
        assert_eq!(
            d.retired
                .iter()
                .map(|r| r.path.as_str())
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert_eq!(d.retired.len(), 7);
        for retired in d.retired {
            assert_eq!(retired.historical_sha256.len(), 64);
            for (path, hash) in retired.replacements {
                assert_eq!(file_sha256(&root.join(path)).unwrap(), hash);
            }
        }
    }
    #[test]
    fn retired_producer_requires_historical_bytes_and_current_native_identity() {
        let root = std::env::temp_dir().join(format!(
            "producer-provenance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        std::fs::write(root.join("old.py"), b"historical producer").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "source"]);
        let source = git(&root, &["rev-parse", "HEAD"]);
        let mut descriptor = Descriptor {
            schema: "fixture".into(),
            baseline_id: "fixture".into(),
            baseline_manifest_sha256: String::new(),
            source_git_sha: source.clone(),
            retired: vec![Retired {
                path: "old.py".into(),
                historical_sha256: file_sha256(&root.join("old.py")).unwrap(),
                replacements: BTreeMap::from([(
                    "native.rs".into(),
                    format!("{:x}", Sha256::digest(b"native producer")),
                )]),
            }],
        };
        std::fs::remove_file(root.join("old.py")).unwrap();
        std::fs::write(root.join("native.rs"), b"native producer").unwrap();
        assert_eq!(
            verify(&descriptor, &root).unwrap(),
            BTreeSet::from(["old.py".into()])
        );
        std::fs::write(root.join("native.rs"), b"modified").unwrap();
        assert!(verify(&descriptor, &root).is_err());
        std::fs::write(root.join("native.rs"), b"native producer").unwrap();
        descriptor.source_git_sha = "0000000000000000000000000000000000000000".into();
        assert!(verify(&descriptor, &root).is_err());
        descriptor.source_git_sha = source;
        descriptor.retired[0].historical_sha256 = "0".repeat(64);
        assert!(verify(&descriptor, &root).is_err());
        std::fs::write(root.join("old.py"), b"changed present producer").unwrap();
        assert!(verify(&descriptor, &root).unwrap().is_empty());
        assert!(
            !Command::new("git")
                .args(["diff", "--quiet", "HEAD", "--", "old.py"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
