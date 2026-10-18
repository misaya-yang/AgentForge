//! Storage keys, bounded object access and filesystem backend from core.storage.
use super::{ContentError, ContentFuture, percent_encode};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub size_bytes: u64,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
}
pub trait ObjectStorage: Send + Sync {
    /// Paginated logical keys relative to this backend's configured deployment prefix.
    fn list_keys<'a>(
        &'a self,
        _prefix: &'a str,
        _cursor: Option<String>,
    ) -> ContentFuture<'a, (Vec<String>, Option<String>)> {
        Box::pin(async { Err(ContentError::new(501, "Object listing is unsupported")) })
    }

    /// Conditional creation for migration: never replace a concurrently created object.
    fn upload_if_absent<'a>(
        &'a self,
        _key: &'a str,
        _content: Vec<u8>,
        _mime: &'a str,
        _metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async {
            Err(ContentError::new(
                501,
                "Conditional object creation is unsupported",
            ))
        })
    }
    fn local_path(&self, _key: &str) -> Result<Option<PathBuf>, ContentError> {
        Ok(None)
    }
    fn upload<'a>(
        &'a self,
        key: &'a str,
        content: Vec<u8>,
        content_type: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String>;
    fn download<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Vec<u8>>;
    fn download_bounded<'a>(&'a self, key: &'a str, max: usize) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let content = self.download(key).await?;
            if content.len() > max {
                return Err(object_too_large());
            }
            Ok(content)
        })
    }
    /// Stream to a caller-owned path without allocating the complete object.
    /// Built-in implementations stage beside target and replace it only after
    /// successful bounded completion; failures leave an existing target intact.
    fn download_to_path<'a>(
        &'a self,
        _key: &'a str,
        _target: &'a Path,
        _max_bytes: usize,
    ) -> ContentFuture<'a, ()> {
        // External backends must opt in to true bounded streaming. Falling back
        // to download() here would silently reintroduce whole-PDF allocation.
        Box::pin(async {
            Err(ContentError::new(
                501,
                "Streaming download is not supported",
            ))
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> ContentFuture<'a, bool>;
    fn delete_prefix<'a>(&'a self, prefix: &'a str) -> ContentFuture<'a, u64>;
    fn head<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Option<ObjectInfo>>;
    fn download_url<'a>(
        &'a self,
        key: &'a str,
        expiry: u32,
        filename: Option<&'a str>,
    ) -> ContentFuture<'a, Option<String>>;
    fn upload_url<'a>(
        &'a self,
        key: &'a str,
        content_type: &'a str,
        expiry: u32,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, Option<BTreeMap<String, String>>>;
}
pub fn prefixed_key(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let mut key = key.trim_start_matches('/').replace('\0', "");
    while key.contains("..") {
        key = key.replace("..", "_");
    }
    if prefix.is_empty() || key == prefix || key.starts_with(&format!("{prefix}/")) {
        key
    } else {
        format!("{prefix}/{key}")
    }
}
#[derive(Clone)]
pub struct LocalStorage {
    root: PathBuf,
    prefix: String,
}
impl LocalStorage {
    pub fn new(root: PathBuf, prefix: String) -> Self {
        Self { root, prefix }
    }
    pub fn path(&self, key: &str) -> Result<PathBuf, ContentError> {
        let key = prefixed_key(&self.prefix, key);
        if Path::new(&key).components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(ContentError::new(400, "Invalid path"));
        }
        let root = if self.root.is_absolute() {
            self.root.clone()
        } else {
            std::env::current_dir()?.join(&self.root)
        };
        let target = root.join(key);
        // Reject existing symlink ancestors escaping the configured root.
        let canonical_root = std::fs::canonicalize(&root).unwrap_or(root.clone());
        let mut ancestor = target.as_path();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| ContentError::new(400, "Invalid path"))?;
        }
        if root.exists() && !std::fs::canonicalize(ancestor)?.starts_with(&canonical_root) {
            return Err(ContentError::new(400, "Invalid path"));
        }
        Ok(target)
    }
}
/// Legacy Gateway file sidecars used MD5. Migration verifies this historical
/// field independently; SHA-256 remains the destination integrity digest.
pub fn legacy_content_md5(mut reader: impl std::io::Read) -> Result<String, ContentError> {
    use md5::{Digest, Md5};
    let mut hash = Md5::new();
    let mut buffer = [0; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn object_too_large() -> ContentError {
    ContentError::new(413, "Object exceeds maximum size")
}
async fn bounded_response(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, ContentError> {
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(object_too_large());
    }
    let mut content = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ContentError::storage())?
    {
        if chunk.len() > max.saturating_sub(content.len()) {
            return Err(object_too_large());
        }
        content.extend_from_slice(&chunk);
    }
    Ok(content)
}
/// The final target belongs to the caller. A unique sibling staging file belongs
/// to this guard until rename succeeds. No target contents or symlink are followed.
/// On cancellation/error Drop closes and removes the staging path (best effort if
/// the filesystem itself refuses deletion); no detached download task is spawned.
struct StagedDownload {
    path: PathBuf,
    file: Option<tokio::fs::File>,
    committed: bool,
}
impl StagedDownload {
    fn new(target: &Path) -> Result<Self, ContentError> {
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if target.file_name().is_none() {
            return Err(ContentError::new(400, "Invalid target path"));
        }
        let path = parent.join(format!(".gateway-download-{}.part", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // Synchronous create makes guard installation cancellation-atomic; all
        // potentially large reads and writes use bounded asynchronous I/O.
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file: Some(tokio::fs::File::from_std(file)),
            committed: false,
        })
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ContentError> {
        use tokio::io::AsyncWriteExt;
        self.file
            .as_mut()
            .ok_or_else(ContentError::storage)?
            .write_all(bytes)
            .await?;
        Ok(())
    }
    async fn finish(mut self, target: &Path) -> Result<(), ContentError> {
        use tokio::io::AsyncWriteExt;
        self.file
            .as_mut()
            .ok_or_else(ContentError::storage)?
            .flush()
            .await?;
        drop(self.file.take());
        // Same-directory rename is atomic; an existing caller temp file may be
        // replaced, but cannot be partially overwritten by a failed transfer.
        std::fs::rename(&self.path, target)?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for StagedDownload {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
async fn response_to_path(
    mut response: reqwest::Response,
    target: &Path,
    max_bytes: usize,
) -> Result<(), ContentError> {
    if response
        .content_length()
        .is_some_and(|n| n > max_bytes as u64)
    {
        return Err(object_too_large());
    }
    let mut output = StagedDownload::new(target)?;
    let mut copied = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ContentError::storage())?
    {
        if chunk.len() > max_bytes.saturating_sub(copied) {
            return Err(object_too_large());
        }
        output.write(&chunk).await?;
        copied += chunk.len();
    }
    output.finish(target).await
}
fn sidecar(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".meta");
    PathBuf::from(s)
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ContentError> + Send + 'static,
) -> Result<T, ContentError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ContentError::storage())?
}
impl ObjectStorage for LocalStorage {
    fn upload_if_absent<'a>(
        &'a self,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        mut metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let path = self.path(key)?;
            let mime = mime.to_owned();
            blocking(move || {
                use std::io::Write;
                std::fs::create_dir_all(path.parent().ok_or_else(ContentError::storage)?)?;
                // Reject orphan sidecars before claiming a new data path.
                if sidecar(&path).try_exists()? {
                    return Err(ContentError::new(409, "Object metadata already exists"));
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                file.write_all(&content)?;
                file.sync_all()?;
                metadata.insert("content_type".into(), mime);
                let mut meta = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(sidecar(&path))?;
                meta.write_all(&serde_json::to_vec(&metadata)?)?;
                meta.sync_all()?;
                Ok(())
            })
            .await
        })
    }

    fn local_path(&self, key: &str) -> Result<Option<PathBuf>, ContentError> {
        Self::new(self.root.clone(), String::new())
            .path(key)
            .map(Some)
    }
    fn upload<'a>(
        &'a self,
        key: &'a str,
        content: Vec<u8>,
        content_type: &'a str,
        mut metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String> {
        Box::pin(async move {
            let path = self.path(key)?;
            let mime = content_type.to_owned();
            blocking(move || {
                std::fs::create_dir_all(path.parent().ok_or_else(ContentError::storage)?)?;
                std::fs::write(&path, content)?;
                if !metadata.is_empty() {
                    metadata.entry("content_type".into()).or_insert(mime);
                    std::fs::write(sidecar(&path), serde_json::to_vec(&metadata)?)?;
                }
                Ok(format!("file://{}", path.display()))
            })
            .await
        })
    }
    fn download<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let path = self.path(key)?;
            blocking(move || Ok(std::fs::read(path)?)).await
        })
    }
    fn download_bounded<'a>(&'a self, key: &'a str, max: usize) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let path = self.path(key)?;
            blocking(move || {
                use std::io::Read;
                let file = std::fs::File::open(path)?;
                if file.metadata()?.len() > max as u64 {
                    return Err(object_too_large());
                }
                let mut content = Vec::new();
                file.take((max as u64).saturating_add(1))
                    .read_to_end(&mut content)?;
                if content.len() > max {
                    return Err(object_too_large());
                }
                Ok(content)
            })
            .await
        })
    }
    fn download_to_path<'a>(
        &'a self,
        key: &'a str,
        target: &'a Path,
        max_bytes: usize,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            use tokio::io::AsyncReadExt;
            let path = self.path(key)?;
            let mut source = tokio::fs::File::open(path).await?;
            if source.metadata().await?.len() > max_bytes as u64 {
                return Err(object_too_large());
            }
            let mut output = StagedDownload::new(target)?;
            let mut buffer = vec![0u8; 64 * 1024];
            let mut copied = 0usize;
            loop {
                let size = source.read(&mut buffer).await?;
                if size == 0 {
                    break;
                }
                if size > max_bytes.saturating_sub(copied) {
                    return Err(object_too_large());
                }
                output.write(&buffer[..size]).await?;
                copied += size;
            }
            output.finish(target).await
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> ContentFuture<'a, bool> {
        Box::pin(async move {
            let path = self.path(key)?;
            blocking(move || {
                if !path.exists() {
                    return Ok(false);
                }
                std::fs::remove_file(&path)?;
                if sidecar(&path).exists() {
                    std::fs::remove_file(sidecar(&path))?;
                }
                Ok(true)
            })
            .await
        })
    }
    fn delete_prefix<'a>(&'a self, prefix: &'a str) -> ContentFuture<'a, u64> {
        Box::pin(async move {
            let path = self.path(prefix)?;
            let directory = prefix.ends_with('/');
            blocking(move || {
                fn walk(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), ContentError> {
                    if !path.exists() {
                        return Ok(());
                    }
                    for entry in std::fs::read_dir(path)? {
                        let entry = entry?;
                        let ty = entry.file_type()?;
                        if ty.is_dir() {
                            walk(&entry.path(), out)?;
                        } else if ty.is_file() {
                            out.push(entry.path());
                        }
                    }
                    Ok(())
                }
                let mut files = Vec::new();
                if directory {
                    walk(&path, &mut files)?;
                } else if let Some(parent) = path.parent() {
                    if parent.exists() {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        for entry in std::fs::read_dir(parent)? {
                            let entry = entry?;
                            if entry.file_type()?.is_file()
                                && entry
                                    .file_name()
                                    .to_string_lossy()
                                    .starts_with(name.as_ref())
                            {
                                files.push(entry.path());
                            }
                        }
                    }
                }
                let mut count = 0;
                for file in files {
                    std::fs::remove_file(&file)?;
                    if !file.to_string_lossy().ends_with(".meta") {
                        count += 1;
                    }
                }
                Ok(count)
            })
            .await
        })
    }
    fn head<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Option<ObjectInfo>> {
        Box::pin(async move {
            let path = self.path(key)?;
            blocking(move || {
                if !path.is_file() {
                    return Ok(None);
                }
                let mut metadata: BTreeMap<String, String> = std::fs::read(sidecar(&path))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default();
                let content_type = metadata.remove("content_type");
                Ok(Some(ObjectInfo {
                    size_bytes: std::fs::metadata(path)?.len(),
                    content_type,
                    metadata,
                }))
            })
            .await
        })
    }
    fn download_url<'a>(
        &'a self,
        key: &'a str,
        _expiry: u32,
        _filename: Option<&'a str>,
    ) -> ContentFuture<'a, Option<String>> {
        Box::pin(async move { Ok(Some(format!("file://{}", self.path(key)?.display()))) })
    }
    fn upload_url<'a>(
        &'a self,
        _key: &'a str,
        _content_type: &'a str,
        _expiry: u32,
        _metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, Option<BTreeMap<String, String>>> {
        Box::pin(async { Ok(None) })
    }
}
/// Cloud SDK adapter with the same source backend operations. The transport owns
/// signing and credentials and returns provider-normalized errors; no public URL
/// is accepted as a download input.
pub trait CloudObjectTransport: Send + Sync {
    fn put_if_absent<'a>(
        &'a self,
        _bucket: &'a str,
        _key: &'a str,
        _content: Vec<u8>,
        _mime: &'a str,
        _metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async {
            Err(ContentError::new(
                501,
                "Conditional object creation is unsupported",
            ))
        })
    }

    fn upload_headers(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    fn put<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()>;
    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> ContentFuture<'a, Vec<u8>>;
    fn get_bounded<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        max: usize,
    ) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let content = self.get(bucket, key).await?;
            if content.len() > max {
                return Err(object_too_large());
            }
            Ok(content)
        })
    }
    fn get_to_path<'a>(
        &'a self,
        _bucket: &'a str,
        _key: &'a str,
        _target: &'a Path,
        _max_bytes: usize,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async {
            Err(ContentError::new(
                501,
                "Streaming download is not supported",
            ))
        })
    }
    fn remove<'a>(&'a self, bucket: &'a str, keys: Vec<String>) -> ContentFuture<'a, u64>;
    fn list<'a>(
        &'a self,
        bucket: &'a str,
        prefix: &'a str,
        cursor: Option<String>,
    ) -> ContentFuture<'a, (Vec<String>, Option<String>)>;
    fn head<'a>(&'a self, bucket: &'a str, key: &'a str) -> ContentFuture<'a, Option<ObjectInfo>>;
    fn presign<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        method: &'a str,
        expiry: u32,
        headers: BTreeMap<String, String>,
        query: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String>;
}
pub struct CloudStorage {
    pub transport: Arc<dyn CloudObjectTransport>,
    pub bucket: String,
    pub prefix: String,
    pub public_base: String,
    pub s3: bool,
}
impl ObjectStorage for CloudStorage {
    fn list_keys<'a>(
        &'a self,
        prefix: &'a str,
        cursor: Option<String>,
    ) -> ContentFuture<'a, (Vec<String>, Option<String>)> {
        Box::pin(async move {
            let physical = prefixed_key(&self.prefix, prefix);
            let (keys, next) = self
                .transport
                .list(&self.bucket, &physical, cursor.clone())
                .await?;
            if keys.len() > 1000 || next.as_ref().is_some_and(|v| Some(v) == cursor.as_ref()) {
                return Err(ContentError::storage());
            }
            let base = self.prefix.trim_matches('/');
            let mut logical = Vec::with_capacity(keys.len());
            for key in keys {
                if !key.starts_with(&physical) {
                    return Err(ContentError::storage());
                }
                logical.push(if base.is_empty() {
                    key
                } else {
                    key.strip_prefix(&format!("{base}/"))
                        .ok_or_else(ContentError::storage)?
                        .to_owned()
                });
            }
            Ok((logical, next))
        })
    }

    fn upload_if_absent<'a>(
        &'a self,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            self.transport
                .put_if_absent(
                    &self.bucket,
                    &prefixed_key(&self.prefix, key),
                    content,
                    mime,
                    metadata,
                )
                .await
        })
    }

    fn upload<'a>(
        &'a self,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String> {
        Box::pin(async move {
            let key = prefixed_key(&self.prefix, key);
            self.transport
                .put(&self.bucket, &key, content, mime, metadata)
                .await?;
            Ok(format!(
                "{}/{}",
                self.public_base.trim_end_matches('/'),
                key.split('/')
                    .map(percent_encode)
                    .collect::<Vec<_>>()
                    .join("/")
            ))
        })
    }
    fn download<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.transport
                .get(&self.bucket, &prefixed_key(&self.prefix, key))
                .await
        })
    }
    fn download_bounded<'a>(&'a self, key: &'a str, max: usize) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.transport
                .get_bounded(&self.bucket, &prefixed_key(&self.prefix, key), max)
                .await
        })
    }
    fn download_to_path<'a>(
        &'a self,
        key: &'a str,
        target: &'a Path,
        max_bytes: usize,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            self.transport
                .get_to_path(
                    &self.bucket,
                    &prefixed_key(&self.prefix, key),
                    target,
                    max_bytes,
                )
                .await
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> ContentFuture<'a, bool> {
        Box::pin(async move {
            Ok(self
                .transport
                .remove(&self.bucket, vec![prefixed_key(&self.prefix, key)])
                .await
                .is_ok())
        })
    }
    fn delete_prefix<'a>(&'a self, prefix: &'a str) -> ContentFuture<'a, u64> {
        Box::pin(async move {
            let prefix = prefixed_key(&self.prefix, prefix);
            let mut cursor = None;
            let mut total = 0;
            loop {
                let (keys, next) = self.transport.list(&self.bucket, &prefix, cursor).await?;
                for chunk in keys.chunks(1000) {
                    total += self.transport.remove(&self.bucket, chunk.to_vec()).await?;
                }
                cursor = next;
                if cursor.is_none() {
                    break;
                }
            }
            Ok(total)
        })
    }
    fn head<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Option<ObjectInfo>> {
        Box::pin(async move {
            self.transport
                .head(&self.bucket, &prefixed_key(&self.prefix, key))
                .await
        })
    }
    fn download_url<'a>(
        &'a self,
        key: &'a str,
        expiry: u32,
        filename: Option<&'a str>,
    ) -> ContentFuture<'a, Option<String>> {
        Box::pin(async move {
            let key = prefixed_key(&self.prefix, key);
            if !self.s3 {
                return Ok(Some(format!(
                    "{}/{}",
                    self.public_base.trim_end_matches('/'),
                    key.split('/')
                        .map(percent_encode)
                        .collect::<Vec<_>>()
                        .join("/")
                )));
            }
            let mut query = BTreeMap::new();
            if let Some(filename) = filename {
                query.insert(
                    "response-content-disposition".into(),
                    if filename.is_ascii() {
                        format!("attachment; filename=\"{filename}\"")
                    } else {
                        format!(
                            "attachment; filename=\"{}\"; filename*=UTF-8''{}",
                            filename
                                .chars()
                                .map(|c| if c.is_ascii() { c } else { '_' })
                                .collect::<String>(),
                            percent_encode(filename)
                        )
                    },
                );
            }
            Ok(Some(
                self.transport
                    .presign(&self.bucket, &key, "GET", expiry, BTreeMap::new(), query)
                    .await?,
            ))
        })
    }
    fn upload_url<'a>(
        &'a self,
        key: &'a str,
        mime: &'a str,
        expiry: u32,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, Option<BTreeMap<String, String>>> {
        Box::pin(async move {
            if !self.s3 {
                return Ok(None);
            }
            let expiry = expiry.clamp(1, 900);
            let mut headers = self.transport.upload_headers();
            headers.insert("Content-Type".into(), mime.into());
            headers.extend(s3_metadata_headers(metadata)?);
            let url = self
                .transport
                .presign(
                    &self.bucket,
                    &prefixed_key(&self.prefix, key),
                    "PUT",
                    expiry,
                    headers.clone(),
                    BTreeMap::new(),
                )
                .await?;
            let mut result = BTreeMap::from([
                ("upload_url".into(), url),
                ("method".into(), "PUT".into()),
                ("headers".into(), serde_json::to_string(&headers)?),
            ]);
            result.insert("expires_in".into(), expiry.to_string());
            Ok(Some(result))
        })
    }
}

