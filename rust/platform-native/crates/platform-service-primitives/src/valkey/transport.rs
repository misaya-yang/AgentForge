use super::{
    CacheKey, EndpointConfig, ServiceIdentity, SplitConfig, ValkeyError,
    config::{read_password, read_public},
    policy::verify_server,
};
use redis::{
    Client, ConnectionAddr, FromRedisValue, IntoConnectionInfo, RedisConnectionInfo,
    TlsCertificates,
    aio::{ConnectionManager, ConnectionManagerConfig},
};
use std::time::Duration;
#[derive(Clone)]
pub struct CoordConnection {
    connection: ConnectionManager,
    endpoint: EndpointConfig,
}
#[derive(Clone)]
pub struct CacheConnection {
    connection: ConnectionManager,
    service: ServiceIdentity,
}
#[derive(Clone)]
pub enum CacheAvailability {
    Ready(CacheConnection),
    Unavailable(ValkeyError),
}
pub struct SplitConnections {
    pub coord: CoordConnection,
    pub cache: CacheAvailability,
}
fn error(e: redis::RedisError) -> ValkeyError {
    super::tls_errors::classify(&e)
}
pub(super) async fn connect(config: &EndpointConfig) -> Result<ConnectionManager, ValkeyError> {
    connect_with_response_timeout(config, Duration::from_millis(500)).await
}
async fn connect_with_response_timeout(
    config: &EndpointConfig,
    response_timeout: Duration,
) -> Result<ConnectionManager, ValkeyError> {
    let password = read_password(&config.password_file)?;
    let ca = read_public(&config.ca_file)?;
    let info = (config.host.as_str(), config.port)
        .into_connection_info()
        .map_err(|_| ValkeyError::Configuration)?
        .set_addr(ConnectionAddr::TcpTls {
            host: config.host.clone(),
            port: config.port,
            insecure: false,
            tls_params: None,
        })
        .set_redis_settings(
            RedisConnectionInfo::default()
                .set_db(0)
                .set_username(&config.username)
                .set_password(password),
        );
    let client = Client::build_with_tls(
        info,
        TlsCertificates {
            client_tls: None,
            root_cert: Some(ca),
        },
    )
    .map_err(|_| ValkeyError::Configuration)?;
    let options = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(Duration::from_secs(2)))
        .set_response_timeout(Some(response_timeout))
        .set_number_of_retries(1)
        .set_min_delay(Duration::from_millis(50))
        .set_max_delay(Duration::from_millis(100));
    let mut connection = tokio::time::timeout(
        Duration::from_secs(5),
        ConnectionManager::new_with_config(client, options),
    )
    .await
    .map_err(|_| ValkeyError::Timeout)?
    .map_err(error)?;
    let server: String = redis::cmd("INFO")
        .arg("server")
        .query_async(&mut connection)
        .await
        .map_err(error)?;
    let memory: String = redis::cmd("INFO")
        .arg("memory")
        .query_async(&mut connection)
        .await
        .map_err(error)?;
    let persistence: String = redis::cmd("INFO")
        .arg("persistence")
        .query_async(&mut connection)
        .await
        .map_err(error)?;
    let identity: String = redis::cmd("CLIENT")
        .arg("INFO")
        .query_async(&mut connection)
        .await
        .map_err(error)?;
    verify_server(
        config.instance,
        &config.username,
        &server,
        &memory,
        &persistence,
        &identity,
    )?;
    Ok(connection)
}
impl SplitConnections {
    pub async fn connect(config: &SplitConfig) -> Result<Self, ValkeyError> {
        // Read both credential/CA sets before any network I/O; malformed cache
        // configuration is not disguised as an ordinary cache outage.
        for endpoint in [&config.coord, &config.cache] {
            read_password(&endpoint.password_file)?;
            read_public(&endpoint.ca_file)?;
        }
        let coord = CoordConnection {
            connection: connect(&config.coord).await?,
            endpoint: config.coord.clone(),
        };
        let cache = match connect(&config.cache).await {
            Ok(connection) => CacheAvailability::Ready(CacheConnection {
                connection,
                service: config.cache.service,
            }),
            Err(e) if super::tls_errors::cache_failure_is_optional(e) => {
                CacheAvailability::Unavailable(e)
            }
            Err(e) => return Err(e),
        };
        Ok(Self { coord, cache })
    }
}
impl CoordConnection {
    /// Independent authenticated/TLS-verified connection for the usage consumer.
    /// Supports its bounded XREADGROUP BLOCK1000 without occupying the admission
    /// manager or inheriting its 500ms response deadline. Never returns a Client.
    pub async fn dedicated_manager(&self) -> Result<ConnectionManager, ValkeyError> {
        connect_with_response_timeout(&self.endpoint, Duration::from_secs(5)).await
    }

    /// Compatibility seam only: existing sessions/lease/rate/idempotency adapters
    /// retain their public interfaces, and receive this handle exclusively.
    pub async fn memory_pressure_warning(&self) -> Result<bool, ValkeyError> {
        let info: String = redis::cmd("INFO")
            .arg("memory")
            .query_async(&mut self.connection.clone())
            .await
            .map_err(error)?;
        super::memory_pressure_warning(&info)
    }
    pub fn coordination_manager(&self) -> ConnectionManager {
        self.connection.clone()
    }
    pub async fn execute<T: FromRedisValue>(&self, command: &redis::Cmd) -> Result<T, ValkeyError> {
        command
            .query_async(&mut self.connection.clone())
            .await
            .map_err(error)
    }
}
impl CacheConnection {
    pub fn service(&self) -> ServiceIdentity {
        self.service
    }

    /// Optional cache recovery can reconnect this endpoint without touching coord.
    pub async fn connect(config: &EndpointConfig) -> Result<Self, ValkeyError> {
        if config.instance() != super::Instance::Cache {
            return Err(ValkeyError::Configuration);
        }
        Ok(Self {
            connection: connect(config).await?,
            service: config.service(),
        })
    }

    fn key(&self, key: &CacheKey) -> Result<(), ValkeyError> {
        if key.service != self.service {
            Err(ValkeyError::CacheScope)
        } else {
            Ok(())
        }
    }
    pub async fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>, ValkeyError> {
        self.key(key)?;
        // Bound allocation even if another authorized writer stored an oversized value.
        let value: Vec<u8> = redis::cmd("GETRANGE")
            .arg(key.as_str())
            .arg(0)
            .arg(1_048_576)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(error)?;
        super::policy::bounded_cache_value(value)
    }
    pub async fn set(
        &self,
        key: &CacheKey,
        value: &[u8],
        ttl_seconds: u64,
    ) -> Result<(), ValkeyError> {
        self.key(key)?;
        if value.is_empty() || value.len() > 1_048_576 || !(1..=300).contains(&ttl_seconds) {
            return Err(ValkeyError::CacheScope);
        }
        redis::cmd("SET")
            .arg(key.as_str())
            .arg(value)
            .arg("EX")
            .arg(ttl_seconds)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(error)
    }
    pub async fn invalidate(&self, key: &CacheKey) -> Result<(), ValkeyError> {
        self.key(key)?;
        let _: u64 = redis::cmd("DEL")
            .arg(key.as_str())
            .query_async(&mut self.connection.clone())
            .await
            .map_err(error)?;
        Ok(())
    }
}
