//! asyncpg's UTC-aware row formatting and explicit naive-input timezone policy.
//! Python datetime.isoformat uses six fractional digits when microseconds exist.
use crate::{
    config_api_keys_store::{ConfigApiKeysStoreError, ConfigApiKeysTimestampFormatter},
    protocol::ProtocolTimestamp,
    quota_admin_api::{QuotaAdminResult, QuotaTimestampBinder},
    rate_config_api::{RateConfigError, RateConfigTimestampFormatter},
    roles_store::{RolesStoreError, RolesTimestampFormatter},
    self_service_api_keys::SelfServiceApiKeyError,
    self_service_api_keys_store::KeyTimestampBinding,
};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
#[derive(Clone, Copy)]
pub struct DatabaseTimestampPolicy {
    /// The source host's timezone for naive datetime input. UTC deployments use
    /// FixedOffset::east_opt(0); an offset must never be inferred from a request.
    pub naive_offset: FixedOffset,
}
impl DatabaseTimestampPolicy {
    fn encoded(value: DateTime<Utc>) -> String {
        ProtocolTimestamp::Offset(value.fixed_offset()).to_isoformat()
    }
    fn instant(&self, value: ProtocolTimestamp) -> Option<DateTime<Utc>> {
        match value {
            ProtocolTimestamp::Offset(v) => Some(v.with_timezone(&Utc)),
            ProtocolTimestamp::Naive(v) => self
                .naive_offset
                .from_local_datetime(&v)
                .single()
                .map(|value| value.with_timezone(&Utc)),
        }
    }
}
impl ConfigApiKeysTimestampFormatter for DatabaseTimestampPolicy {
    fn format(&self, value: DateTime<Utc>) -> Result<String, ConfigApiKeysStoreError> {
        Ok(Self::encoded(value))
    }
}
impl RolesTimestampFormatter for DatabaseTimestampPolicy {
    fn format(&self, value: DateTime<Utc>) -> Result<String, RolesStoreError> {
        Ok(Self::encoded(value))
    }
}
impl RateConfigTimestampFormatter for DatabaseTimestampPolicy {
    fn format(&self, value: DateTime<Utc>) -> Result<String, RateConfigError> {
        Ok(Self::encoded(value))
    }
}
impl KeyTimestampBinding for DatabaseTimestampPolicy {
    fn bind(&self, value: ProtocolTimestamp) -> Result<DateTime<Utc>, SelfServiceApiKeyError> {
        self.instant(value).ok_or(SelfServiceApiKeyError::Internal)
    }
}
impl QuotaTimestampBinder for DatabaseTimestampPolicy {
    fn bind(&self, value: ProtocolTimestamp) -> QuotaAdminResult<DateTime<Utc>> {
        self.instant(value)
            .ok_or(crate::quota_admin_api::QuotaAdminError::Internal)
    }
}