/// S3-compatible HTTP implementation of the transport. Credentials are provided
/// by composition; this module never reads environment files or logs signatures.
pub struct S3HttpTransport {
    client: reqwest::Client,
    region: String,
    access_key: String,
    secret_key: Vec<u8>,
    endpoint: Option<reqwest::Url>,
    encryption_headers: BTreeMap<String, String>,
}
impl S3HttpTransport {
    pub fn new(
        client: reqwest::Client,
        region: String,
        access_key: String,
        secret_key: Vec<u8>,
        endpoint: Option<String>,
    ) -> Result<Self, ContentError> {
        let endpoint = endpoint
            .map(|s| {
                reqwest::Url::parse(&s).map_err(|_| ContentError::new(400, "Invalid S3 endpoint"))
            })
            .transpose()?;
        if endpoint.as_ref().is_some_and(|u| {
            !matches!(u.scheme(), "http" | "https")
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
        }) {
            return Err(ContentError::new(400, "Invalid S3 endpoint"));
        }
        Ok(Self {
            client,
            region,
            access_key,
            secret_key,
            endpoint,
            encryption_headers: BTreeMap::new(),
        })
    }
    /// Empty mode uses bucket policy/default encryption, including non-AWS providers.
    pub fn with_encryption(
        mut self,
        mode: &str,
        kms_key: Option<&str>,
    ) -> Result<Self, ContentError> {
        match mode {
            "" if kms_key.is_none() => {}
            "AES256" if kms_key.is_none() => {
                self.encryption_headers
                    .insert("x-amz-server-side-encryption".into(), mode.into());
            }
            "aws:kms" => {
                self.encryption_headers
                    .insert("x-amz-server-side-encryption".into(), mode.into());
                if let Some(key) = kms_key {
                    if key.is_empty() || !key.is_ascii() || key.chars().any(char::is_control) {
                        return Err(ContentError::new(400, "Invalid SSE key"));
                    }
                    self.encryption_headers.insert(
                        "x-amz-server-side-encryption-aws-kms-key-id".into(),
                        key.into(),
                    );
                }
            }
            _ => return Err(ContentError::new(400, "Invalid SSE configuration")),
        }
        Ok(self)
    }
    fn sign(
        &self,
        bucket: &str,
        key: &str,
        method: &str,
        expiry: u32,
        headers: &BTreeMap<String, String>,
        query: BTreeMap<String, String>,
    ) -> Result<String, ContentError> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        fn hmac(key: &[u8], data: &str) -> Vec<u8> {
            let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key");
            mac.update(data.as_bytes());
            mac.finalize().into_bytes().to_vec()
        }
        let mut url = if let Some(base) = &self.endpoint {
            let mut url = base.clone();
            let path = format!(
                "{}/{}/{}",
                url.path().trim_end_matches('/'),
                percent_encode(bucket),
                key.split('/')
                    .map(percent_encode)
                    .collect::<Vec<_>>()
                    .join("/")
            );
            url.set_path(&path);
            url
        } else {
            let mut url = reqwest::Url::parse(&format!(
                "https://{bucket}.s3.{}.amazonaws.com",
                self.region
            ))
            .map_err(|_| ContentError::new(400, "Invalid S3 bucket"))?;
            url.set_path(&format!(
                "/{}",
                key.split('/')
                    .map(percent_encode)
                    .collect::<Vec<_>>()
                    .join("/")
            ));
            url
        };
        let host = url.host_str().ok_or_else(ContentError::storage)?;
        let host = if let Some(port) = url.port() {
            format!("{host}:{port}")
        } else {
            host.into()
        };
        let mut canonical_headers: BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| {
                (
                    k.to_ascii_lowercase(),
                    v.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect();
        canonical_headers.insert("host".into(), host);
        let signed_headers = canonical_headers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(";");
        let now = chrono::Utc::now();
        let date = now.format("%Y%m%d").to_string();
        let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let mut query = query;
        query.insert("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into());
        query.insert(
            "X-Amz-Credential".into(),
            format!("{}/{scope}", self.access_key),
        );
        query.insert("X-Amz-Date".into(), timestamp.clone());
        query.insert("X-Amz-Expires".into(), expiry.clamp(1, 900).to_string());
        query.insert("X-Amz-SignedHeaders".into(), signed_headers.clone());
        let mut encoded = query
            .into_iter()
            .map(|(k, v)| (percent_encode(&k), percent_encode(&v)))
            .collect::<Vec<_>>();
        encoded.sort();
        let query = encoded
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let headers = canonical_headers
            .into_iter()
            .map(|(k, v)| format!("{k}:{v}\n"))
            .collect::<String>();
        let canonical = format!(
            "{method}\n{}\n{query}\n{headers}\n{signed_headers}\nUNSIGNED-PAYLOAD",
            url.path()
        );
        let input = format!(
            "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            super::sha256(canonical.as_bytes())
        );
        let mut secret = b"AWS4".to_vec();
        secret.extend_from_slice(&self.secret_key);
        let key = hmac(
            &hmac(&hmac(&hmac(&secret, &date), &self.region), "s3"),
            "aws4_request",
        );
        let signature = hmac(&key, &input)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        url.set_query(Some(&format!("{query}&X-Amz-Signature={signature}")));
        url.set_fragment(None);
        Ok(url.into())
    }
    async fn request(
        &self,
        bucket: &str,
        key: &str,
        method: reqwest::Method,
        body: Option<Vec<u8>>,
        headers: BTreeMap<String, String>,
        query: BTreeMap<String, String>,
    ) -> Result<reqwest::Response, ContentError> {
        let url = self.sign(bucket, key, method.as_str(), 3600, &headers, query)?;
        let mut request = self.client.request(method, url);
        for (k, v) in headers {
            request = request.header(k, v);
        }
        if let Some(body) = body {
            request = request.body(body)
        }
        let response = request.send().await.map_err(|_| ContentError::storage())?;
        if !response.status().is_success() {
            return Err(ContentError::new(
                match response.status().as_u16() {
                    // Authorization failures must not become retryable storage outages.
                    status @ (401 | 403 | 404 | 409 | 412) => status,
                    _ => 503,
                },
                "Object storage request failed",
            ));
        }
        Ok(response)
    }
}
impl CloudObjectTransport for S3HttpTransport {
    fn put_if_absent<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let mut headers = self.encryption_headers.clone();
            headers.insert("content-type".into(), mime.into());
            headers.insert("if-none-match".into(), "*".into());
            headers.extend(s3_metadata_headers(metadata)?);
            self.request(
                bucket,
                key,
                reqwest::Method::PUT,
                Some(content),
                headers,
                BTreeMap::new(),
            )
            .await?;
            Ok(())
        })
    }

    fn upload_headers(&self) -> BTreeMap<String, String> {
        self.encryption_headers.clone()
    }
    fn put<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let mut headers = self.encryption_headers.clone();
            headers.insert("content-type".into(), mime.into());
            headers.extend(s3_metadata_headers(metadata)?);
            self.request(
                bucket,
                key,
                reqwest::Method::PUT,
                Some(content),
                headers,
                BTreeMap::new(),
            )
            .await?;
            Ok(())
        })
    }
    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            Ok(self
                .request(
                    bucket,
                    key,
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?
                .bytes()
                .await
                .map_err(|_| ContentError::storage())?
                .to_vec())
        })
    }
    fn get_bounded<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        max: usize,
    ) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            bounded_response(
                self.request(
                    bucket,
                    key,
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?,
                max,
            )
            .await
        })
    }
    fn get_to_path<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        target: &'a Path,
        max_bytes: usize,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let response = self
                .request(
                    bucket,
                    key,
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?;
            response_to_path(response, target, max_bytes).await
        })
    }
    fn remove<'a>(&'a self, bucket: &'a str, keys: Vec<String>) -> ContentFuture<'a, u64> {
        Box::pin(async move {
            if keys.is_empty() {
                return Ok(0);
            }
            if keys.len() == 1 {
                self.request(
                    bucket,
                    &keys[0],
                    reqwest::Method::DELETE,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?;
                return Ok(1);
            }
            use base64::{Engine, engine::general_purpose::STANDARD};
            use md5::{Digest, Md5};
            let mut count = 0;
            for batch in keys.chunks(1000) {
                let xml = format!(
                    "<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{}</Delete>",
                    batch
                        .iter()
                        .map(|key| format!(
                            "<Object><Key>{}</Key></Object>",
                            quick_xml::escape::escape(key)
                        ))
                        .collect::<String>()
                );
                let headers = BTreeMap::from([
                    (
                        "content-md5".into(),
                        STANDARD.encode(Md5::digest(xml.as_bytes())),
                    ),
                    ("content-type".into(), "application/xml".into()),
                ]);
                let response = self
                    .request(
                        bucket,
                        "",
                        reqwest::Method::POST,
                        Some(xml.into_bytes()),
                        headers,
                        BTreeMap::from([("delete".into(), String::new())]),
                    )
                    .await?
                    .text()
                    .await
                    .map_err(|_| ContentError::storage())?;
                let mut reader = quick_xml::Reader::from_str(&response);
                loop {
                    match reader.read_event().map_err(|_| ContentError::storage())? {
                        quick_xml::events::Event::Start(e)
                            if e.local_name().as_ref() == b"Error" =>
                        {
                            return Err(ContentError::new(
                                503,
                                "S3 prefix deletion was only partially applied",
                            ));
                        }
                        quick_xml::events::Event::Eof => break,
                        _ => (),
                    }
                }
                count += batch.len() as u64;
            }
            Ok(count)
        })
    }
    fn list<'a>(
        &'a self,
        bucket: &'a str,
        prefix: &'a str,
        cursor: Option<String>,
    ) -> ContentFuture<'a, (Vec<String>, Option<String>)> {
        Box::pin(async move {
            let mut query = BTreeMap::from([
                ("list-type".into(), "2".into()),
                ("prefix".into(), prefix.into()),
                ("max-keys".into(), "1000".into()),
            ]);
            if let Some(cursor) = cursor {
                query.insert("continuation-token".into(), cursor);
            }
            let xml = self
                .request(
                    bucket,
                    "",
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    query,
                )
                .await?
                .text()
                .await
                .map_err(|_| ContentError::storage())?;
            list_xml(&xml, "NextContinuationToken")
        })
    }
    fn head<'a>(&'a self, bucket: &'a str, key: &'a str) -> ContentFuture<'a, Option<ObjectInfo>> {
        Box::pin(async move {
            let response = match self
                .request(
                    bucket,
                    key,
                    reqwest::Method::HEAD,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) if e.status == 404 => return Ok(None),
                Err(e) => return Err(e),
            };
            let h = response.headers();
            let size = h
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .ok_or_else(ContentError::storage)?;
            let content_type = h
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let metadata: BTreeMap<String, String> = h
                .iter()
                .filter_map(|(k, v)| {
                    Some((
                        k.as_str().strip_prefix("x-amz-meta-")?.to_owned(),
                        v.to_str().ok()?.to_owned(),
                    ))
                })
                .collect();
            Ok(Some(ObjectInfo {
                size_bytes: size,
                content_type,
                metadata: decode_s3_metadata(metadata),
            }))
        })
    }
    fn presign<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        method: &'a str,
        expiry: u32,
        headers: BTreeMap<String, String>,
        query: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String> {
        Box::pin(async move {
            let mut headers = headers;
            if method == "PUT" {
                headers.extend(self.encryption_headers.clone());
            }
            self.sign(bucket, key, method, expiry, &headers, query)
        })
    }
}

