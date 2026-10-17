//! Test-only exact historical source views. No production pin override or fake
//! receipt: retain all actual locked files, replace only archived manifest/lock,
//! and remove authority inputs absent from that historical inventory.
use super::*;
pub(crate) struct HistoricalSourceFixture {
    directory: PathBuf,
    root: PathBuf,
    lock_sha: &'static str,
}
impl HistoricalSourceFixture {
    pub(crate) fn new(epoch: i32) -> Self {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let (lock_archive, manifest_archive, lock_sha, manifest_sha) = match epoch {
            21 => (
                "docs/design/canonical-schema-successor-022-approved21-package-lock.json",
                "docs/design/canonical-schema-successor-022-approved21-manifest.yml",
                crate::source_tools::next_successor_022::APPROVED_021_LOCK_SHA256,
                "bbc6927b258011d63b97150f4eafbce18d20907c3c3c3f59d23eaab947e04cdd",
            ),
            22 => (
                "docs/design/canonical-schema-successor-023-approved22-package-lock.json",
                "docs/design/canonical-schema-successor-023-approved22-manifest.yml",
                crate::source_tools::next_successor_023::APPROVED_022_LOCK_SHA256,
                "d723f09097401133748454355cad52e886df50c628d964b2fc55ca256685fc89",
            ),
            23 => {
                let active =
                    manifest::load_epoch_manifest(&repository.join(ACTIVE_MANIFEST_RELATIVE))
                        .unwrap();
                if active.epoch == 23 {
                    (
                        ACTIVE_LOCK_RELATIVE,
                        ACTIVE_MANIFEST_RELATIVE,
                        crate::source_tools::next_successor_024::APPROVED_023_LOCK_SHA256,
                        crate::source_tools::next_successor_024::APPROVED_023_MANIFEST_SHA256,
                    )
                } else {
                    (
                        "docs/design/canonical-schema-successor-024-approved23-package-lock.json",
                        "docs/design/canonical-schema-successor-024-approved23-manifest.yml",
                        crate::source_tools::next_successor_024::APPROVED_023_LOCK_SHA256,
                        crate::source_tools::next_successor_024::APPROVED_023_MANIFEST_SHA256,
                    )
                }
            }
            _ => panic!("unreviewed historical epoch fixture"),
        };
        let lock_bytes = fs::read(repository.join(lock_archive)).unwrap();
        let manifest_bytes = fs::read(repository.join(manifest_archive)).unwrap();
        assert_eq!(sha256_bytes(&lock_bytes), lock_sha);
        assert_eq!(sha256_bytes(&manifest_bytes), manifest_sha);
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "agentforge-historical-{epoch}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&directory).unwrap();
        let root = directory.join("source");
        fs::create_dir(&root).unwrap();
        copy_tree(&repository.join("database"), &root.join("database")).unwrap();
        fs::create_dir(root.join("docs")).unwrap();
        copy_tree(&repository.join("docs/design"), &root.join("docs/design")).unwrap();
        fs::write(root.join(ACTIVE_LOCK_RELATIVE), lock_bytes).unwrap();
        fs::write(root.join(ACTIVE_MANIFEST_RELATIVE), manifest_bytes).unwrap();
        let lock: Value =
            serde_json::from_slice(&fs::read(root.join(ACTIVE_LOCK_RELATIVE)).unwrap()).unwrap();
        let expected = lock_files(&lock)
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = collect_distribution_inputs(&root.join("database")).unwrap();
        for relative in actual.difference(&expected) {
            let path = source_file(&root, &format!("database/{relative}")).unwrap();
            fs::remove_file(path).unwrap();
        }
        let fixture = Self {
            directory,
            root,
            lock_sha,
        };
        fixture.verify_inventory().unwrap();
        assert_eq!(
            manifest::load_epoch_manifest(&fixture.root.join(ACTIVE_MANIFEST_RELATIVE))
                .unwrap()
                .epoch,
            epoch
        );
        fixture
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn verify_inventory(&self) -> Result<()> {
        if manifest::file_sha256(&source_file(&self.root, ACTIVE_LOCK_RELATIVE)?)? != self.lock_sha
        {
            return Err(refused("historical fixture lock changed"));
        }
        let lock: Value = serde_json::from_slice(&fs::read(self.root.join(ACTIVE_LOCK_RELATIVE))?)
            .map_err(|_| refused("historical fixture malformed lock"))?;
        let files = lock_files(&lock)?;
        if collect_distribution_inputs(&self.root.join("database"))?
            != files.keys().cloned().collect::<BTreeSet<_>>()
        {
            return Err(refused("historical fixture coverage drift"));
        }
        for (relative, expected) in files {
            if manifest::file_sha256(&source_file(&self.root, &format!("database/{relative}"))?)?
                != expected
                    .as_str()
                    .ok_or_else(|| refused("historical fixture bad digest"))?
            {
                return Err(refused("historical fixture byte drift"));
            }
        }
        Ok(())
    }
    /// Old package integrity is separate from current deployment eligibility.
    /// A retired pin must still be refused; never alter production admission to
    /// make a historical unit-test fixture look like a deployable release.
    pub(crate) fn assert_release_eligibility<T>(&self, result: Result<T>) -> Option<T> {
        let eligible = [
            distribution::LOCK_SHA256,
            distribution::PREVIOUS_RELEASE_LOCK_SHA256,
            // Exact022 proof pin is retained for the recursive023 ancestry
            // verifier even after the canonical release advances to024.
            crate::source_tools::next_successor_023::APPROVED_022_LOCK_SHA256,
            distribution::PREDECESSOR_LOCK_SHA256,
        ]
        .contains(&self.lock_sha);
        match (eligible, result) {
            (true, Ok(value)) => Some(value),
            (false, Err(AuthorityError::Refused(message)))
                if message == "authority distribution lock differs from the release pin" =>
            {
                None
            }
            (expected, outcome) => panic!(
                "historical release eligibility mismatch (eligible={expected}, succeeded={})",
                outcome.is_ok()
            ),
        }
    }
}
impl Drop for HistoricalSourceFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub(crate) fn verify_current_chain(root: &Path) -> manifest::EpochManifest {
    let active = manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap();
    let verified = match active.epoch {
        21 => verify_activated_source(root).unwrap().1,
        22 => {
            crate::source_tools::next_successor_022::verify_activated_source(root)
                .unwrap()
                .1
        }
        23 => {
            crate::source_tools::next_successor_023::verify_activated_source(root)
                .unwrap()
                .1
        }
        24 => {
            let receipt =
                crate::source_tools::next_successor_024::verify_activated_source(root).unwrap();
            assert_eq!(receipt["candidate_epoch"], 24);
            manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap()
        }
        _ => panic!("current root must use an explicitly supported fully verified chain"),
    };
    assert_eq!(verified.epoch, active.epoch);
    assert_eq!(verified.changes.len(), active.epoch as usize);
    assert_eq!(
        serde_json::to_value(&verified.changes).unwrap(),
        serde_json::to_value(&active.changes).unwrap()
    );
    verified
}
