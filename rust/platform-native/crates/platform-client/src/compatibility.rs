//! Read the existing Gateway release identity without asserting Runtime health.
use crate::{Client, Error};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GatewayVersion {
    pub schema_version: String,
    pub service_id: String,
    pub release_id: String,
    pub git_sha: String,
    pub image_digest: String,
    pub manifest_sha256: String,
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Client {
    /// One bounded same-origin GET. This is identity, not a Runtime handshake.
    pub async fn gateway_version(&self) -> Result<GatewayVersion, Error> {
        let value = self.get(&["version"]).await?;
        if value["schema_version"] != "ai-platform/service-version/v1"
            || value["service_id"] != "gateway"
        {
            return Err(Error::Contract(
                "Gateway version contract mismatch: this client requires gateway ai-platform/service-version/v1",
            ));
        }
        let version: GatewayVersion = serde_json::from_value(value).map_err(|_| Error::Response)?;
        if version.release_id.trim().is_empty()
            || version.release_id.len() > 256
            || version.release_id.chars().any(char::is_control)
            || !lower_hex(&version.git_sha, 40)
            || !lower_hex(&version.manifest_sha256, 64)
            || !version.image_digest.strip_prefix("sha256:").is_some_and(|s| lower_hex(s, 64))
        {
            return Err(Error::Response);
        }
        Ok(version)
    }
}
