use super::artifacts::mime_type;
use super::storage::{LocalStorage, ObjectStorage};
use super::{ContentAccess, ContentError, ContentIdentity, hex_id, sha256};
use axum::{
    Json, Router,
    extract::{Multipart, Path, Query, State},
    http::HeaderMap,
    routing::{delete, get, post},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Instant,
};
const EXTENSIONS: &[&str] = &[
    "bmp", "csv", "doc", "docx", "gif", "jpeg", "jpg", "md", "pdf", "png", "txt", "webp", "xlsx",
];
pub fn validate_user_id(id: &str) -> Result<(), ContentError> {
    if id.is_empty() {
        return Err(ContentError::new(400, "User ID is required"));
    }
    if id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ContentError::new(
            400,
            "Invalid user ID format. Only alphanumeric, underscore, and hyphen allowed (max 64 chars)",
        ));
    }
    Ok(())
}
pub fn validate_file_id(id: &str) -> Result<(), ContentError> {
    if id.is_empty() {
        return Err(ContentError::new(400, "File ID is required"));
    }
    if !hex_id(id, "", 8) {
        return Err(ContentError::new(
            400,
            "Invalid file ID format. Must be exactly 8 hexadecimal characters",
        ));
    }
    Ok(())
}
fn extension(name: &str) -> String {
    FsPath::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}
