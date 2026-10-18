use ai_platform_client::device_auth::{DeviceChallenge, DeviceLogin};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use tokio::sync::Mutex;
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub gateway: Option<String>,
    pub cohort: String,
}
pub struct PendingLogin {
    pub challenge: DeviceChallenge,
    pub verifier: String,
}
pub struct DesktopState {
    pub directory: PathBuf,
    pub preferences: Mutex<Preferences>,
    pub pending: Mutex<Option<PendingLogin>>,
    // Serializes configuration, login/refresh/logout, pairing and update install.
    // Config cannot switch origins while an operation with a credential is live.
    pub operations: Mutex<()>,
    pub node_config: Option<PathBuf>,
    pub http: reqwest::Client,
}
impl DesktopState {
    pub fn new(directory: PathBuf) -> Result<Self, &'static str> {
        std::fs::create_dir_all(&directory)
            .map_err(|_| "private settings directory unavailable")?;
        let meta = std::fs::symlink_metadata(&directory)
            .map_err(|_| "private settings directory unavailable")?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("private settings directory invalid");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "settings permissions unavailable")?;
        }
        let file = directory.join("preferences.json");
        let preferences = if file.exists() {
            read_json::<Preferences>(&file)?
        } else {
            let preferences = Preferences {
                gateway: None,
                cohort: uuid::Uuid::new_v4().to_string(),
            };
            write_new(
                &file,
                &serde_json::to_vec(&preferences).map_err(|_| "settings invalid")?,
            )?;
            preferences
        };
        uuid::Uuid::parse_str(&preferences.cohort).map_err(|_| "settings cohort invalid")?;
        if let Some(origin) = &preferences.gateway {
            crate::policy::gateway(origin)?;
        }
        let node_config = std::env::var_os("AGENTFORGE_LOCAL_NODE_CONFIG").map(PathBuf::from);
        if node_config.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err("Local Node configuration path must be absolute");
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| "transport unavailable")?;
        Ok(Self {
            directory,
            preferences: Mutex::new(preferences),
            pending: Mutex::new(None),
            operations: Mutex::new(()),
            node_config,
            http,
        })
    }
    pub async fn login(&self) -> Result<DeviceLogin, &'static str> {
        let preferences = self.preferences.lock().await;
        let origin = preferences
            .gateway
            .as_deref()
            .ok_or("configure Gateway first")?;
        // This is the existing registered public-client ID; no new unregistered
        // desktop grant, secret or scope is invented by the shell.
        DeviceLogin::new(origin, "agentforge-cli").map_err(|_| "native vault unavailable")
    }
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, &'static str> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "settings unavailable")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 65536 {
        return Err("settings file invalid");
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|_| "settings unavailable")?)
        .map_err(|_| "settings invalid")
}
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "settings write unavailable")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "settings write failed")
}
pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), &'static str> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("settings path invalid")?)
            .map_err(|_| "settings write unavailable")?;
    temporary
        .write_all(&serde_json::to_vec(value).map_err(|_| "settings invalid")?)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| "settings write failed")?;
    temporary
        .persist(path)
        .map_err(|_| "settings replace failed")?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or("settings path invalid")?)
        .and_then(|file| file.sync_all())
        .map_err(|_| "settings sync failed")?;
    Ok(())
}
