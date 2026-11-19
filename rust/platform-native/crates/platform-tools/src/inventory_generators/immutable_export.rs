//! Explicit historical export; deliberately independent of clean-HEAD refresh.
use super::*;
use std::io::Write;

pub(super) fn run(args: &Args) -> Result<Value> {
    require(
        !args.flag("--write") && !args.flag("--verify"),
        "immutable export does not accept --write or --verify",
    )?;
    require(
        args.values
            .get("--kind")
            .is_none_or(|k| k == "loc-baseline"),
        "immutable export only supports loc-baseline",
    )?;
    export(
        &args.root(),
        args.get("--source-rev")?,
        args.get("--source-tree")?,
        Path::new(args.get("--output-dir")?),
    )
}

fn export(root: &Path, source: &str, tree: &str, output: &Path) -> Result<Value> {
    require(
        valid_sha(source) && valid_sha(tree),
        "source revision and tree must be exact full lowercase SHA",
    )?;
    let root = root.canonicalize()?;
    let repository =
        PathBuf::from(git_text(&root, &["rev-parse", "--show-toplevel"])?.trim()).canonicalize()?;
    // Canonicalize the existing parent, so symlink aliases cannot redirect into
    // the repository. Only a brand-new directory is accepted, never a refresh.
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    let name = output
        .file_name()
        .ok_or("output must name a new directory")?;
    require(name != "." && name != "..", "invalid output directory")?;
    let output = output
        .parent()
        .ok_or("output parent absent")?
        .canonicalize()?
        .join(name);
    require(
        !output.starts_with(&repository) && !output.starts_with(&root),
        "immutable export destination must be external to the repository",
    )?;
    require(
        !output.try_exists()? && std::fs::symlink_metadata(&output).is_err(),
        "immutable export never overwrites an existing destination",
    )?;
    let head = git_text(&root, &["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_owned();
    require(valid_sha(&head), "invalid Git HEAD")?;
    require(
        source != head,
        "immutable historical export requires a strict ancestor, not current HEAD",
    )?;
    git(&root, &["merge-base", "--is-ancestor", source, &head])?;
    let snapshot = Snapshot::read(&root, source)?;
    require(
        snapshot.tree == tree,
        "source tree does not match requested immutable tree",
    )?;
    let baseline = serialize(&facts::loc(&snapshot)?)?;
    let evidence = json!({"schema":"ai-platform/immutable-loc-export/v1", "baseline_id":ID,
        "source_revision":source,"source_tree":tree,"ancestor_of":head,"ancestor_proof":"git merge-base --is-ancestor",
        "snapshot_reader":"Snapshot::read/git blobs", "working_tree_used":false,
        "outputs":[{"file":"loc-baseline.json","sha256":hash(&baseline)}]});
    let evidence_bytes = serialize(&evidence)?;
    require(
        git_text(&root, &["rev-parse", "--verify", "HEAD"])?.trim() == head,
        "HEAD changed during immutable export",
    )?;
    // Atomic mkdir is the no-clobber claim. Never create missing parent trees,
    // overwrite files, or delete a destination after a partial I/O failure.
    std::fs::create_dir(&output)?;
    for (name, bytes) in [
        ("loc-baseline.json", baseline),
        ("export-evidence.json", evidence_bytes),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join(name))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(
        json!({"result":"pass","operation":"immutable-loc-export","output_dir":output,"evidence":evidence}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        sha: String,
        tree: String,
    }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "immutable-loc-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = base.join("repo");
            std::fs::create_dir_all(root.join("rust")).unwrap();
            std::fs::create_dir_all(root.join("web")).unwrap();
            git(&root, &["init", "-q"]).unwrap();
            git(&root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
            git(&root, &["config", "user.name", "Fixture"]).unwrap();
            std::fs::write(root.join("rust/big.rs"), "// old\n".repeat(801)).unwrap();
            std::fs::write(root.join("web/big.ts"), "// old\n".repeat(501)).unwrap();
            git(&root, &["add", "."]).unwrap();
            git(&root, &["commit", "-qm", "baseline"]).unwrap();
            let sha = git_text(&root, &["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_owned();
            let tree = git_text(&root, &["rev-parse", "HEAD^{tree}"])
                .unwrap()
                .trim()
                .to_owned();
            std::fs::write(root.join("rust/later.rs"), "// later committed work\n").unwrap();
            git(&root, &["add", "."]).unwrap();
            git(&root, &["commit", "-qm", "later work"]).unwrap();
            Self {
                base,
                root,
                sha,
                tree,
            }
        }
        fn export(&self, name: &str) -> Result<Value> {
            export(&self.root, &self.sha, &self.tree, &self.base.join(name))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
    #[test]
    fn dirty_tree_does_not_change_historical_counts() {
        let f = Fixture::new();
        f.export("first").unwrap();
        std::fs::write(f.root.join("rust/big.rs"), "// new\n".repeat(999)).unwrap();
        std::fs::write(f.root.join("rust/untracked.rs"), "// new\n".repeat(999)).unwrap();
        f.export("second").unwrap();
        let first = std::fs::read(f.base.join("first/loc-baseline.json")).unwrap();
        assert_eq!(
            first,
            std::fs::read(f.base.join("second/loc-baseline.json")).unwrap()
        );
        let v: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(v["oversized_rust"]["files"][0]["lines"], 801);
        assert_eq!(v["thresholds"]["rust_new_file_max"], 800);
        assert_eq!(v["thresholds"]["typescript_new_file_max"], 500);
    }
    #[test]
    fn refuses_overwrite_and_repository_destinations() {
        let f = Fixture::new();
        f.export("first").unwrap();
        assert!(f.export("first").is_err());
        assert!(export(&f.root, &f.sha, &f.tree, &f.root.join("new-baseline")).is_err());
        assert!(export(&f.root, &f.sha, &f.tree, &f.root).is_err());
    }
    #[test]
    fn refuses_missing_nonancestor_and_wrong_tree() {
        let f = Fixture::new();
        assert!(export(&f.root, &"0".repeat(40), &f.tree, &f.base.join("missing")).is_err());
        let divergent = git_text(&f.root, &["commit-tree", &f.tree, "-m", "unrelated"]).unwrap();
        assert!(
            export(
                &f.root,
                divergent.trim(),
                &f.tree,
                &f.base.join("divergent")
            )
            .is_err()
        );
        assert!(export(&f.root, &f.sha, &"0".repeat(40), &f.base.join("wrong-tree")).is_err());
        assert!(!f.base.join("wrong-tree").exists());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_parent_into_repository() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(&f.root, f.base.join("alias")).unwrap();
        assert!(export(&f.root, &f.sha, &f.tree, &f.base.join("alias/new")).is_err());
    }
}