pub fn validate_extension(name: &str) -> Result<String, ContentError> {
    if name.is_empty() {
        return Err(ContentError::new(400, "Filename is required"));
    }
    let ext = extension(name);
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err(ContentError::new(
            400,
            format!(
                "File type not allowed: .{ext}. Allowed types: {}",
                EXTENSIONS
                    .iter()
                    .map(|s| format!(".{s}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Ok(ext)
}
#[derive(Clone)]
pub struct FileStorage {
    pub backend: Arc<dyn ObjectStorage>,
}
impl FileStorage {
    pub async fn upload(
        &self,
        user: &ContentIdentity,
        name: &str,
        content: Vec<u8>,
        max: usize,
    ) -> Result<Value, ContentError> {
        self.upload_reader(user, name, &content[..], max).await
    }
    pub async fn upload_reader<R: tokio::io::AsyncRead + Unpin + Send>(
        &self,
        user: &ContentIdentity,
        name: &str,
        mut reader: R,
        max: usize,
    ) -> Result<Value, ContentError> {
        use md5::{Digest, Md5};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        validate_user_id(&user.user_id)?;
        if !user.tenant_id.is_empty() {
            validate_user_id(&user.tenant_id)?
        }
        let ext = validate_extension(name)?;
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        let filename = format!("{id}_{}.{}", Utc::now().format("%Y%m%d_%H%M%S"), ext);
        let key = if user.tenant_id.is_empty() {
            format!("uploads/{}/{filename}", user.user_id)
        } else {
            format!("uploads/{}/{}/{filename}", user.tenant_id, user.user_id)
        };
        let mut metadata = BTreeMap::from([
            ("user_id".into(), user.user_id.clone()),
            ("file_id".into(), id.clone()),
            ("original_filename".into(), name.into()),
        ]);
        let mut size = 0usize;
        let mut chunk = vec![0u8; 64 * 1024];
        let oversized = || {
            ContentError::new(
                413,
                format!(
                    "File exceeds maximum size of {:.1}MB",
                    max as f64 / 1048576.0
                ),
            )
        };
        if let Some(path) = self.backend.local_path(&key)? {
            tokio::fs::create_dir_all(path.parent().ok_or_else(ContentError::storage)?).await?;
            let mut temporary = path.as_os_str().to_os_string();
            temporary.push(".tmp");
            let temporary = TempUpload {
                path: PathBuf::from(temporary),
                name: String::new(),
            };
            let mut output = tokio::fs::File::create(&temporary.path).await?;
            let mut hash = Md5::new();
            loop {
                let n = reader.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                if n > max.saturating_sub(size) {
                    return Err(oversized());
                }
                size += n;
                hash.update(&chunk[..n]);
                output.write_all(&chunk[..n]).await?;
            }
            drop(output);
            tokio::fs::rename(&temporary.path, &path).await?;
            metadata.insert("content_hash".into(), format!("{:x}", hash.finalize()));
            metadata.insert("content_type".into(), mime_type(&ext).into());
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(".meta");
            tokio::fs::write(sidecar, super::python_json(&json!(metadata), true, true)).await?;
        } else {
            let mut bytes = Vec::new();
            loop {
                let n = reader.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                if n > max.saturating_sub(size) {
                    return Err(oversized());
                }
                size += n;
                bytes.extend_from_slice(&chunk[..n]);
            }
            metadata.insert("content_hash".into(), sha256(&bytes));
            self.backend
                .upload(&key, bytes, mime_type(&ext), metadata)
                .await?;
        }
        let path = format!("/{key}");
        Ok(
            json!({"file_id":id,"file_path":path,"filename":name,"size_bytes":size,"mime_type":mime_type(&ext),"uploaded_at":Utc::now(),"message":format!("File uploaded successfully. Use this path in your message: {path}")}),
        )
    }
    pub async fn delete(&self, user: &ContentIdentity, id: &str) -> Result<Value, ContentError> {
        validate_file_id(id)?;
        let prefix = format!("uploads/{}/{}/{id}_", user.tenant_id, user.user_id);
        if self.backend.delete_prefix(&prefix).await? == 0 {
            return Err(ContentError::new(404, "File not found"));
        }
        Ok(json!({"status":"deleted","file_id":id}))
    }
}
#[derive(Clone)]
pub struct CleanupConfig {
    pub storage_path: PathBuf,
    pub file_ttl_days: i64,
    pub user_quota_mb: u64,
    pub cleanup_interval_hours: u64,
    pub enabled: bool,
}
impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            storage_path: PathBuf::from("./uploads"),
            file_ttl_days: 7,
            user_quota_mb: 500,
            cleanup_interval_hours: 6,
            enabled: true,
        }
    }
}
#[derive(Clone)]
pub struct FileCleanup {
    pub config: CleanupConfig,
}
fn format_size(bytes: u64) -> String {
    let mut n = bytes as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if n < 1024.0 {
            return format!("{n:.1} {unit}");
        }
        n /= 1024.0;
    }
    format!("{n:.1} TB")
}
#[derive(Clone)]
struct Entry {
    path: PathBuf,
    size: u64,
    modified: DateTime<Utc>,
}
fn entries(dir: &FsPath) -> Result<Vec<Entry>, ContentError> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut rows = vec![];
    for row in std::fs::read_dir(dir)? {
        let path = row?.path();
        if path.is_file() {
            let stat = std::fs::metadata(&path)?;
            rows.push(Entry {
                path,
                size: stat.len(),
                modified: stat.modified()?.into(),
            });
        }
    }
    Ok(rows)
}
impl FileCleanup {
    fn cleanup_directory(&self, dir: &FsPath) -> Result<(u64, u64), ContentError> {
        let mut files = entries(dir)?;
        files.sort_by_key(|e| e.modified);
        let mut size: u64 = files.iter().map(|e| e.size).sum();
        let mut freed = 0;
        let mut count = 0;
        let threshold = Utc::now() - Duration::days(self.config.file_ttl_days);
        let mut remaining = vec![];
        for file in files {
            if file.modified < threshold && std::fs::remove_file(&file.path).is_ok() {
                freed += file.size;
                size -= file.size;
                count += 1;
            } else {
                remaining.push(file)
            }
        }
        for file in remaining {
            if size <= self.config.user_quota_mb * 1024 * 1024 {
                break;
            }
            if std::fs::remove_file(&file.path).is_ok() {
                freed += file.size;
                size -= file.size;
                count += 1;
            }
        }
        Ok((count, freed))
    }
    pub async fn run_cleanup(&self, target: Option<String>) -> Result<Value, ContentError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move||{let root=this.config.storage_path.join("uploads");if let Some(target)=target{validate_user_id(&target)?;let dir=root.join(&target);if !dir.exists(){return Ok(json!({"files_deleted":0,"bytes_freed":0,"message":"User directory not found"}))}let(n,bytes)=this.cleanup_directory(&dir)?;return Ok(json!({"files_deleted":n,"bytes_freed":bytes,"bytes_freed_formatted":format_size(bytes),"message":format!("Cleaned up {n} files for user {target}")}))}let start=Instant::now();let(mut n,mut bytes,mut users,mut errors)=(0,0,0,0);if root.exists(){for entry in std::fs::read_dir(&root)?{let path=entry?.path();if !path.is_dir(){continue}match this.cleanup_directory(&path){Ok((count,freed))=>{n+=count;bytes+=freed;if count>0{users+=1;}},Err(_)=>errors+=1}if std::fs::read_dir(&path)?.next().is_none(){let _=std::fs::remove_dir(&path);}}}Ok(json!({"files_deleted":n,"bytes_freed":bytes,"bytes_freed_formatted":format!("{:.2} MB",bytes as f64/1048576.0),"users_cleaned":users,"errors":errors,"duration_ms":start.elapsed().as_millis()}))}).await.map_err(|_|ContentError::storage())?
    }
    pub async fn stats(&self) -> Result<Value, ContentError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move||{let root=this.config.storage_path.join("uploads");if !root.exists(){return Ok(json!({"total_files":0,"total_size":0,"total_size_formatted":"0 B","users":[]}))}let mut users=vec![];let(mut total_files,mut total_size)=(0usize,0u64);for entry in std::fs::read_dir(root)?{let path=entry?.path();if !path.is_dir(){continue}let files=entries(&path)?;if files.is_empty(){continue}let size:u64=files.iter().map(|f|f.size).sum();total_files+=files.len();total_size+=size;users.push(json!({"user_id":path.file_name().unwrap_or_default().to_string_lossy(),"files":files.len(),"size":size,"size_formatted":format_size(size),"quota_used_percent":(size as f64/(this.config.user_quota_mb*1048576) as f64*1000.0).round()/10.0,"oldest_file":files.iter().map(|f|f.modified).min(),"newest_file":files.iter().map(|f|f.modified).max()}));}users.sort_by_key(|v|std::cmp::Reverse(v["size"].as_u64().unwrap_or(0)));Ok(json!({"total_files":total_files,"total_size":total_size,"total_size_formatted":format_size(total_size),"users":users,"config":{"ttl_days":this.config.file_ttl_days,"user_quota_mb":this.config.user_quota_mb,"cleanup_interval_hours":this.config.cleanup_interval_hours,"cleanup_enabled":this.config.enabled}}))}).await.map_err(|_|ContentError::storage())?
    }
    /// Host owns task lifetime and shutdown; starts after the source's 60s grace.
    pub async fn run_scheduler(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        if !self.config.enabled {
            return;
        }
        tokio::select! {_=tokio::time::sleep(std::time::Duration::from_secs(60))=>(),_=shutdown.changed()=>return};
        loop {
            let _ = self.run_cleanup(None).await;
            tokio::select! {_=tokio::time::sleep(std::time::Duration::from_secs(self.config.cleanup_interval_hours*3600))=>(),_=shutdown.changed()=>break}
        }
    }
}
#[derive(Clone)]
pub struct FilesApi {
    pub storage: FileStorage,
    pub local_path: PathBuf,
    pub max_bytes: usize,
    pub admins: BTreeSet<String>,
    pub cleanup: FileCleanup,
    pub access: Arc<dyn ContentAccess>,
}
fn platform_admin(user: &ContentIdentity) -> bool {
    user.roles.iter().any(|role| matches!(role.trim().to_lowercase().as_str(), "platform_admin" | "superadmin" | "super_admin"))
}
impl FilesApi {
    pub fn user_directory(&self, user: &str, tenant: &str) -> Result<PathBuf, ContentError> {
        validate_user_id(user)?;
        if !tenant.is_empty() {
            validate_user_id(tenant)?;
        }
        let storage = LocalStorage::new(self.local_path.join("uploads"), String::new());
        let path = storage.path(&if tenant.is_empty() {
            user.to_owned()
        } else {
            format!("{tenant}/{user}")
        })?;
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }
    fn admin(&self, user: &ContentIdentity) -> Result<(), ContentError> {
        if platform_admin(user) || self.admins.contains(&user.user_id)
            || user.roles.iter().any(|r| r == "admin" || r == "file_admin")
        {
            Ok(())
        } else {
            Err(ContentError::new(
                403,
                "Admin access required for this operation",
            ))
        }
    }
    fn entry_json(user: &ContentIdentity, e: &Entry) -> Value {
        let name = e.path.file_name().unwrap_or_default().to_string_lossy();
        let stem = e.path.file_stem().unwrap_or_default().to_string_lossy();
        let id = stem
            .split('_')
            .next()
            .unwrap_or("")
            .chars()
            .take(8)
            .collect::<String>();
        json!({"file_id":id,"file_path":format!("/uploads/{}/{}/{name}",user.tenant_id,user.user_id),"filename":name,"size_bytes":e.size,"mime_type":mime_type(&extension(&name)),"uploaded_at":e.modified})
    }
    pub async fn list(&self, user: &ContentIdentity) -> Result<Value, ContentError> {
        let mut rows = entries(&self.user_directory(&user.user_id, &user.tenant_id)?)?;
        rows.sort_by_key(|e| std::cmp::Reverse(e.modified));
        let files: Vec<_> = rows
            .iter()
            .take(50)
            .map(|e| Self::entry_json(user, e))
            .collect();
        Ok(json!({"total":files.len(),"files":files}))
    }
    pub async fn get(&self, user: &ContentIdentity, id: &str) -> Result<Value, ContentError> {
        validate_file_id(id)?;
        for entry in entries(&self.user_directory(&user.user_id, &user.tenant_id)?)? {
            if entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .starts_with(&format!("{id}_"))
            {
                return Ok(Self::entry_json(user, &entry));
            }
        }
        Err(ContentError::new(404, "File not found"))
    }
}
pub fn router(api: Arc<FilesApi>) -> Router {
    let max = api.max_bytes.saturating_mul(5).saturating_add(1024 * 1024);
    Router::new()
        .route("/files", get(list))
        .route("/files/upload", post(upload))
        .route("/files/upload/multiple", post(multiple))
        .route("/files/{id}", get(info).delete(remove))
        .route("/files/admin/stats", get(stats))
        .route("/files/admin/cleanup", post(cleanup))
        .route("/files/admin/user/{user}", delete(delete_user))
        .layer(axum::extract::DefaultBodyLimit::max(max))
        .with_state(api)
}
async fn list(
    State(api): State<Arc<FilesApi>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    Ok(Json(api.list(&u).await?))
}
async fn info(
    State(api): State<Arc<FilesApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    Ok(Json(api.get(&u, &id).await?))
}
async fn remove(
    State(api): State<Arc<FilesApi>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    Ok(Json(api.storage.delete(&u, &id).await?))
}
struct TempUpload {
    path: PathBuf,
    name: String,
}
impl Drop for TempUpload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
async fn receive(
    api: Arc<FilesApi>,
    headers: HeaderMap,
    mut multipart: Multipart,
    multiple: bool,
) -> Result<Json<Value>, ContentError> {
    use tokio::io::AsyncWriteExt;
    let u = api.access.resolve(&headers).await?;
    let mut pending = vec![];
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| ContentError::new(400, "Invalid multipart upload"))?
    {
        if field.name() != Some(if multiple { "files" } else { "file" }) {
            continue;
        }
        let name = field
            .file_name()
            .ok_or_else(|| ContentError::new(400, "Filename is required"))?
            .to_owned();
        validate_extension(&name)?;
        let temporary = TempUpload {
            path: std::env::temp_dir().join(format!("ai-platform-upload-{}", uuid::Uuid::new_v4())),
            name,
        };
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary.path)
            .await?;
        let mut bytes = 0usize;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ContentError::new(400, "Invalid multipart upload"))?
        {
            if chunk.len() > api.max_bytes.saturating_sub(bytes) {
                return Err(ContentError::new(
                    413,
                    format!(
                        "File exceeds maximum size of {:.1}MB",
                        api.max_bytes as f64 / 1048576.0
                    ),
                ));
            }
            bytes += chunk.len();
            output.write_all(&chunk).await?;
        }
        drop(output);
        pending.push(temporary);
        if pending.len() > if multiple { 5 } else { 1 } {
            return Err(ContentError::new(400, "Maximum 5 files per upload"));
        }
    }
    if pending.is_empty() {
        return Err(ContentError::new(422, "Field required"));
    }
    let mut out = vec![];
    for upload in pending {
        api.access.rate_limit(&u, "file_upload", None).await?;
        out.push(
            api.storage
                .upload_reader(
                    &u,
                    &upload.name,
                    tokio::fs::File::open(&upload.path).await?,
                    api.max_bytes,
                )
                .await?,
        );
    }
    Ok(Json(if multiple { json!(out) } else { out.remove(0) }))
}
async fn upload(
    State(api): State<Arc<FilesApi>>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<Value>, ContentError> {
    receive(api, headers, multipart, false).await
}
async fn multiple(
    State(api): State<Arc<FilesApi>>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<Value>, ContentError> {
    receive(api, headers, multipart, true).await
}
async fn stats(
    State(api): State<Arc<FilesApi>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    api.admin(&u)?;
    if !platform_admin(&u) {
        return Err(ContentError::new(403, "Platform administrator required for global file administration"));
    }
    Ok(Json(api.cleanup.stats().await?))
}
async fn cleanup(
    State(api): State<Arc<FilesApi>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    api.admin(&u)?;
    if !platform_admin(&u) {
        return Err(ContentError::new(403, "Platform administrator required for global file administration"));
    }
    Ok(Json(
        api.cleanup.run_cleanup(q.get("user_id").cloned()).await?,
    ))
}
async fn delete_user(
    State(api): State<Arc<FilesApi>>,
    Path(target): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ContentError> {
    let u = api.access.resolve(&headers).await?;
    api.admin(&u)?;
    let tenant = q.get("target_tenant_id").map(String::as_str)
        .filter(|value| !value.is_empty()).unwrap_or(&u.tenant_id);
    if !platform_admin(&u) && (u.tenant_id.is_empty() || tenant != u.tenant_id) {
        return Err(ContentError::new(403, "Cross-tenant file administration denied"));
    }
    let dir = api.user_directory(&target, tenant)?;
    let (mut n, mut bytes) = (0, 0);
    for file in entries(&dir)? {
        if std::fs::remove_file(file.path).is_ok() {
            n += 1;
            bytes += file.size;
        }
    }
    let _ = std::fs::remove_dir(dir);
    Ok(Json(
        json!({"status":"deleted","user_id":target,"files_deleted":n,"bytes_freed":bytes,"bytes_freed_formatted":format!("{:.2} MB",bytes as f64/1048576.0)}),
    ))
}

#[cfg(test)]
mod manual_file_scope_regressions {
    use super::*;
    use crate::migration_content::{ContentFuture, artifacts::Artifact};

    struct FixedIdentity(ContentIdentity);
    impl ContentAccess for FixedIdentity {
        fn resolve<'a>(&'a self, _: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity> {
            Box::pin(async { Ok(self.0.clone()) })
        }
        fn require_capability<'a>(&'a self, _: &'a ContentIdentity, _: &'a str) -> ContentFuture<'a, ()> {
            Box::pin(async { panic!("unexpected capability backend") })
        }
        fn rate_limit<'a>(&'a self, _: &'a ContentIdentity, _: &'a str, _: Option<&'a str>) -> ContentFuture<'a, ()> {
            Box::pin(async { panic!("unexpected rate backend") })
        }
        fn authorize_session<'a>(&'a self, _: &'a ContentIdentity, _: &'a str) -> ContentFuture<'a, Value> {
            Box::pin(async { panic!("unexpected session backend") })
        }
        fn authorize_artifact_source<'a>(&'a self, _: &'a ContentIdentity, _: &'a Artifact) -> ContentFuture<'a, ()> {
            Box::pin(async { panic!("unexpected artifact backend") })
        }
        fn require_vision_model<'a>(&'a self, _: &'a ContentIdentity, _: &'a str, _: Option<&'a str>) -> ContentFuture<'a, ()> {
            Box::pin(async { panic!("unexpected vision backend") })
        }
    }
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tenant-file-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    fn setup(root: &FsPath, role: &str, tenant: &str, configured: bool) -> Arc<FilesApi> {
        let identity = ContentIdentity {
            user_id: "actor".into(), tenant_id: tenant.into(), roles: vec![role.into()],
            is_authenticated: true, ..Default::default()
        };
        Arc::new(FilesApi {
            storage: FileStorage { backend: Arc::new(LocalStorage::new(root.into(), String::new())) },
            local_path: root.into(), max_bytes: 1024,
            admins: if configured { BTreeSet::from(["actor".into()]) } else { BTreeSet::new() },
            cleanup: FileCleanup { config: CleanupConfig { storage_path: root.into(), ..Default::default() } },
            access: Arc::new(FixedIdentity(identity)),
        })
    }
    fn query(target: Option<&str>) -> Query<HashMap<String, String>> {
        Query(target.map(|value| HashMap::from([("target_tenant_id".into(), value.into())])).unwrap_or_default())
    }
    #[tokio::test]
    async fn foreign_and_missing_tenant_delete_denied_before_directory_access() {
        for (role, tenant, configured) in [("admin", "a", false), ("file_admin", "a", false), ("user", "a", true), ("admin", "", false)] {
            let scratch = Scratch::new();
            // If user_directory runs it would create uploads/b/victim.
            let result = delete_user(State(setup(&scratch.0, role, tenant, configured)), Path("victim".into()), query(Some("b")), HeaderMap::new()).await;
            assert_eq!(result.unwrap_err().status, 403);
            assert!(!scratch.0.join("uploads").exists());
        }
    }
    #[tokio::test]
    async fn own_tenant_empty_default_and_explicit_platform_delete_work() {
        for (role, configured, requested, selected) in [
            ("admin", false, None, "a"), ("admin", false, Some(""), "a"),
            ("file_admin", false, Some("a"), "a"), ("user", true, Some("a"), "a"),
            ("platform_admin", false, Some("b"), "b"), (" SUPER_ADMIN ", false, Some("b"), "b"),
        ] {
            let scratch = Scratch::new();
            let selected_dir = scratch.0.join("uploads").join(selected).join("victim");
            let unrelated = scratch.0.join("uploads/c/victim");
            std::fs::create_dir_all(&selected_dir).unwrap();
            std::fs::create_dir_all(&unrelated).unwrap();
            std::fs::write(selected_dir.join("12345678_test.txt"), b"test").unwrap();
            std::fs::write(unrelated.join("12345678_keep.txt"), b"keep").unwrap();
            let result = delete_user(State(setup(&scratch.0, role, "a", configured)), Path("victim".into()), query(requested), HeaderMap::new()).await.unwrap();
            assert_eq!(result.0["files_deleted"], json!(1));
            assert_eq!(result.0["bytes_freed"], json!(4));
            assert!(!selected_dir.exists());
            assert_eq!(std::fs::read(unrelated.join("12345678_keep.txt")).unwrap(), b"keep");
        }
    }
    #[tokio::test]
    async fn global_stats_and_cleanup_require_explicit_platform_identity() {
        for (role, configured, allowed) in [("admin", false, false), ("file_admin", false, false), ("user", true, false), ("user", false, false), ("platform_admin", false, true), ("superadmin", false, true)] {
            let scratch = Scratch::new();
            let api = setup(&scratch.0, role, "a", configured);
            let result = stats(State(api.clone()), HeaderMap::new()).await;
            if allowed { assert!(result.is_ok()); } else { assert_eq!(result.unwrap_err().status, 403); }
            for target in [None, Some("victim")] {
                let q = Query(target.map(|value: &str| HashMap::from([("user_id".into(), value.into())])).unwrap_or_default());
                let result = cleanup(State(api.clone()), q, HeaderMap::new()).await;
                if allowed { assert!(result.is_ok()); } else { assert_eq!(result.unwrap_err().status, 403); }
            }
            assert!(!scratch.0.join("uploads").exists());
        }
    }
}
