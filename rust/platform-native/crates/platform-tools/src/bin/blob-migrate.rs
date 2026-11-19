//! Offline plan by default; all remote writes require the explicit `copy` command.
use ai_platform_service_primitives::content::storage::{CloudStorage, S3HttpTransport};
use ai_platform_tools::blob_migration::{self as migration, Manifest, MetadataReferences, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn required<'a>(opts: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    opts.get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing {name}").into())
}
fn external(root: &Path, value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .ok_or("output parent missing")?
        .canonicalize()?;
    let resolved = parent.join(absolute.file_name().ok_or("output filename missing")?);
    if resolved.starts_with(root) || resolved.is_symlink() {
        return Err("migration output must be outside source root and not a symlink".into());
    }
    Ok(resolved)
}
fn env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("missing {name}").into())
}
async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).ok_or("usage: blob-migrate plan|copy|verify --root PATH --manifest PATH [--scope PREFIX] [--state PATH --receipt PATH --source-quiesced]")?;
    if !["plan", "copy", "verify"].contains(&command) {
        return Err("unknown migration command".into());
    }
    let mut opts = BTreeMap::new();
    let mut dry = false;
    let mut quiesced = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" if !dry => {
                dry = true;
                i += 1;
            }
            "--source-quiesced" if !quiesced => {
                quiesced = true;
                i += 1;
            }
            key @ ("--root"
            | "--manifest"
            | "--scope"
            | "--max-object-bytes"
            | "--state"
            | "--receipt"
            | "--metadata-references"
            | "--key-prefix") => {
                let value = args.get(i + 1).ok_or("option value missing")?;
                if opts.insert(key.to_owned(), value.to_owned()).is_some() {
                    return Err("duplicate option".into());
                }
                i += 2;
            }
            _ => return Err("unknown or duplicate option".into()),
        }
    }
    let root_input = PathBuf::from(required(&opts, "--root")?);
    if root_input.is_symlink() {
        return Err("source root may not be a symlink".into());
    }
    let root = root_input.canonicalize()?;
    let manifest_path = external(&root, required(&opts, "--manifest")?)?;
    if command == "plan" {
        if quiesced
            || opts.contains_key("--state")
            || opts.contains_key("--receipt")
            || opts.contains_key("--metadata-references")
        {
            return Err("plan does not accept copy/verify options".into());
        }
        let max = opts
            .get("--max-object-bytes")
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(64 * 1024 * 1024);
        let manifest = migration::inventory_with_prefix(
            &root,
            required(&opts, "--scope")?,
            max,
            opts.get("--key-prefix").map(String::as_str).unwrap_or(""),
        )?;
        if !dry {
            if manifest_path.try_exists()? {
                return Err("manifest exists; choose a new reviewed inventory path".into());
            }
            migration::save(&manifest_path, &manifest)?;
        }
        println!(
            "{}",
            serde_json::json!({"dry_run":dry,"objects":manifest.objects.len(),"manifest_sha256":manifest.digest()?,"cutover_ready":false})
        );
        return Ok(());
    }
    if opts.contains_key("--scope")
        || opts.contains_key("--max-object-bytes")
        || opts.contains_key("--key-prefix")
    {
        return Err("copy/verify take scope and limit only from the manifest".into());
    }
    if command == "copy" && opts.contains_key("--metadata-references") {
        return Err("metadata references are certified only by verify".into());
    }
    let state = external(&root, required(&opts, "--state")?)?;
    let receipt = external(&root, required(&opts, "--receipt")?)?;
    if state == receipt || state == manifest_path || receipt == manifest_path {
        return Err("manifest, state and receipt must be distinct".into());
    }
    let references_path = opts
        .get("--metadata-references")
        .map(|p| external(&root, p))
        .transpose()?;
    if references_path
        .as_ref()
        .is_some_and(|p| p == &state || p == &receipt || p == &manifest_path)
    {
        return Err("metadata references must not overlap manifest or output paths".into());
    }
    if dry {
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        manifest.validate()?;
        if migration::inventory_with_prefix(
            &root,
            &manifest.scope,
            manifest.max_object_bytes,
            &manifest.destination_prefix,
        )? != manifest
        {
            return Err("source inventory changed".into());
        }
        println!(
            "{}",
            serde_json::json!({"dry_run":true,"objects":manifest.objects.len(),"manifest_sha256":manifest.digest()?,"cutover_ready":false})
        );
        return Ok(());
    }
    let lock_path = state.with_extension("migration-lock");
    if [manifest_path.as_path(), state.as_path(), receipt.as_path()].contains(&lock_path.as_path())
        || references_path.as_ref() == Some(&lock_path)
    {
        return Err("lock path overlaps an input/output".into());
    }
    let mut lock = std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path).map_err(|_| "migration lock exists or cannot be created; inspect owner before explicit stale-lock removal")?;
    let _guard = Lock(lock_path.clone());
    {
        use std::io::Write;
        writeln!(lock, "{}", std::process::id())?;
        lock.sync_all()?;
    }
    // Invalidate prior readiness before any operation that can fail or mutate the destination.
    migration::save(
        &receipt,
        &serde_json::json!({"cutover_ready":false,"status":"in-progress"}),
    )?;
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest.validate()?;
    if migration::inventory_with_prefix(
        &root,
        &manifest.scope,
        manifest.max_object_bytes,
        &manifest.destination_prefix,
    )? != manifest
    {
        return Err("source inventory changed".into());
    }
    let metadata_references: Option<MetadataReferences> = references_path
        .map(|p| -> Result<_> { Ok(serde_json::from_slice(&std::fs::read(p)?)?) })
        .transpose()?;
    if command == "verify" && quiesced && metadata_references.is_none() {
        return Err(
            "final verify requires --metadata-references from the quiesced metadata owners".into(),
        );
    }
    let bucket = env("BLOB_MIGRATION_S3_BUCKET")?;
    let endpoint = std::env::var("BLOB_MIGRATION_S3_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty());
    let region = std::env::var("BLOB_MIGRATION_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
    let encryption = std::env::var("BLOB_MIGRATION_S3_SSE").unwrap_or_default();
    let kms = std::env::var("BLOB_MIGRATION_S3_KMS_KEY_ID").ok();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?;
    let transport = S3HttpTransport::new(
        client,
        region.clone(),
        env("BLOB_MIGRATION_S3_ACCESS_KEY")?,
        env("BLOB_MIGRATION_S3_SECRET_KEY")?.into_bytes(),
        endpoint.clone(),
    )?
    .with_encryption(&encryption, kms.as_deref())?;
    // Destination identity binds resumed work without recording credentials or endpoint URLs.
    let destination = migration::sha(&serde_json::to_vec(&(
        endpoint, region, &bucket, encryption, kms,
    ))?);
    let storage = CloudStorage {
        transport: Arc::new(transport),
        bucket,
        prefix: String::new(),
        public_base: String::new(),
        s3: true,
    };
    let mut result = migration::migrate(
        &root,
        &manifest,
        &storage,
        &destination,
        &state,
        command == "verify",
        quiesced,
    )
    .await?;
    if let Some(refs) = metadata_references {
        result.certify_metadata(&manifest, &refs)?;
    }
    migration::save(&receipt, &result)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("blob migration failed: {error}");
        std::process::exit(2);
    }
}