/// OSS v1 auth used by oss2.Auth, with virtual-host bucket addressing. Endpoint
/// and credentials are supplied by host configuration, never by an API caller.
pub struct OssHttpTransport {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    access_key: String,
    secret_key: Vec<u8>,
}
impl OssHttpTransport {
    pub fn new(
        client: reqwest::Client,
        endpoint: &str,
        access_key: String,
        secret_key: Vec<u8>,
    ) -> Result<Self, ContentError> {
        let endpoint = if endpoint.contains("://") {
            endpoint.to_owned()
        } else {
            format!("http://{endpoint}")
        };
        let endpoint = reqwest::Url::parse(&endpoint)
            .map_err(|_| ContentError::new(400, "Invalid OSS endpoint"))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(ContentError::new(400, "Invalid OSS endpoint"));
        }
        Ok(Self {
            client,
            endpoint,
            access_key,
            secret_key,
        })
    }
    fn url(
        &self,
        bucket: &str,
        key: &str,
        query: &BTreeMap<String, String>,
    ) -> Result<reqwest::Url, ContentError> {
        let mut url = self.endpoint.clone();
        let host = url.host_str().ok_or_else(ContentError::storage)?;
        let ip = host.parse::<std::net::IpAddr>().is_ok();
        if ip {
            url.set_path(&format!(
                "/{bucket}/{}",
                key.split('/')
                    .map(percent_encode)
                    .collect::<Vec<_>>()
                    .join("/")
            ));
        } else {
            url.set_host(Some(&format!("{bucket}.{host}")))
                .map_err(|_| ContentError::storage())?;
            url.set_path(&format!(
                "/{}",
                key.split('/')
                    .map(percent_encode)
                    .collect::<Vec<_>>()
                    .join("/")
            ));
        }
        url.set_query(None);
        url.query_pairs_mut().extend_pairs(query);
        url.set_fragment(None);
        Ok(url)
    }
    async fn request(
        &self,
        bucket: &str,
        key: &str,
        method: reqwest::Method,
        body: Option<Vec<u8>>,
        mut headers: BTreeMap<String, String>,
        query: BTreeMap<String, String>,
    ) -> Result<reqwest::Response, ContentError> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use hmac::{Hmac, Mac};
        use sha1::Sha1;
        let date = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let mime = headers.get("content-type").cloned().unwrap_or_default();
        let md5 = headers.get("content-md5").cloned().unwrap_or_default();
        let canonical = headers
            .iter()
            .filter(|(k, _)| k.starts_with("x-oss-"))
            .map(|(k, v)| format!("{k}:{}\n", v.trim()))
            .collect::<String>();
        let resource = format!("/{bucket}/{key}");
        let input = format!(
            "{}\n{md5}\n{mime}\n{date}\n{canonical}{resource}",
            method.as_str()
        );
        let mut mac = Hmac::<Sha1>::new_from_slice(&self.secret_key).expect("HMAC key");
        mac.update(input.as_bytes());
        let signature = STANDARD.encode(mac.finalize().into_bytes());
        headers.insert("date".into(), date);
        headers.insert(
            "authorization".into(),
            format!("OSS {}:{signature}", self.access_key),
        );
        let mut request = self.client.request(method, self.url(bucket, key, &query)?);
        for (k, v) in headers {
            request = request.header(k, v)
        }
        if let Some(body) = body {
            request = request.body(body)
        }
        let response = request.send().await.map_err(|_| ContentError::storage())?;
        if !response.status().is_success() {
            return Err(ContentError::new(
                match response.status().as_u16() {
                    // Authorization failures must not become retryable storage outages.
                    status @ (401 | 403 | 404) => status,
                    _ => 503,
                },
                "Object storage request failed",
            ));
        }
        Ok(response)
    }
}
impl CloudObjectTransport for OssHttpTransport {
    fn put<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        content: Vec<u8>,
        mime: &'a str,
        metadata: BTreeMap<String, String>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let mut headers = BTreeMap::from([("content-type".into(), mime.into())]);
            for (k, v) in metadata {
                headers.insert(format!("x-oss-meta-{}", k.to_lowercase()), v);
            }
            self.request(
                bucket,
                key,
                reqwest::Method::PUT,
                Some(content),
                headers,
                BTreeMap::new(),
            )
            .await?;
            Ok(())
        })
    }
    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            Ok(self
                .request(
                    bucket,
                    key,
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?
                .bytes()
                .await
                .map_err(|_| ContentError::storage())?
                .to_vec())
        })
    }
    fn get_bounded<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        max: usize,
    ) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            bounded_response(
                self.request(
                    bucket,
                    key,
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?,
                max,
            )
            .await
        })
    }
    fn get_to_path<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        target: &'a Path,
        max_bytes: usize,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let response = self
                .request(
                    bucket,
                    key,
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?;
            response_to_path(response, target, max_bytes).await
        })
    }
    fn remove<'a>(&'a self, bucket: &'a str, keys: Vec<String>) -> ContentFuture<'a, u64> {
        Box::pin(async move {
            let mut count = 0;
            for key in keys {
                self.request(
                    bucket,
                    &key,
                    reqwest::Method::DELETE,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await?;
                count += 1
            }
            Ok(count)
        })
    }
    fn list<'a>(
        &'a self,
        bucket: &'a str,
        prefix: &'a str,
        cursor: Option<String>,
    ) -> ContentFuture<'a, (Vec<String>, Option<String>)> {
        Box::pin(async move {
            let mut query = BTreeMap::from([
                ("prefix".into(), prefix.into()),
                ("max-keys".into(), "1000".into()),
            ]);
            if let Some(marker) = cursor {
                query.insert("marker".into(), marker);
            }
            let xml = self
                .request(
                    bucket,
                    "",
                    reqwest::Method::GET,
                    None,
                    BTreeMap::new(),
                    query,
                )
                .await?
                .text()
                .await
                .map_err(|_| ContentError::storage())?;
            list_xml(&xml, "NextMarker")
        })
    }
    fn head<'a>(&'a self, bucket: &'a str, key: &'a str) -> ContentFuture<'a, Option<ObjectInfo>> {
        Box::pin(async move {
            let response = match self
                .request(
                    bucket,
                    key,
                    reqwest::Method::HEAD,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) if e.status == 404 => return Ok(None),
                Err(e) => return Err(e),
            };
            object_head(response.headers(), "x-oss-meta-").map(Some)
        })
    }
    fn presign<'a>(
        &'a self,
        bucket: &'a str,
        key: &'a str,
        _method: &'a str,
        _expiry: u32,
        _headers: BTreeMap<String, String>,
        query: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String> {
        Box::pin(async move { Ok(self.url(bucket, key, &query)?.into()) })
    }
}
fn list_xml(xml: &str, next_name: &str) -> Result<(Vec<String>, Option<String>), ContentError> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut field = String::new();
    let mut value = String::new();
    let mut keys = vec![];
    let mut next = None;
    loop {
        match reader.read_event().map_err(|_| ContentError::storage())? {
            quick_xml::events::Event::Start(e) => {
                field = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                value.clear();
            }
            quick_xml::events::Event::Text(e) => {
                let decoded = e.decode().map_err(|_| ContentError::storage())?;
                value.push_str(
                    &quick_xml::escape::unescape(&decoded).map_err(|_| ContentError::storage())?,
                );
            }
            quick_xml::events::Event::GeneralRef(e) => {
                value.push_str(&super::attachments::xml_reference(
                    &e.decode().map_err(|_| ContentError::storage())?,
                )?)
            }
            quick_xml::events::Event::End(_) => {
                if field == "Key" {
                    keys.push(value.clone())
                } else if field == next_name && !value.is_empty() {
                    next = Some(value.clone())
                }
                field.clear();
                value.clear();
            }
            quick_xml::events::Event::DocType(_) => return Err(ContentError::storage()),
            quick_xml::events::Event::Eof => break,
            _ => (),
        }
    }
    Ok((keys, next))
}
fn object_head(h: &reqwest::header::HeaderMap, prefix: &str) -> Result<ObjectInfo, ContentError> {
    let size = h
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let content_type = h
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let metadata = h
        .iter()
        .filter_map(|(k, v)| {
            Some((
                k.as_str().strip_prefix(prefix)?.to_owned(),
                v.to_str().ok()?.to_owned(),
            ))
        })
        .collect();
    Ok(ObjectInfo {
        size_bytes: size,
        content_type,
        metadata,
    })
}
fn s3_metadata(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    value
        .nfkd()
        .filter(char::is_ascii)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const S3_METADATA_FORMAT: &str = "agentforge-internal-metadata-format";
fn s3_metadata_headers(
    metadata: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, ContentError> {
    let mut headers = BTreeMap::new();
    let mut encoded = false;
    for (key, value) in metadata {
        let key = s3_metadata(&key);
        if key.eq_ignore_ascii_case(S3_METADATA_FORMAT) {
            return Err(ContentError::new(400, "Reserved object metadata key"));
        }
        let wire = s3_metadata_value(&value);
        encoded |= wire != value;
        headers.insert(format!("x-amz-meta-{key}"), wire);
    }
    if encoded {
        headers.insert(
            format!("x-amz-meta-{S3_METADATA_FORMAT}"),
            "base64-v1".into(),
        );
    }
    Ok(headers)
}
fn decode_s3_metadata(mut metadata: BTreeMap<String, String>) -> BTreeMap<String, String> {
    // Legacy values, even those matching our new value prefix, remain untouched.
    if metadata
        .get(S3_METADATA_FORMAT)
        .is_some_and(|v| v == "base64-v1")
    {
        metadata.remove(S3_METADATA_FORMAT);
        for value in metadata.values_mut() {
            *value = decode_s3_metadata_value(value);
        }
    }
    metadata
}
/// Preserve Unicode and significant whitespace without invalid HTTP header bytes.
fn s3_metadata_value(value: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    if value.starts_with("af-b64-v1:")
        || s3_metadata(value) != value
        || value.chars().any(char::is_control)
    {
        format!("af-b64-v1:{}", STANDARD.encode(value.as_bytes()))
    } else {
        value.to_owned()
    }
}
fn decode_s3_metadata_value(value: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    value
        .strip_prefix("af-b64-v1:")
        .and_then(|v| STANDARD.decode(v).ok())
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_else(|| value.to_owned())
}
#[cfg(test)]
mod shared_storage_tests {
    use super::*;
    #[test]
    fn metadata_values_roundtrip_without_loss() {
        let legacy = BTreeMap::from([("filename".into(), "af-b64-v1:YWJj".into())]);
        assert_eq!(decode_s3_metadata(legacy.clone()), legacy);
        for value in [
            "filename.pdf",
            "你好.pdf",
            " two  spaces ",
            "af-b64-v1:YWJj",
            "line\nbreak",
        ] {
            assert_eq!(decode_s3_metadata_value(&s3_metadata_value(value)), value);
        }
    }
    #[tokio::test]
    async fn short_lived_presigns_include_sse_and_matching_upload_headers() {
        let transport = S3HttpTransport::new(
            reqwest::Client::new(),
            "region".into(),
            "key".into(),
            b"secret".to_vec(),
            Some("https://objects.example".into()),
        )
        .unwrap()
        .with_encryption("AES256", None)
        .unwrap();
        let store = CloudStorage {
            transport: Arc::new(transport),
            bucket: "bucket".into(),
            prefix: "env".into(),
            public_base: String::new(),
            s3: true,
        };
        let result = store
            .upload_url("uploads/tenant/file", "text/plain", 3600, BTreeMap::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["expires_in"], "900");
        assert!(result["upload_url"].contains("X-Amz-Expires=900"));
        assert!(result["upload_url"].contains("x-amz-server-side-encryption"));
        assert!(result["headers"].contains("AES256"));
        let url = store.download_url("file", 0, None).await.unwrap().unwrap();
        assert!(url.contains("X-Amz-Expires=1"));
    }
    async fn mock_response(response: String) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            let mut buf = [0; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (endpoint, task)
    }
    #[tokio::test]
    async fn conditional_put_cannot_overwrite_and_head_decodes_metadata() {
        let (endpoint, request) = mock_response(
            "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
        )
        .await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let transport = S3HttpTransport::new(
            client.clone(),
            "region".into(),
            "key".into(),
            b"secret".to_vec(),
            Some(endpoint),
        )
        .unwrap()
        .with_encryption("AES256", None)
        .unwrap();
        let result = transport
            .put_if_absent(
                "bucket",
                "env/uploads/tenant-a/file",
                b"hello".to_vec(),
                "text/plain",
                BTreeMap::from([("filename".into(), "你好.txt".into())]),
            )
            .await;
        assert_eq!(result.unwrap_err().status, 412);
        let request = request.await.unwrap();
        assert!(request.contains("if-none-match: *"));
        assert!(request.contains("x-amz-server-side-encryption: AES256"));
        assert!(request.contains(&format!(
            "x-amz-meta-filename: {}",
            s3_metadata_value("你好.txt")
        )));
        let (endpoint, request) = mock_response(format!("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: text/plain\r\nx-amz-meta-filename: {}\r\nx-amz-meta-agentforge-internal-metadata-format: base64-v1\r\nConnection: close\r\n\r\n", s3_metadata_value("你好.txt"))).await;
        let transport = S3HttpTransport::new(
            client,
            "region".into(),
            "key".into(),
            b"secret".to_vec(),
            Some(endpoint),
        )
        .unwrap();
        let head = transport
            .head("bucket", "env/uploads/tenant-a/file")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head.size_bytes, 5);
        assert_eq!(head.metadata["filename"], "你好.txt");
        request.await.unwrap();
    }
    #[test]
    fn invalid_sse_is_rejected_without_io() {
        assert!(
            S3HttpTransport::new(
                reqwest::Client::new(),
                "region".into(),
                "key".into(),
                vec![],
                None
            )
            .unwrap()
            .with_encryption("AES256", Some("kms"))
            .is_err()
        );
    }
}
