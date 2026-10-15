//! Dedicated owner-only credential directory with atomic writes and exclusive
//! process ownership. Host must use one shared Arc per directory. No env discovery.
use super::*;
use oauth::Tokens;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

type SharedSession = Arc<Mutex<Option<Tokens>>>;
pub struct ProtectedStore {
    directory: PathBuf,
    _lock: File,
    host_id: String,
    sessions: std::sync::Mutex<std::collections::HashMap<String, SharedSession>>,
}
#[derive(Serialize, Deserialize)]
struct Record {
    identity: AccountIdentity,
    tokens: Option<Tokens>,
}
impl ProtectedStore {
    /// Explicit setup boundary, creates private storage and a stable host identifier.
    /// Unix is supported; unsupported platforms fail closed (use native keychain there).
    #[cfg(unix)]
    pub fn open(directory: impl AsRef<Path>) -> Result<Arc<Self>> {
        use std::os::unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            io::AsRawFd,
        };
        let directory = directory.as_ref().to_path_buf();
        if !directory.exists() {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .map_err(|_| PlanError::local("credential_store_create"))?;
        }
        let meta = std::fs::symlink_metadata(&directory)
            .map_err(|_| PlanError::local("credential_store_metadata"))?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.mode() & 0o077 != 0
            || meta.uid() != unsafe { libc::geteuid() }
        {
            return Err(PlanError::local("credential_store_permissions"));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join(".lock"))
            .map_err(|_| PlanError::local("credential_store_lock"))?;
        check_file(&lock)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(PlanError::local("credential_store_in_use"));
        }
        let mut store = Self {
            directory,
            _lock: lock,
            host_id: String::new(),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        store.host_id = if store.directory.join("host.json").exists() {
            let value: Value = serde_json::from_slice(&store.read("host.json")?)
                .map_err(|_| PlanError::local("invalid_host_record"))?;
            value
                .get("host_id")
                .and_then(Value::as_str)
                .filter(|v| oauth::valid_host_id(v))
                .ok_or_else(|| PlanError::local("invalid_host_record"))?
                .to_owned()
        } else {
            let id = format!("urn:uuid:{}", Uuid::new_v4());
            store.write(
                "host.json",
                &serde_json::to_vec(&json!({"host_id":id}))
                    .map_err(|_| PlanError::local("credential_encode"))?,
            )?;
            id
        };
        Ok(Arc::new(store))
    }
    #[cfg(not(unix))]
    pub fn open(_directory: impl AsRef<Path>) -> Result<Arc<Self>> {
        Err(PlanError::local("native_credential_store_required"))
    }
    pub(crate) fn session(
        &self,
        identity: &AccountIdentity,
        initial: SharedSession,
    ) -> Result<SharedSession> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| PlanError::local("credential_store_lock"))?;
        Ok(sessions
            .entry(filename(identity))
            .or_insert(initial)
            .clone())
    }
    pub fn host_id(&self) -> &str {
        &self.host_id
    }
    pub fn load(self: &Arc<Self>, identity: &AccountIdentity) -> Result<Account> {
        let record: Record = serde_json::from_slice(&self.read(&filename(identity))?)
            .map_err(|_| PlanError::local("invalid_credential_record"))?;
        if record.identity != *identity
            || identity.client_id.is_empty()
            || identity.client_id == "dynamic_agent_client"
            || identity.subject.is_empty()
        {
            return Err(PlanError::local("credential_identity_mismatch"));
        }
        Ok(Account {
            identity: record.identity,
            tokens: self.session(identity, Arc::new(Mutex::new(record.tokens)))?,
            store: Some(self.clone()),
        })
    }
    /// Account picker metadata only, never includes tokens. No email-based account merging.
    pub fn accounts(&self) -> Result<Vec<AccountIdentity>> {
        let mut result = Vec::new();
        for entry in std::fs::read_dir(&self.directory)
            .map_err(|_| PlanError::local("credential_store_read"))?
        {
            let entry = entry.map_err(|_| PlanError::local("credential_store_read"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("account-") && name.ends_with(".json") {
                let record: Record = serde_json::from_slice(&self.read(&name)?)
                    .map_err(|_| PlanError::local("invalid_credential_record"))?;
                if filename(&record.identity) != name {
                    return Err(PlanError::local("credential_identity_mismatch"));
                }
                result.push(record.identity);
            }
        }
        result.sort_by(|a, b| {
            a.client_id
                .cmp(&b.client_id)
                .then(a.subject.cmp(&b.subject))
        });
        Ok(result)
    }
    pub(crate) fn save_record(
        &self,
        identity: &AccountIdentity,
        tokens: &Option<Tokens>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct BorrowedRecord<'a> {
            identity: &'a AccountIdentity,
            tokens: &'a Option<Tokens>,
        }
        let bytes = serde_json::to_vec(&BorrowedRecord { identity, tokens })
            .map_err(|_| PlanError::local("credential_encode"))?;
        self.write(&filename(identity), &bytes)
    }
    #[cfg(unix)]
    fn read(&self, name: &str) -> Result<Vec<u8>> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.directory.join(name))
            .map_err(|_| PlanError::local("credential_store_read"))?;
        check_file(&file)?;
        let mut bytes = Vec::new();
        file.take((MAX_JSON + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| PlanError::local("credential_store_read"))?;
        if bytes.len() > MAX_JSON {
            return Err(PlanError::local("credential_record_too_large"));
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    fn read(&self, _name: &str) -> Result<Vec<u8>> {
        Err(PlanError::local("native_credential_store_required"))
    }
    #[cfg(unix)]
    fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = self.directory.join(format!(".pending-{}", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temp)
                .map_err(|_| PlanError::local("credential_store_write"))?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| PlanError::local("credential_store_write"))?;
            std::fs::rename(&temp, self.directory.join(name))
                .map_err(|_| PlanError::local("credential_store_write"))?;
            File::open(&self.directory)
                .and_then(|d| d.sync_all())
                .map_err(|_| PlanError::local("credential_store_sync"))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
    #[cfg(not(unix))]
    fn write(&self, _name: &str, _bytes: &[u8]) -> Result<()> {
        Err(PlanError::local("native_credential_store_required"))
    }
}
fn filename(identity: &AccountIdentity) -> String {
    let bytes = serde_json::to_vec(&(identity.client_id.as_str(), identity.subject.as_str()))
        .expect("string pair");
    format!("account-{:x}.json", Sha256::digest(bytes))
}
#[cfg(unix)]
fn check_file(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = file
        .metadata()
        .map_err(|_| PlanError::local("credential_store_metadata"))?;
    if !m.is_file()
        || m.mode() & 0o077 != 0
        || m.uid() != unsafe { libc::geteuid() }
        || m.nlink() != 1
    {
        return Err(PlanError::local("credential_store_permissions"));
    }
    Ok(())
}
