//! Canonical `src/api/v1/connectors.py` user-connector PostgreSQL statements.
//!
//! Queries select tenant overrides before filtering enabled state. Other source
//! ordering and predicates are retained, including id-only refresh
//! writes after a caller-scoped read. This is an internal credential boundary, not
//! an independently authorized public repository. `ListMine` never selects tokens.
//! Unqualified names retain the host's canonical search_path.
//!
//! The default profile models asyncpg's unconfigured text JSON codec. The source
//! forwards callback metadata dictionaries directly, with no json.dumps or pool
//! codec initializer. Its dict-encoding failure is inferred from that source path,
//! not an installed-asyncpg runtime acceptance result. Hosts with JSON object
//! codecs must explicitly use `Decoded`; selecting it does not validate the
//! default Python callback path. See connectors.py:385-403 and canonical shared
//! persistence/database.py:356-361,390-409.
//!
//! Construction and unpolled futures are inert. Every statement owns a supervised
//! transaction. Abort AND join callers, then `drain_cancelled` before pool closure.
//! Data/control pools must be independent, direct or session-pinned connections to
//! the same database/role. BEGIN advances transaction NOW() by one round-trip;
//! cancellation cannot undo COMMIT already dispatched. Backend errors and bound
//! credentials are never included in Debug, Display or returned error detail.
use crate::{
    connector_user_types::{
        ConnectorError, ConnectorFuture, ConnectorRecord, ConnectorUserOperation,
        ConnectorUserStore,
    },
    postgres_operation::PgOperationStorage,
    protocol::ProtocolTimestamp,
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{Map, Value, json, value::RawValue};
use sqlx::{
    Column, Encode, PgPool, Postgres, Row, Type, TypeInfo, ValueRef,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgRow, PgTypeInfo},
    types::Json,
};

pub const EFFECTIVE_ENABLED_CONFIG_SQL: &str = "SELECT * FROM ( SELECT * FROM connector_configs WHERE provider = $1 AND (tenant_id = $2 OR tenant_id = '') ORDER BY (tenant_id = $2) DESC LIMIT 1 ) AS effective WHERE enabled = true";
pub const AVAILABLE_CONFIGS_SQL: &str = "SELECT * FROM ( SELECT DISTINCT ON (provider) * FROM connector_configs WHERE tenant_id = $1 OR tenant_id = '' ORDER BY provider, (tenant_id = $1) DESC ) AS effective WHERE enabled = true";
pub const USER_STATUSES_SQL: &str =
    "SELECT provider, status FROM user_connectors WHERE tenant_id = $1 AND user_id = $2";
pub const LIST_MINE_SQL: &str = "SELECT provider, display_name, status, last_used_at, provider_metadata, created_at\n           FROM user_connectors WHERE tenant_id = $1 AND user_id = $2 AND status != 'revoked'";
pub const GET_USER_SQL: &str = "SELECT * FROM user_connectors\n           WHERE tenant_id = $1 AND user_id = $2 AND provider = $3";
pub const UPSERT_TOKENS_SQL: &str = "INSERT INTO user_connectors (tenant_id, user_id, provider, access_token, refresh_token,\n           token_expires_at, token_scopes, provider_metadata, status, updated_at)\n           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'connected', NOW())\n           ON CONFLICT (tenant_id, user_id, provider)\n           DO UPDATE SET access_token = $4, refresh_token = $5, token_expires_at = $6,\n                         token_scopes = $7, provider_metadata = COALESCE($8, user_connectors.provider_metadata),\n                         status = 'connected', last_error = NULL, updated_at = NOW()";
pub const MARK_REFRESH_FAILED_SQL: &str = "UPDATE user_connectors SET status = 'expired', last_error = $1, updated_at = NOW() WHERE id = $2";
pub const SAVE_REFRESHED_SQL: &str = "UPDATE user_connectors SET access_token = $1, refresh_token = COALESCE($2, refresh_token),\n               token_expires_at = $3, status = 'connected', updated_at = NOW() WHERE id = $4";
pub const REVOKE_SQL: &str = "UPDATE user_connectors SET status = 'revoked', access_token = NULL,\n           refresh_token = NULL, updated_at = NOW()\n           WHERE tenant_id = $1 AND user_id = $2 AND provider = $3";

/// Both profiles apply to config.extra_config and user.provider_metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectorJsonProfile {
    /// asyncpg's unconfigured JSON/JSONB codec: wire JSON is returned as text;
    /// a supplied string is raw JSON, not a JSON string. Non-string input fails.
    #[default]
    AsyncpgText,
    /// An explicitly configured json.dumps/json.loads-style codec: values are
    /// encoded/decoded as JSON, including a string containing JSON remaining a
    /// string. This does not normalize legacy JSON-string metadata to an object.
    Decoded,
}

