//! Test-only exact historical source views. No production pin override or fake
//! receipt: retain all actual locked files, replace only archived manifest/lock,
//! and remove authority inputs absent from that historical inventory.
use super::*;

#[derive(Clone, Copy)]
struct Successor027Identity {
    lock_sha: &'static str,
    manifest_sha: &'static str,
    reference_sha: &'static str,
    measurement_sha: &'static str,
    activation_sha: &'static str,
    source_plan_sha: &'static str,
}
pub(crate) struct HistoricalSourceFixture {
    directory: PathBuf,
    root: PathBuf,
    lock_sha: &'static str,
    successor_027_identity: Option<Successor027Identity>,
}
impl HistoricalSourceFixture {
    pub(crate) fn new(epoch: i32) -> Self {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let active_epoch =
            manifest::load_epoch_manifest(&repository.join(ACTIVE_MANIFEST_RELATIVE))
                .unwrap()
                .epoch;
        let successor_027_identity = (epoch == 27).then(|| Successor027Identity {
            lock_sha: crate::source_tools::next_successor_028::APPROVED_027_LOCK_SHA256
                .expect("028 fixture needs the real 027 package-lock pin"),
            manifest_sha: crate::source_tools::next_successor_028::APPROVED_027_MANIFEST_SHA256
                .expect("028 fixture needs the real 027 manifest pin"),
            reference_sha: crate::source_tools::next_successor_028::APPROVED_027_REFERENCE_SHA256
                .expect("028 fixture needs the real 027 verification pin"),
            measurement_sha:
                crate::source_tools::next_successor_028::APPROVED_027_MEASUREMENT_SHA256
                    .expect("028 fixture needs the real 027 measurement pin"),
            activation_sha: crate::source_tools::next_successor_028::APPROVED_027_ACTIVATION_SHA256
                .expect("028 fixture needs the real 027 activation pin"),
            source_plan_sha:
                crate::source_tools::next_successor_028::APPROVED_027_SOURCE_PLAN_SHA256
                    .expect("028 fixture needs the real 027 source-plan pin"),
        });
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
                if active_epoch == 23 {
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
            24 => (
                "docs/design/canonical-schema-successor-025-approved24-package-lock.json",
                "docs/design/canonical-schema-successor-025-approved24-manifest.yml",
                crate::source_tools::next_successor_025::APPROVED_024_LOCK_SHA256,
                crate::source_tools::next_successor_025::APPROVED_024_MANIFEST_SHA256,
            ),
            25 => (
                "docs/design/canonical-schema-successor-026-approved25-package-lock.json",
                "docs/design/canonical-schema-successor-026-approved25-manifest.yml",
                crate::source_tools::next_successor_026::APPROVED_025_LOCK_SHA256,
                crate::source_tools::next_successor_026::APPROVED_025_MANIFEST_SHA256,
            ),
            26 => (
                "docs/design/canonical-schema-successor-027-approved26-package-lock.json",
                "docs/design/canonical-schema-successor-027-approved26-manifest.yml",
                crate::source_tools::next_successor_027::APPROVED_026_LOCK_SHA256,
                crate::source_tools::next_successor_027::APPROVED_026_MANIFEST_SHA256,
            ),
            27 => {
                let identity = successor_027_identity.unwrap();
                let (lock, manifest) = match active_epoch {
                    27 => (ACTIVE_LOCK_RELATIVE, ACTIVE_MANIFEST_RELATIVE),
                    28 => (
                        "docs/design/canonical-schema-successor-028-approved27-package-lock.json",
                        "docs/design/canonical-schema-successor-028-approved27-manifest.yml",
                    ),
                    _ => panic!("epoch27 fixture requires active27 or active28 source"),
                };
                (lock, manifest, identity.lock_sha, identity.manifest_sha)
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
        for relative in [
            "docs/design/canonical-schema-successor-028-approved27-package-lock.json",
            "docs/design/canonical-schema-successor-028-approved27-manifest.yml",
        ] {
            let path = root.join(relative);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
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
            successor_027_identity,
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
        if let Some(identity) = self.successor_027_identity {
            self.verify_successor_027_identity(identity)?;
        }
        Ok(())
    }
    fn verify_successor_027_identity(&self, identity: Successor027Identity) -> Result<()> {
        let hash = |relative: &str| -> Result<String> {
            Ok(manifest::file_sha256(&source_file(&self.root, relative)?)?)
        };
        let epoch = "database/migrations/2026_08_post_kb_v1/";
        if hash(ACTIVE_LOCK_RELATIVE)? != identity.lock_sha
            || hash(ACTIVE_MANIFEST_RELATIVE)? != identity.manifest_sha
            || hash(&format!("{epoch}027_verification.json"))? != identity.reference_sha
            || hash(&format!("{epoch}SUCCESSOR_027_MEASUREMENT.json"))? != identity.measurement_sha
            || hash(&format!("{epoch}SUCCESSOR_027_ACTIVATION.json"))? != identity.activation_sha
        {
            return Err(refused(
                "historical 027 fixture differs from a real pinned artifact",
            ));
        }
        let activation =
            crate::source_tools::next_successor_027::verify_activated_source(&self.root)?;
        if activation["source_plan_sha256"] != identity.source_plan_sha
            || activation["verification_ref_sha256"] != identity.reference_sha
            || activation["measurement_receipt_sha256"] != identity.measurement_sha
        {
            return Err(refused(
                "historical 027 fixture has a different activated source chain",
            ));
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
            // Exact026 proof pin remains available to the recursive027 verifier.
            crate::source_tools::next_successor_027::APPROVED_026_LOCK_SHA256,
            // Exact024 pin remains accepted as a historical source package.
            crate::source_tools::next_successor_025::APPROVED_024_LOCK_SHA256,
            crate::source_tools::next_successor_024::APPROVED_023_LOCK_SHA256,
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
        25 => {
            let receipt =
                crate::source_tools::next_successor_025::verify_activated_source(root).unwrap();
            assert_eq!(receipt["candidate_epoch"], 25);
            manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap()
        }
        26 => {
            let receipt =
                crate::source_tools::next_successor_026::verify_activated_source(root).unwrap();
            assert_eq!(receipt["candidate_epoch"], 26);
            manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap()
        }
        27 => {
            let receipt =
                crate::source_tools::next_successor_027::verify_activated_source(root).unwrap();
            assert_eq!(receipt["candidate_epoch"], 27);
            manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap()
        }
        28 => {
            let receipt =
                crate::source_tools::next_successor_028::verify_activated_source(root).unwrap();
            assert_eq!(receipt["candidate_epoch"], 28);
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
