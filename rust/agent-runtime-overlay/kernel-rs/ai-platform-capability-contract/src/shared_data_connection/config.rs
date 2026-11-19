use sqlx::{
    ConnectOptions,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseRole {
    Gateway,
    Runtime,
    CapabilityWorker,
    KnowledgeApi,
    KnowledgeWorker,
}
impl DatabaseRole {
    pub const ALL: [Self; 5] = [
        Self::Gateway,
        Self::Runtime,
        Self::CapabilityWorker,
        Self::KnowledgeApi,
        Self::KnowledgeWorker,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Gateway => "ai_gateway_gateway",
            Self::Runtime => "ai_gateway_runtime",
            Self::CapabilityWorker => "ai_gateway_capability_worker",
            Self::KnowledgeApi => "ai_gateway_knowledge_api",
            Self::KnowledgeWorker => "ai_gateway_knowledge_worker",
        }
    }
    pub fn alias(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Runtime => "runtime",
            Self::CapabilityWorker => "capability_worker",
            Self::KnowledgeApi => "knowledge_api",
            Self::KnowledgeWorker => "knowledge_worker",
        }
    }
    pub fn search_path(self) -> &'static str {
        match self {
            Self::Gateway => "pg_catalog,gateway,assistant,public",
            Self::Runtime => "pg_catalog,assistant,public",
            Self::CapabilityWorker => "pg_catalog,assistant,public",
            Self::KnowledgeApi | Self::KnowledgeWorker => "pg_catalog,knowledge,public",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Direct,
    Session,
    Transaction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPurpose {
    BackendPinned,
    TenantTransactions,
    CancellationControl,
}
#[derive(Clone, Debug)]
pub struct TlsFiles {
    pub ca: PathBuf,
    pub certificate: PathBuf,
    pub key: PathBuf,
}
#[derive(Clone)]
pub struct ConnectionConfig {
    pub role: DatabaseRole,
    pub mode: ConnectionMode,
    pub purpose: ConnectionPurpose,
    pub dsn_file: PathBuf,
    pub password_file: Option<PathBuf>,
    pub tls: TlsFiles,
    pub expected_host: String,
    pub expected_database: String,
    pub physical_database: String,
    pub max_connections: u32,
    pub replicas: u32,
    pub role_connection_limit: u32,
    pub pooled_backend_limit: u32,
    pub statement_timeout_ms: u32,
}
impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("role", &self.role)
            .field("mode", &self.mode)
            .field("purpose", &self.purpose)
            .field("max_connections", &self.max_connections)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataError(pub &'static str);
impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "data_connection_{}", self.0)
    }
}
impl std::error::Error for DataError {}
pub(crate) fn ensure(ok: bool, why: &'static str) -> Result<(), DataError> {
    if ok { Ok(()) } else { Err(DataError(why)) }
}
/// Private files may be owner-readable or an explicitly mounted service group-readable
/// secret (0400/0440/0600/0640). Public CA/certificates need not be private.
/// Final symlinks are rejected; mount projected Kubernetes secrets using subPath or
/// resolve a trusted secret volume into an owned private file before startup.
pub fn read_material(path: &Path, private: bool) -> Result<Vec<u8>, DataError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| DataError("material_unavailable"))?;
    ensure(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() > 0
            && metadata.len() <= 65536,
        "material_invalid",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if private {
            ensure(
                metadata.permissions().mode() & 0o037 == 0,
                "material_permissions",
            )?;
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| DataError("material_unavailable"))?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| DataError("material_unavailable"))?;
    ensure(
        !bytes.is_empty() && bytes.len() <= 65536,
        "material_invalid",
    )?;
    Ok(bytes)
}
fn secret_text(path: &Path) -> Result<String, DataError> {
    let bytes = read_material(path, true)?;
    let raw = String::from_utf8(bytes).map_err(|_| DataError("secret_encoding"))?;
    let value = raw
        .strip_suffix("\n")
        .unwrap_or(&raw)
        .strip_suffix('\r')
        .unwrap_or(raw.strip_suffix("\n").unwrap_or(&raw));
    ensure(
        !value.is_empty() && !value.chars().any(char::is_control),
        "secret_shape",
    )?;
    Ok(value.into())
}
impl ConnectionConfig {
    /// Prefix example: DATA_GATEWAY. The caller supplies one explicit environment
    /// snapshot; values are paths, role-independent settings and public identities.
    pub fn from_environment(
        prefix: &str,
        role: DatabaseRole,
        purpose: ConnectionPurpose,
        e: &BTreeMap<String, String>,
    ) -> Result<Self, DataError> {
        let get = |suffix: &str| {
            e.get(&format!("{prefix}_{suffix}"))
                .filter(|s| !s.is_empty())
                .cloned()
                .ok_or(DataError("configuration_missing"))
        };
        for suffix in ["DSN", "PASSWORD"] {
            ensure(
                !e.get(&format!("{prefix}_{suffix}"))
                    .is_some_and(|s| !s.is_empty()),
                "environment_secret_refused",
            )?;
        }
        let number = |suffix: &str, default: u32| -> Result<u32, DataError> {
            e.get(&format!("{prefix}_{suffix}"))
                .map(|s| s.parse().map_err(|_| DataError("configuration_number")))
                .unwrap_or(Ok(default))
        };
        let mode = match get("MODE")?.as_str() {
            "direct" => ConnectionMode::Direct,
            "session" => ConnectionMode::Session,
            "transaction" => ConnectionMode::Transaction,
            _ => return Err(DataError("configuration_mode")),
        };
        let config = Self {
            role,
            mode,
            purpose,
            dsn_file: get("DSN_FILE")?.into(),
            password_file: e
                .get(&format!("{prefix}_PASSWORD_FILE"))
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            tls: TlsFiles {
                ca: get("TLS_CA_FILE")?.into(),
                certificate: get("TLS_CERT_FILE")?.into(),
                key: get("TLS_KEY_FILE")?.into(),
            },
            expected_host: get("EXPECTED_HOST")?,
            expected_database: get("DATABASE_ALIAS")?,
            physical_database: get("PHYSICAL_DATABASE")?,
            max_connections: number(
                "POOL_MAX",
                if purpose == ConnectionPurpose::CancellationControl {
                    2
                } else {
                    4
                },
            )?,
            replicas: number("REPLICAS", 1)?,
            role_connection_limit: number("ROLE_CONNECTION_LIMIT", 64)?,
            pooled_backend_limit: number("POOLED_BACKEND_LIMIT", 8)?,
            statement_timeout_ms: number("STATEMENT_TIMEOUT_MS", 30000)?,
        };
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), DataError> {
        ensure(
            (1..=64).contains(&self.max_connections)
                && (1..=64).contains(&self.replicas)
                && (1..=128).contains(&self.role_connection_limit)
                && (1..=64).contains(&self.pooled_backend_limit),
            "pool_bounds",
        )?;
        ensure(
            (100..=300000).contains(&self.statement_timeout_ms),
            "statement_timeout_bounds",
        )?;
        ensure(
            !self.expected_host.is_empty()
                && !self.expected_database.is_empty()
                && !self.physical_database.is_empty(),
            "database_identity",
        )?;
        ensure(
            !(self.mode == ConnectionMode::Transaction
                && self.purpose != ConnectionPurpose::TenantTransactions),
            "transaction_pool_requires_scoped_operation",
        )?;
        ensure(
            !(self.purpose == ConnectionPurpose::CancellationControl
                && self.mode != ConnectionMode::Direct),
            "cancellation_control_requires_direct",
        )?;
        ensure(
            self.purpose != ConnectionPurpose::CancellationControl || self.max_connections <= 2,
            "cancellation_control_pool_bounds",
        )?;
        let backends = if self.mode == ConnectionMode::Transaction {
            self.pooled_backend_limit
        } else {
            self.max_connections
                .checked_mul(self.replicas)
                .ok_or(DataError("pool_bounds"))?
        };
        ensure(
            backends + self.replicas * 2 <= self.role_connection_limit,
            "role_connection_budget",
        )?;
        if self.mode != ConnectionMode::Direct {
            let suffix = if self.mode == ConnectionMode::Transaction {
                "tx"
            } else {
                "session"
            };
            ensure(
                self.expected_database == format!("{}_{suffix}", self.role.alias()),
                "role_pool_alias",
            )?;
        }
        Ok(())
    }
    pub fn connect_options(&self) -> Result<PgConnectOptions, DataError> {
        self.validate()?;
        let dsn = secret_text(&self.dsn_file)?;
        let url = reqwest::Url::parse(&dsn).map_err(|_| DataError("dsn_invalid"))?;
        ensure(
            matches!(url.scheme(), "postgres" | "postgresql")
                && url.host_str() == Some(self.expected_host.as_str())
                && url.username() == self.role.name()
                && url.path() == format!("/{}", self.expected_database)
                && url.port().is_some_and(|port| port > 0)
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "dsn_identity_or_embedded_password",
        )?;
        // Credentials are independent file secrets, never inherited PGPASSWORD.
        let password = self
            .password_file
            .as_ref()
            .map(|p| secret_text(p))
            .transpose()?
            .unwrap_or_default();
        let options = PgConnectOptions::from_str(&dsn).map_err(|_| DataError("dsn_invalid"))?;
        ensure(
            options.get_username() == self.role.name()
                && options.get_database() == Some(self.expected_database.as_str())
                && options.get_options().is_none(),
            "dsn_role_or_startup_options",
        )?;
        let ca = read_material(&self.tls.ca, false)?;
        let cert = read_material(&self.tls.certificate, false)?;
        let key = read_material(&self.tls.key, true)?;
        ensure(
            !ca.windows(11).any(|v| v == b"PRIVATE KEY")
                && !cert.windows(11).any(|v| v == b"PRIVATE KEY"),
            "public_certificate_contains_private_key",
        )?;
        ensure(
            ca.windows(27).any(|v| v == b"-----BEGIN CERTIFICATE-----")
                && cert
                    .windows(27)
                    .any(|v| v == b"-----BEGIN CERTIFICATE-----")
                && key.windows(11).any(|v| v == b"PRIVATE KEY"),
            "tls_pem_shape",
        )?;
        Ok(options
            .password(&password)
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert_from_pem(ca)
            .ssl_client_cert_from_pem(cert)
            .ssl_client_key_from_pem(key)
            .statement_cache_capacity(if self.mode == ConnectionMode::Transaction {
                128
            } else {
                100
            })
            .application_name(self.role.alias())
            .disable_statement_logging())
    }
    pub async fn connect(&self) -> Result<sqlx::PgPool, DataError> {
        let pool = self
            .pool_options()?
            .connect_with(self.connect_options()?)
            .await
            .map_err(|_| DataError("connect_failed"))?;
        self.verify_pool_identity(&pool).await?;
        Ok(pool)
    }
    /// Reused by platform adapters that add their existing startup/session
    /// options while preserving this role, physical-database and budget probe.
    pub async fn verify_pool_identity(&self,pool:&sqlx::PgPool)->Result<(),DataError>{
        let identity: (String,String,bool,i32) = sqlx::query_as("SELECT current_user::text,current_database()::text,rolsuper OR rolbypassrls,rolconnlimit FROM pg_catalog.pg_roles WHERE rolname=current_user").fetch_one(pool).await.map_err(|_|DataError("identity_probe_failed"))?;
        if identity.0 != self.role.name()
            || identity.1 != self.physical_database
            || identity.2
            || identity.3 != self.role_connection_limit as i32
        {
            pool.close().await;
            return Err(DataError("connected_role_database_or_limit_mismatch"));
        }
        Ok(())
    }
    pub fn pool_options(&self) -> Result<PgPoolOptions, DataError> {
        self.validate()?;
        Ok(PgPoolOptions::new()
            .min_connections(0)
            .max_connections(self.max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(60))
            .max_lifetime(Duration::from_secs(1800)))
    }
}