pub struct PgConnectorUserStore {
    storage: PgOperationStorage,
    json_profile: ConnectorJsonProfile,
}
impl PgConnectorUserStore {
    pub fn new(data: PgPool, control: PgPool) -> Result<Self, ConnectorError> {
        Ok(Self {
            storage: PgOperationStorage::new(data, control)
                .map_err(|_| ConnectorError::storage())?,
            json_profile: ConnectorJsonProfile::AsyncpgText,
        })
    }
    /// Inert host configuration; it opens no connection and inspects no env vars.
    pub fn with_json_profile(mut self, profile: ConnectorJsonProfile) -> Self {
        self.json_profile = profile;
        self
    }
    pub fn json_profile(&self) -> ConnectorJsonProfile {
        self.json_profile
    }
    pub async fn drain_cancelled(&self) -> Result<(), ConnectorError> {
        self.storage
            .drain_cancelled()
            .await
            .map_err(|_| ConnectorError::storage())
    }
}
impl ConnectorUserStore for PgConnectorUserStore {
    fn run(&self, operation: ConnectorUserOperation) -> ConnectorFuture<'_, Vec<ConnectorRecord>> {
        Box::pin(async move {
            let profile = self.json_profile;
            let mut connection = self
                .storage
                .acquire()
                .await
                .map_err(|_| ConnectorError::storage())?;
            let rows = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        match operation {
                            ConnectorUserOperation::EffectiveEnabledConfig {
                                provider,
                                tenant_id,
                            } => Ok(sqlx::query(EFFECTIVE_ENABLED_CONFIG_SQL)
                                .bind(provider)
                                .bind(tenant_id)
                                .fetch_optional(connection)
                                .await?
                                .into_iter()
                                .collect()),
                            ConnectorUserOperation::AvailableConfigs { tenant_id } => {
                                sqlx::query(AVAILABLE_CONFIGS_SQL)
                                    .bind(tenant_id)
                                    .fetch_all(connection)
                                    .await
                            }
                            ConnectorUserOperation::UserStatuses { user } => {
                                sqlx::query(USER_STATUSES_SQL)
                                    .bind(user.tenant_id)
                                    .bind(user.user_id)
                                    .fetch_all(connection)
                                    .await
                            }
                            ConnectorUserOperation::ListMine { user } => {
                                sqlx::query(LIST_MINE_SQL)
                                    .bind(user.tenant_id)
                                    .bind(user.user_id)
                                    .fetch_all(connection)
                                    .await
                            }
                            ConnectorUserOperation::GetUser { user, provider } => {
                                Ok(sqlx::query(GET_USER_SQL)
                                    .bind(user.tenant_id)
                                    .bind(user.user_id)
                                    .bind(provider)
                                    .fetch_optional(connection)
                                    .await?
                                    .into_iter()
                                    .collect())
                            }
                            ConnectorUserOperation::UpsertTokens(write) => {
                                // None binds SQL NULL, never JSON null. A Decoded Some(Null)
                                // binds JSON null; AsyncpgText represents it as Some("null").
                                let metadata = encode_metadata(profile, write.metadata)?;
                                sqlx::query(UPSERT_TOKENS_SQL)
                                    .bind(write.user.tenant_id)
                                    .bind(write.user.user_id)
                                    .bind(write.provider)
                                    .bind(write.access_token)
                                    .bind(write.refresh_token)
                                    .bind(write.expires_at)
                                    .bind(write.scopes)
                                    .bind(metadata.map(Json))
                                    .execute(connection)
                                    .await?;
                                Ok(Vec::new())
                            }
                            ConnectorUserOperation::MarkRefreshFailed { id } => {
                                sqlx::query(MARK_REFRESH_FAILED_SQL)
                                    .bind("Refresh failed")
                                    .bind(PgConnectorUuid::from_value(id)?)
                                    .execute(connection)
                                    .await?;
                                Ok(Vec::new())
                            }
                            ConnectorUserOperation::SaveRefreshed {
                                id,
                                access_token,
                                refresh_token,
                                expires_at,
                            } => {
                                sqlx::query(SAVE_REFRESHED_SQL)
                                    .bind(access_token)
                                    .bind(refresh_token)
                                    .bind(expires_at)
                                    .bind(PgConnectorUuid::from_value(id)?)
                                    .execute(connection)
                                    .await?;
                                Ok(Vec::new())
                            }
                            ConnectorUserOperation::Revoke { user, provider } => {
                                sqlx::query(REVOKE_SQL)
                                    .bind(user.tenant_id)
                                    .bind(user.user_id)
                                    .bind(provider)
                                    .execute(connection)
                                    .await?;
                                Ok(Vec::new())
                            }
                        }
                    })
                })
                .await
                .map_err(|_| ConnectorError::storage())?;
            rows.iter().map(|row| raw_row(row, profile)).collect()
        })
    }
}

