use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instance {
    Coord,
    Cache,
}
impl Instance {
    pub fn name(self) -> &'static str {
        match self {
            Self::Coord => "coord",
            Self::Cache => "cache",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceIdentity {
    Gateway,
    KnowledgeApi,
    KnowledgeWorker,
}
impl ServiceIdentity {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::KnowledgeApi => "knowledge-api",
            Self::KnowledgeWorker => "knowledge-worker",
        }
    }
    pub fn username(self, instance: Instance) -> String {
        format!("{}-{}", self.name(), instance.name())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValkeyError {
    Configuration,
    CredentialFile,
    Authentication,
    Transport,
    Pressure,
    ServerPolicy,
    Timeout,
    CacheScope,
}
impl fmt::Display for ValkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "valkey_{self:?}")
    }
}
impl std::error::Error for ValkeyError {}
#[derive(Clone)]
pub struct EndpointConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password_file: PathBuf,
    pub(crate) ca_file: PathBuf,
    pub(crate) instance: Instance,
    pub(crate) service: ServiceIdentity,
}
impl fmt::Debug for EndpointConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndpointConfig")
            .field("instance", &self.instance)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}
impl EndpointConfig {
    pub fn new(
        endpoint: &str,
        username: &str,
        password_file: PathBuf,
        ca_file: PathBuf,
        service: ServiceIdentity,
        instance: Instance,
    ) -> Result<Self, ValkeyError> {
        if endpoint.len() > 1024
            || endpoint.trim() != endpoint
            || endpoint.bytes().any(|b| b.is_ascii_control())
        {
            return Err(ValkeyError::Configuration);
        }
        let u = reqwest::Url::parse(endpoint).map_err(|_| ValkeyError::Configuration)?;
        if u.scheme() != "rediss"
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || u.path() != "/0"
            || u.host_str().is_none()
            || u.port().is_none()
            || username != service.username(instance)
            || !password_file.is_absolute()
            || !ca_file.is_absolute()
        {
            return Err(ValkeyError::Configuration);
        }
        Ok(Self {
            host: u
                .host_str()
                .unwrap()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase(),
            port: u.port().unwrap(),
            username: username.into(),
            password_file,
            ca_file,
            instance,
            service,
        })
    }
    pub fn instance(&self) -> Instance {
        self.instance
    }
    pub fn service(&self) -> ServiceIdentity {
        self.service
    }
    pub(crate) fn address(&self) -> (&str, u16) {
        (&self.host, self.port)
    }
}
#[derive(Clone, Debug)]
pub struct SplitConfig {
    pub(crate) coord: EndpointConfig,
    pub(crate) cache: EndpointConfig,
}
impl SplitConfig {
    pub fn coord(&self) -> &EndpointConfig {
        &self.coord
    }
    pub fn cache(&self) -> &EndpointConfig {
        &self.cache
    }
    pub fn new(coord: EndpointConfig, cache: EndpointConfig) -> Result<Self, ValkeyError> {
        if coord.instance != Instance::Coord
            || cache.instance != Instance::Cache
            || coord.service != cache.service
            || coord.address() == cache.address()
            || coord.password_file == cache.password_file
        {
            return Err(ValkeyError::Configuration);
        }
        Ok(Self { coord, cache })
    }
    /// Legacy mode is explicit absence; any partial/new configuration is fatal.
    /// Call with a captured process environment, never on a request path.
    pub fn from_values(
        values: &BTreeMap<String, String>,
        service: ServiceIdentity,
    ) -> Result<Option<Self>, ValkeyError> {
        let allowed = [
            "AI_PLATFORM_VALKEY_MODE",
            "AI_PLATFORM_VALKEY_COORD_ENDPOINT",
            "AI_PLATFORM_VALKEY_COORD_USERNAME",
            "AI_PLATFORM_VALKEY_COORD_PASSWORD_FILE",
            "AI_PLATFORM_VALKEY_COORD_CA_FILE",
            "AI_PLATFORM_VALKEY_CACHE_ENDPOINT",
            "AI_PLATFORM_VALKEY_CACHE_USERNAME",
            "AI_PLATFORM_VALKEY_CACHE_PASSWORD_FILE",
            "AI_PLATFORM_VALKEY_CACHE_CA_FILE",
        ];
        if values
            .keys()
            .any(|key| key.starts_with("AI_PLATFORM_VALKEY_") && !allowed.contains(&key.as_str()))
        {
            return Err(ValkeyError::Configuration);
        }
        let has_new = values.keys().any(|k| k.starts_with("AI_PLATFORM_VALKEY_"));
        let mode = values.get("AI_PLATFORM_VALKEY_MODE").map(String::as_str);
        if !has_new {
            return Ok(None);
        }
        if mode != Some("split") {
            return Err(ValkeyError::Configuration);
        }
        let endpoint = |kind: Instance| -> Result<EndpointConfig, ValkeyError> {
            let prefix = format!("AI_PLATFORM_VALKEY_{}", kind.name().to_ascii_uppercase());
            let get = |suffix: &str| {
                values
                    .get(&format!("{prefix}_{suffix}"))
                    .filter(|v| !v.trim().is_empty())
                    .map(String::as_str)
                    .ok_or(ValkeyError::Configuration)
            };
            EndpointConfig::new(
                get("ENDPOINT")?,
                get("USERNAME")?,
                get("PASSWORD_FILE")?.into(),
                get("CA_FILE")?.into(),
                service,
                kind,
            )
        };
        Self::new(endpoint(Instance::Coord)?, endpoint(Instance::Cache)?).map(Some)
    }
}
pub(crate) fn read_public(path: &Path) -> Result<Vec<u8>, ValkeyError> {
    read_bounded(path, false)
}
pub(crate) fn read_password(path: &Path) -> Result<String, ValkeyError> {
    let b = read_bounded(path, true)?;
    let value = String::from_utf8(b).map_err(|_| ValkeyError::CredentialFile)?;
    // Generated credentials are 64 lowercase hex; no trimming or newline ambiguity.
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ValkeyError::CredentialFile);
    }
    Ok(value)
}
fn read_bounded(path: &Path, private: bool) -> Result<Vec<u8>, ValkeyError> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| ValkeyError::CredentialFile)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() == 0 || meta.len() > 131072 {
        return Err(ValkeyError::CredentialFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & if private { 0o077 } else { 0o022 } != 0 {
            return Err(ValkeyError::CredentialFile);
        }
    }
    std::fs::read(path).map_err(|_| ValkeyError::CredentialFile)
}
