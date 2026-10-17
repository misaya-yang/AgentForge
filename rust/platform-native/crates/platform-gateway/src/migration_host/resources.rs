//! Concrete process-owned resources, from Container.initialize/shutdown.
//! No schema bootstrap is performed by the application process.
use super::lifecycle::HostError;
use ai_platform_service_primitives::valkey::{
    CacheAvailability, CoordConnection, SplitConfig, SplitConnections,
};
use redis::aio::ConnectionManager;
use serde::Deserialize;
use sqlx::{PgPool, postgres::PgPoolOptions};
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseSettings {
    pub enabled: bool,
    pub dsn: String,
    pub auto_init: bool,
    pub permission_cache_ttl_seconds: u64,
    pub pool_min_size: u32,
    pub pool_max_size: u32,
    pub api_key_usage_flush_interval_seconds: u64,
    pub api_key_usage_flush_batch_size: usize,
}
impl Default for DatabaseSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            dsn: "postgresql://localhost:5432/gateway".into(),
            auto_init: false,
            permission_cache_ttl_seconds: 60,
            pool_min_size: 2,
            pool_max_size: 10,
            api_key_usage_flush_interval_seconds: 2,
            api_key_usage_flush_batch_size: 100,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct RedisSettings {
    pub enabled: bool,
    pub url: String,
}
impl Default for RedisSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "redis://localhost:6379/0".into(),
        }
    }
}
pub struct ConnectionResources {
    pub database: Option<PgPool>,
    pub independent_control: Option<PgPool>,
    pub redis: Option<ConnectionManager>,
    /// In split mode all legacy session/rate/lease adapters alias coord only.
    pub coordination: Option<CoordConnection>,
    /// Optional acceleration; never supplied to a coordination adapter.
    pub cache: Option<CacheAvailability>,
    pub database_enabled: bool,
    pub redis_enabled: bool,
}
impl ConnectionResources {
    /// Connectivity failures are retained as unavailable resources, matching the
    /// source's best-effort container initialization; invalid config is fatal.
    pub async fn connect(
        database: &DatabaseSettings,
        redis: &RedisSettings,
    ) -> Result<Self, HostError> {
        Self::connect_split(database, redis, None).await
    }
    pub async fn connect_split(
        database: &DatabaseSettings,
        redis: &RedisSettings,
        split: Option<&SplitConfig>,
    ) -> Result<Self, HostError> {
        if database.auto_init
            || database.pool_max_size == 0
            || database.pool_min_size > database.pool_max_size
        {
            return Err(HostError::Configuration);
        }
        let pool = if database.enabled {
            PgPoolOptions::new()
                .min_connections(database.pool_min_size)
                .max_connections(database.pool_max_size)
                .connect(&database.dsn)
                .await
                .ok()
        } else {
            None
        };
        let control = if pool.is_some() {
            PgPoolOptions::new()
                .min_connections(0)
                .max_connections(2)
                .connect(&database.dsn)
                .await
                .ok()
        } else {
            None
        };
        let (redis_connection, coordination, cache) = if let Some(split) = split {
            let connections = SplitConnections::connect(split)
                .await
                .map_err(|_| HostError::Configuration)?;
            (
                Some(connections.coord.coordination_manager()),
                Some(connections.coord),
                Some(connections.cache),
            )
        } else if redis.enabled {
            let connection = match redis::Client::open(redis.url.as_str()) {
                Ok(client) => ConnectionManager::new(client).await.ok(),
                Err(_) => None,
            };
            (connection, None, None)
        } else {
            (None, None, None)
        };
        Ok(Self {
            database: pool,
            independent_control: control,
            redis: redis_connection,
            coordination,
            cache,
            database_enabled: database.enabled,
            redis_enabled: split.is_some() || redis.enabled,
        })
    }
    pub async fn close(self) {
        if let Some(pool) = self.database {
            pool.close().await;
        }
        if let Some(pool) = self.independent_control {
            pool.close().await;
        }
        drop(self.redis);
    }
}