fn binding_error() -> sqlx::Error {
    sqlx::Error::Protocol("connector_storage_value_invalid".into())
}
fn encode_metadata(
    profile: ConnectorJsonProfile,
    value: Option<Value>,
) -> Result<Option<Box<RawValue>>, sqlx::Error> {
    value
        .map(|value| {
            let text = match profile {
                ConnectorJsonProfile::AsyncpgText => {
                    value.as_str().ok_or_else(binding_error)?.to_owned()
                }
                ConnectorJsonProfile::Decoded => {
                    serde_json::to_string(&value).map_err(|_| binding_error())?
                }
            };
            RawValue::from_string(text).map_err(|_| binding_error())
        })
        .transpose()
}

// Bind PostgreSQL UUID in its native binary format without changing canonical
// SQL to add casts or requiring a Cargo feature change. Values have no Debug.
struct PgConnectorUuid(uuid::Uuid);
impl PgConnectorUuid {
    fn from_value(value: Value) -> Result<Self, sqlx::Error> {
        uuid::Uuid::parse_str(value.as_str().ok_or_else(binding_error)?)
            .map(Self)
            .map_err(|_| binding_error())
    }
}
impl Type<Postgres> for PgConnectorUuid {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("uuid")
    }
}
impl Encode<'_, Postgres> for PgConnectorUuid {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        buffer.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }
    fn size_hint(&self) -> usize {
        16
    }
}

fn raw_row(row: &PgRow, profile: ConnectorJsonProfile) -> Result<ConnectorRecord, ConnectorError> {
    let bad = |_: sqlx::Error| ConnectorError::storage();
    let mut object = Map::new();
    for column in row.columns() {
        let index = column.ordinal();
        let cell = row.try_get_raw(index).map_err(bad)?;
        let value = if cell.is_null() {
            Value::Null
        } else {
            match column.type_info().name() {
                "INT2" => json!(row.try_get::<i16, _>(index).map_err(bad)?),
                "INT4" => json!(row.try_get::<i32, _>(index).map_err(bad)?),
                "INT8" => json!(row.try_get::<i64, _>(index).map_err(bad)?),
                "BOOL" => json!(row.try_get::<bool, _>(index).map_err(bad)?),
                "UUID" => json!(
                    uuid::Uuid::from_slice(cell.as_bytes().map_err(|_| ConnectorError::storage())?)
                        .map_err(|_| ConnectorError::storage())?
                        .to_string()
                ),
                "TIMESTAMPTZ" => json!(timestamp(
                    cell.as_bytes().map_err(|_| ConnectorError::storage())?
                )?),
                "JSON" | "JSONB" => {
                    let raw = row.try_get::<Json<Box<RawValue>>, _>(index).map_err(bad)?.0;
                    match profile {
                        ConnectorJsonProfile::AsyncpgText => Value::String(raw.get().to_owned()),
                        ConnectorJsonProfile::Decoded => serde_json::from_str(raw.get())
                            .map_err(|_| ConnectorError::storage())?,
                    }
                }
                _ => json!(row.try_get::<String, _>(index).map_err(bad)?),
            }
        };
        object.insert(column.name().into(), value);
    }
    Ok(ConnectorRecord(Value::Object(object)))
}

// asyncpg's timestamptz codec maps infinity to *naive* datetime.max/min.
// Keep those offsets absent; a subsequent source comparison against aware UTC
// can then fail rather than silently treating the sentinel as a valid expiry.
// Other BC/out-of-range values fail like Python datetime; finite UTC values
// retain Python's zero-or-six fractional digits.
fn timestamp(bytes: &[u8]) -> Result<String, ConnectorError> {
    let micros = i64::from_be_bytes(bytes.try_into().map_err(|_| ConnectorError::storage())?);
    match micros {
        i64::MIN => return Ok("0001-01-01T00:00:00".into()),
        i64::MAX => return Ok("9999-12-31T23:59:59.999999".into()),
        _ => (),
    }
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1)
        .expect("valid epoch")
        .and_hms_opt(0, 0, 0)
        .expect("midnight");
    let value = epoch
        .checked_add_signed(Duration::microseconds(micros))
        .filter(|value| (1..=9999).contains(&value.year()))
        .ok_or_else(ConnectorError::storage)?;
    Ok(ProtocolTimestamp::from(
        DateTime::<Utc>::from_naive_utc_and_offset(value, Utc).fixed_offset(),
    )
    .to_isoformat())
}
