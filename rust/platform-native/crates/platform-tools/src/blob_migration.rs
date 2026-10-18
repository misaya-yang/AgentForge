//! Read-only local inventory, non-destructive copy and full-byte verification.
//! No database metadata or runtime configuration is changed by this module.
use ai_platform_service_primitives::content::storage::ObjectStorage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn check(ok: bool, why: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(why.into()) }
}
pub fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Source-relative key and exact destination key. Only deployment prefix addition is allowed.
    pub source_key: String,
    pub key: String,
    pub bytes: u64,
    pub sha256: String,
    pub content_type: String,
    pub metadata: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub scope: String,
    pub destination_prefix: String,
    pub max_object_bytes: usize,
    pub objects: Vec<Entry>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub manifest_sha256: String,
    pub destination: String,
    pub verified: BTreeSet<String>,
}
#[derive(Debug, Serialize)]
pub struct Receipt {
    pub manifest_sha256: String,
    pub destination: String,
    pub objects: usize,
    pub bytes: u64,
    pub fully_verified: bool,
    pub source_quiesced: bool,
    pub cutover_ready: bool,
    pub verification_only: bool,
    pub metadata_references_sha256: Option<String>,
}
/// Owner-exported metadata from the same quiesced database snapshot.
/// Keys include the deployment prefix, exactly as in the blob inventory.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataReference {
    pub owner: String,
    pub record_id: String,
    pub tenant_id: String,
    pub key: String,
    pub size_bytes: u64,
    pub content_type: String,
    pub sha256: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataReferences {
    pub schema: String,
    pub snapshot_id: String,
    pub objects: Vec<MetadataReference>,
}
impl Receipt {
    pub fn certify_metadata(
        &mut self,
        manifest: &Manifest,
        refs: &MetadataReferences,
    ) -> Result<()> {
        // Clear an earlier certificate before checking another metadata input.
        self.cutover_ready = false;
        self.metadata_references_sha256 = None;
        check(
            self.manifest_sha256 == manifest.digest()? && self.fully_verified,
            "receipt does not match verified blob inventory",
        )?;
        check(
            refs.schema == "agentforge-blob-metadata/v1" && !refs.snapshot_id.trim().is_empty(),
            "metadata snapshot identity required",
        )?;
        let objects: BTreeMap<_, _> = manifest
            .objects
            .iter()
            .map(|e| (e.key.as_str(), e))
            .collect();
        let mut records = BTreeSet::new();
        let mut tenants = BTreeMap::new();
        for r in &refs.objects {
            check(
                !r.owner.trim().is_empty()
                    && !r.record_id.trim().is_empty()
                    && records.insert((&r.owner, &r.record_id, &r.key)),
                "metadata owner or record identity invalid",
            )?;
            let e = objects
                .get(r.key.as_str())
                .ok_or("metadata references an absent or out-of-scope object")?;
            check(
                !r.tenant_id.is_empty() && r.key.split('/').any(|p| p == r.tenant_id),
                "metadata tenant does not match object path",
            )?;
            check(
                e.metadata
                    .get("tenant_id")
                    .is_none_or(|tenant| tenant == &r.tenant_id),
                "sidecar tenant conflicts with metadata owner",
            )?;
            if let Some(previous) = tenants.insert(r.key.as_str(), r.tenant_id.as_str()) {
                check(
                    previous == r.tenant_id,
                    "object referenced by multiple tenants",
                )?;
            }
            check(
                e.bytes == r.size_bytes
                    && e.content_type == r.content_type
                    && r.sha256.as_ref().is_none_or(|sha| sha == &e.sha256),
                "metadata size, content type or checksum mismatch",
            )?;
        }
        check(
            tenants.len() == objects.len(),
            "metadata references do not cover every inventoried object",
        )?;
        self.metadata_references_sha256 = Some(sha(&serde_json::to_vec(refs)?));
        self.cutover_ready = self.verification_only && self.source_quiesced;
        Ok(())
    }
}
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains("..")
        && !key.contains(['\\', '\0'])
        && key
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}
fn source_path(root: &Path, key: &str) -> Result<PathBuf> {
    check(valid_key(key), "invalid object path")?;
    let mut path = root.to_owned();
    for segment in key.split('/') {
        path.push(segment);
        check(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "symlinks are forbidden in source inventory",
        )?;
    }
    check(
        path.canonicalize()?.starts_with(root.canonicalize()?),
        "object escaped source root",
    )?;
    Ok(path)
}
fn sidecar(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".meta");
    p.into()
}
fn inspect(root: &Path, key: &str, max: usize, prefix: &str) -> Result<Entry> {
    let path = source_path(root, key)?;
    let mut file = std::fs::File::open(&path)?;
    let bytes = file.metadata()?.len();
    check(
        bytes <= max as u64,
        "object exceeds configured migration memory limit",
    )?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut observed = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        observed += n as u64;
        check(observed <= max as u64, "source grew beyond object limit")?;
        hash.update(&buf[..n]);
    }
    check(observed == bytes, "source changed during inventory")?;
    let meta = sidecar(&path);
    let mut metadata: BTreeMap<String, String> = if meta.try_exists()? {
        let relative = format!("{key}.meta");
        let meta = source_path(root, &relative)?;
        check(
            std::fs::metadata(&meta)?.len() <= 1024 * 1024,
            "metadata sidecar exceeds limit",
        )?;
        serde_json::from_slice(&std::fs::read(meta)?)?
    } else {
        BTreeMap::new()
    };
    if metadata.contains_key("file_id")
        && metadata.contains_key("user_id")
        && !metadata.contains_key("uploaded_at")
    {
        let uploaded_at: chrono::DateTime<chrono::Utc> =
            std::fs::metadata(&path)?.modified()?.into();
        metadata.insert("uploaded_at".into(), uploaded_at.to_rfc3339());
    }
    let content_type = metadata
        .remove("content_type")
        .unwrap_or_else(|| "application/octet-stream".into());
    check(
        !content_type.is_empty()
            && content_type.is_ascii()
            && !content_type.chars().any(char::is_control),
        "invalid content type",
    )?;
    for key in metadata.keys() {
        check(
            key != "agentforge-internal-metadata-format",
            "source uses a reserved transport metadata key",
        )?;
        check(
            !key.is_empty()
                && key.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                }),
            "metadata keys must be lowercase S3 header tokens",
        )?;
    }
    let sha256 = format!("{:x}", hash.finalize());
    if let Some(expected) = metadata.get("content_hash") {
        let observed = if expected.len() == 32 {
            ai_platform_service_primitives::content::storage::legacy_content_md5(
                std::fs::File::open(&path)?.take(max as u64 + 1),
            )?
        } else {
            sha256.clone()
        };
        check(
            expected.eq_ignore_ascii_case(&observed),
            "local content_hash does not match source bytes",
        )?;
    }
    Ok(Entry {
        source_key: key.into(),
        key: ai_platform_service_primitives::content::storage::prefixed_key(prefix, key),
        bytes,
        sha256,
        content_type,
        metadata,
    })
}
pub fn inventory(root: &Path, scope: &str, max: usize) -> Result<Manifest> {
    inventory_with_prefix(root, scope, max, "")
}
pub fn inventory_with_prefix(
    root: &Path,
    scope: &str,
    max: usize,
    destination_prefix: &str,
) -> Result<Manifest> {
    check(
        destination_prefix.is_empty() || valid_key(destination_prefix),
        "invalid destination deployment prefix",
    )?;
    check(
        valid_key(scope),
        "scope must be a nonempty exact relative prefix",
    )?;
    check(
        (1..=256 * 1024 * 1024).contains(&max),
        "migration object limit must be 1..256MiB",
    )?;
    let directory = source_path(root, scope)?;
    check(directory.is_dir(), "source scope must be a directory")?;
    fn walk(root: &Path, dir: &Path, keys: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            check(!ty.is_symlink(), "source contains a symlink")?;
            if ty.is_dir() {
                walk(root, &entry.path(), keys)?;
            } else {
                check(ty.is_file(), "source contains a special file")?;
                let key = entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or("non UTF-8 key")?
                    .to_owned();
                if key.ends_with(".meta") {
                    check(
                        root.join(
                            key.strip_suffix(".meta")
                                .ok_or("invalid metadata sidecar")?,
                        )
                        .is_file(),
                        "orphan or ambiguous metadata sidecar",
                    )?;
                } else {
                    keys.push(key);
                }
            }
        }
        Ok(())
    }
    let mut keys = vec![];
    walk(root, &directory, &mut keys)?;
    keys.sort();
    let objects = keys
        .iter()
        .map(|key| inspect(root, key, max, destination_prefix))
        .collect::<Result<Vec<_>>>()?;
    check(
        !objects.is_empty(),
        "empty migration scope is not cutover evidence",
    )?;
    Ok(Manifest {
        schema: "agentforge-blob-migration/v1".into(),
        scope: scope.into(),
        destination_prefix: destination_prefix.into(),
        max_object_bytes: max,
        objects,
    })
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        check(
            self.schema == "agentforge-blob-migration/v1"
                && valid_key(&self.scope)
                && !self.objects.is_empty(),
            "invalid migration manifest",
        )?;
        check(
            (1..=256 * 1024 * 1024).contains(&self.max_object_bytes),
            "invalid migration limit",
        )?;
        check(
            self.destination_prefix.is_empty() || valid_key(&self.destination_prefix),
            "invalid deployment prefix",
        )?;
        let mut keys = BTreeSet::new();
        let mut source_keys = BTreeSet::new();
        for e in &self.objects {
            check(
                valid_key(&e.source_key)
                    && valid_key(&e.key)
                    && e.source_key.starts_with(&format!("{}/", self.scope))
                    && e.key
                        == ai_platform_service_primitives::content::storage::prefixed_key(
                            &self.destination_prefix,
                            &e.source_key,
                        )
                    && source_keys.insert(&e.source_key)
                    && keys.insert(&e.key),
                "duplicate or out-of-scope object",
            )?;
            check(
                e.bytes <= self.max_object_bytes as u64
                    && e.sha256.len() == 64
                    && e.sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid object digest or size",
            )?;
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        Ok(sha(&serde_json::to_vec(self)?))
    }
}
/// Atomic checkpoint/receipt persistence. Output must be outside the source scope.
pub fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|v| !v.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    check(!path.is_symlink(), "refusing symlink output")?;
    let staged = parent.join(format!(".blob-migration-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&staged)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        std::fs::rename(&staged, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}
async fn verify(storage: &dyn ObjectStorage, e: &Entry, max: usize) -> Result<()> {
    let info = storage
        .head(&e.key)
        .await?
        .ok_or("destination object missing")?;
    check(
        info.size_bytes == e.bytes
            && info.content_type.as_deref() == Some(&e.content_type)
            && info.metadata == e.metadata,
        "destination metadata mismatch",
    )?;
    let bytes = storage.download_bounded(&e.key, max).await?;
    check(
        bytes.len() as u64 == e.bytes && sha(&bytes) == e.sha256,
        "destination SHA256 mismatch",
    )
}
pub async fn migrate(
    root: &Path,
    manifest: &Manifest,
    storage: &dyn ObjectStorage,
    destination: &str,
    state_path: &Path,
    verify_only: bool,
    source_quiesced: bool,
) -> Result<Receipt> {
    manifest.validate()?;
    check(
        inventory_with_prefix(
            root,
            &manifest.scope,
            manifest.max_object_bytes,
            &manifest.destination_prefix,
        )? == *manifest,
        "source inventory changed; create and review a new manifest",
    )?;
    let digest = manifest.digest()?;
    let mut state: Checkpoint = if state_path.try_exists()? {
        serde_json::from_slice(&std::fs::read(state_path)?)?
    } else {
        Checkpoint {
            manifest_sha256: digest.clone(),
            destination: destination.into(),
            verified: BTreeSet::new(),
        }
    };
    check(
        state.manifest_sha256 == digest && state.destination == destination,
        "checkpoint belongs to another manifest or destination",
    )?;
    // Checkpoints are progress only: every existing object is re-read and verified.
    for e in &manifest.objects {
        check(
            inspect(
                root,
                &e.source_key,
                manifest.max_object_bytes,
                &manifest.destination_prefix,
            )? == *e,
            "source object changed",
        )?;
        if storage.head(&e.key).await?.is_none() {
            check(!verify_only, "verify-only found missing destination object")?;
            let mut bytes = Vec::new();
            std::fs::File::open(source_path(root, &e.source_key)?)?
                .take(manifest.max_object_bytes as u64 + 1)
                .read_to_end(&mut bytes)?;
            check(
                bytes.len() as u64 == e.bytes && sha(&bytes) == e.sha256,
                "source changed before upload",
            )?;
            storage
                .upload_if_absent(&e.key, bytes, &e.content_type, e.metadata.clone())
                .await?;
        }
        // Existing mismatches are never overwritten or deleted.
        verify(storage, e, manifest.max_object_bytes).await?;
        state.verified.insert(e.key.clone());
        save(state_path, &state)?;
    }
    // A second full pass prevents an earlier verified object from becoming stale unnoticed.
    for e in &manifest.objects {
        verify(storage, e, manifest.max_object_bytes).await?;
    }
    check(
        inventory_with_prefix(
            root,
            &manifest.scope,
            manifest.max_object_bytes,
            &manifest.destination_prefix,
        )? == *manifest,
        "source changed during copy; cutover forbidden",
    )?;
    Ok(Receipt {
        manifest_sha256: digest,
        destination: destination.into(),
        objects: manifest.objects.len(),
        bytes: manifest.objects.iter().map(|e| e.bytes).sum(),
        fully_verified: true,
        source_quiesced,
        cutover_ready: false,
        verification_only: verify_only,
        metadata_references_sha256: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_platform_service_primitives::content::storage::LocalStorage;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("blob-migration-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(p.join("source/dev/uploads/tenant-a/user")).unwrap();
            std::fs::create_dir_all(p.join("destination")).unwrap();
            Self(p)
        }
        fn root(&self) -> PathBuf {
            self.0.join("source")
        }
        fn state(&self) -> PathBuf {
            self.0.join("state.json")
        }
        fn storage(&self) -> LocalStorage {
            LocalStorage::new(self.0.join("destination"), String::new())
        }
        fn put(&self, name: &str, bytes: &[u8]) {
            let p = self
                .root()
                .join(format!("dev/uploads/tenant-a/user/{name}"));
            std::fs::write(&p, bytes).unwrap();
            std::fs::write(sidecar(&p),r#"{"content_type":"text/plain","tenant_id":"tenant-a","original_filename":"你好.txt"}"#).unwrap();
        }
        fn manifest(&self) -> Manifest {
            inventory(&self.root(), "dev/uploads/tenant-a", 1024).unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn copy_resume_and_verify_require_separate_quiesced_final_gate() {
        let s = Scratch::new();
        s.put("a", b"hello");
        s.put("b", b"world");
        let manifest = s.manifest();
        let copy = migrate(
            &s.root(),
            &manifest,
            &s.storage(),
            "bucket-one",
            &s.state(),
            false,
            true,
        )
        .await
        .unwrap();
        assert!(!copy.cutover_ready);
        let mut verify = migrate(
            &s.root(),
            &manifest,
            &s.storage(),
            "bucket-one",
            &s.state(),
            true,
            true,
        )
        .await
        .unwrap();
        assert!(!verify.cutover_ready);
        let refs = MetadataReferences {
            schema: "agentforge-blob-metadata/v1".into(),
            snapshot_id: "isolated-snapshot".into(),
            objects: manifest
                .objects
                .iter()
                .map(|e| MetadataReference {
                    owner: "gateway.files".into(),
                    record_id: e.key.clone(),
                    tenant_id: "tenant-a".into(),
                    key: e.key.clone(),
                    size_bytes: e.bytes,
                    content_type: e.content_type.clone(),
                    sha256: Some(e.sha256.clone()),
                })
                .collect(),
        };
        verify.certify_metadata(&manifest, &refs).unwrap();
        assert!(verify.cutover_ready);
        let mut wrong = refs.clone();
        wrong.objects[0].tenant_id = "tenant-b".into();
        assert!(verify.certify_metadata(&manifest, &wrong).is_err());
        assert!(!verify.cutover_ready);
        let mut wrong = refs.clone();
        wrong.objects[0].size_bytes += 1;
        assert!(verify.certify_metadata(&manifest, &wrong).is_err());
        let mut wrong = refs.clone();
        wrong.objects.pop();
        assert!(verify.certify_metadata(&manifest, &wrong).is_err());
        assert_eq!(verify.objects, 2);
        assert!(
            migrate(
                &s.root(),
                &manifest,
                &s.storage(),
                "bucket-two",
                &s.state(),
                true,
                true
            )
            .await
            .is_err()
        );
        assert!(
            !migrate(
                &s.root(),
                &manifest,
                &s.storage(),
                "bucket-one",
                &s.state(),
                true,
                false
            )
            .await
            .unwrap()
            .cutover_ready
        );
    }
    #[tokio::test]
    async fn resumed_checkpoint_does_not_trust_same_length_corrupted_destination() {
        let s = Scratch::new();
        s.put("a", b"hello");
        let m = s.manifest();
        migrate(
            &s.root(),
            &m,
            &s.storage(),
            "bucket",
            &s.state(),
            false,
            false,
        )
        .await
        .unwrap();
        let path = s.0.join("destination").join(&m.objects[0].key);
        std::fs::write(&path, b"wrong").unwrap();
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                false,
                true
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"wrong");
    }
    #[tokio::test]
    async fn interrupted_copy_resumes_without_overwriting_conflicting_objects() {
        let s = Scratch::new();
        s.put("a", b"hello");
        s.put("b", b"world");
        let m = s.manifest();
        s.storage()
            .upload(
                &m.objects[1].key,
                b"wrong".to_vec(),
                "text/plain",
                m.objects[1].metadata.clone(),
            )
            .await
            .unwrap();
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                false,
                false
            )
            .await
            .is_err()
        );
        let state: Checkpoint = serde_json::from_slice(&std::fs::read(s.state()).unwrap()).unwrap();
        assert_eq!(state.verified.len(), 1);
        // Explicit fixture repair models an operator resolving the conflict.
        s.storage().delete(&m.objects[1].key).await.unwrap();
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                false,
                false
            )
            .await
            .unwrap()
            .fully_verified
        );
    }
    #[tokio::test]
    async fn metadata_mismatch_and_source_mutation_forbid_readiness() {
        let s = Scratch::new();
        s.put("a", b"hello");
        let m = s.manifest();
        migrate(
            &s.root(),
            &m,
            &s.storage(),
            "bucket",
            &s.state(),
            false,
            false,
        )
        .await
        .unwrap();
        let path = s.0.join("destination").join(&m.objects[0].key);
        std::fs::write(
            sidecar(&path),
            r#"{"content_type":"text/plain","tenant_id":"tenant-b"}"#,
        )
        .unwrap();
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                true,
                true
            )
            .await
            .is_err()
        );
        s.put("new", b"new");
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                false,
                true
            )
            .await
            .is_err()
        );
    }
    #[test]
    fn boundary_sidecar_hash_and_scope_checks_fail_closed() {
        let s = Scratch::new();
        s.put("a", b"hello");
        let mut m = s.manifest();
        m.objects[0].key = "dev/uploads/tenant-ab/user/a".into();
        assert!(m.validate().is_err());
        for key in [
            "../dev",
            "/dev",
            "dev//uploads",
            "dev/./uploads",
            "dev/../uploads",
            "dev/v1..uploads",
            "dev\\uploads",
        ] {
            assert!(inventory(&s.root(), key, 1024).is_err());
        }
        let path = s.root().join("dev/uploads/tenant-a/user/a");
        std::fs::write(sidecar(&path), r#"{"content_hash":"wrong"}"#).unwrap();
        assert!(inventory(&s.root(), "dev/uploads/tenant-a", 1024).is_err());
        std::fs::write(sidecar(&path), b"{broken").unwrap();
        assert!(inventory(&s.root(), "dev/uploads/tenant-a", 1024).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_tenant_scope() {
        let s = Scratch::new();
        s.put("a", b"hello");
        std::os::unix::fs::symlink(s.state(), s.root().join("dev/uploads/tenant-a/user/link"))
            .unwrap();
        assert!(s.manifest_result().is_err());
    }
    #[cfg(unix)]
    impl Scratch {
        fn manifest_result(&self) -> Result<Manifest> {
            inventory(&self.root(), "dev/uploads/tenant-a", 1024)
        }
    }
    #[tokio::test]
    async fn verify_only_missing_object_never_uploads() {
        let s = Scratch::new();
        s.put("a", b"hello");
        let m = s.manifest();
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                true,
                true
            )
            .await
            .is_err()
        );
        assert!(s.storage().head(&m.objects[0].key).await.unwrap().is_none());
        assert!(!s.state().exists());
    }
    #[tokio::test]
    async fn legacy_unprefixed_uploads_preserve_md5_metadata_and_use_independent_sha256() {
        let s = Scratch::new();
        let path = s.root().join("uploads/tenant-a/user/a");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"hello").unwrap();
        std::fs::write(sidecar(&path), r#"{"content_type":"text/plain","tenant_id":"tenant-a","content_hash":"5d41402abc4b2a76b9719d911017c592"}"#).unwrap();
        let m = inventory_with_prefix(&s.root(), "uploads/tenant-a", 1024, "dev").unwrap();
        assert_eq!(m.objects[0].source_key, "uploads/tenant-a/user/a");
        assert_eq!(m.objects[0].key, "dev/uploads/tenant-a/user/a");
        assert_eq!(m.objects[0].sha256, sha(b"hello"));
        assert_ne!(m.objects[0].sha256, m.objects[0].metadata["content_hash"]);
        m.validate().unwrap();
        assert!(
            migrate(
                &s.root(),
                &m,
                &s.storage(),
                "bucket",
                &s.state(),
                false,
                false
            )
            .await
            .unwrap()
            .fully_verified
        );
        assert_eq!(
            s.storage()
                .download("dev/uploads/tenant-a/user/a")
                .await
                .unwrap(),
            b"hello"
        );
        let mut invalid = m.clone();
        invalid.objects[0].key = "dev/uploads/tenant-b/user/a".into();
        assert!(invalid.validate().is_err());
        std::fs::write(
            sidecar(&path),
            r#"{"content_hash":"00000000000000000000000000000000"}"#,
        )
        .unwrap();
        assert!(inventory_with_prefix(&s.root(), "uploads/tenant-a", 1024, "dev").is_err());
        s.put("already-prefixed", b"hello");
        let existing =
            inventory_with_prefix(&s.root(), "dev/uploads/tenant-a", 1024, "dev").unwrap();
        assert_eq!(existing.objects[0].source_key, existing.objects[0].key);
    }
    #[test]
    fn bounded_inventory_and_orphan_sidecars_are_rejected() {
        let s = Scratch::new();
        s.put("a", b"hello");
        assert!(inventory(&s.root(), "dev/uploads/tenant-a", 4).is_err());
        std::fs::write(s.root().join("dev/uploads/tenant-a/orphan.meta"), b"{}").unwrap();
        assert!(inventory(&s.root(), "dev/uploads/tenant-a", 1024).is_err());
    }
}
