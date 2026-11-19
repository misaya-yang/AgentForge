//! The process owner opts into per-role file credentials before opening a socket.
use super::BootstrapError;
use ai_platform_service_primitives::data_connection::{
    ConnectionConfig, ConnectionPurpose, DatabaseRole,
};
use std::collections::BTreeMap;

pub fn configuration(
    environment: &BTreeMap<String, String>,
) -> Result<Option<(ConnectionConfig, ConnectionConfig)>, BootstrapError> {
    if !environment
        .keys()
        .any(|key| key.starts_with("DATA_GATEWAY_"))
    {
        return Ok(None);
    }
    let data = ConnectionConfig::from_environment(
        "DATA_GATEWAY",
        DatabaseRole::Gateway,
        ConnectionPurpose::BackendPinned,
        environment,
    )
    .map_err(|_| BootstrapError::Configuration("database_file_identity"))?;
    let control = ConnectionConfig::from_environment(
        "DATA_GATEWAY_CONTROL",
        DatabaseRole::Gateway,
        ConnectionPurpose::CancellationControl,
        environment,
    )
    .map_err(|_| BootstrapError::Configuration("database_control_file_identity"))?;
    Ok(Some((data, control)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_file_mode_never_falls_back_to_legacy_database_credentials() {
        assert!(configuration(&BTreeMap::new()).unwrap().is_none());
        for name in [
            "DATA_GATEWAY_MODE",
            "DATA_GATEWAY_CONTROL_MODE",
            "DATA_GATEWAY_DSN_FILE",
        ] {
            assert!(configuration(&BTreeMap::from([(name.into(), "incomplete".into())])).is_err());
        }
    }
}
