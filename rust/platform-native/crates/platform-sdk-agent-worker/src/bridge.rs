//! Read-only authority checks. No redirect, inherited proxy, key forwarding, or retry.
use crate::contract::{Launch, MAX_EVENT, Result};
use serde_json::{Value, json};
use std::time::Duration;
#[derive(Clone)]
pub struct Authority {
    client: reqwest::Client,
    origin: Option<String>,
}
impl Authority {
    pub fn new(origin: Option<String>) -> Result<Self> {
        Self::new_with_ca(origin, None)
    }
    pub fn new_with_ca(origin: Option<String>, ca: Option<&std::path::Path>) -> Result<Self> {
        if ca.is_some() && !origin.as_ref().is_some_and(|s| s.starts_with("https://")) {
            return Err("sdk_gateway_tls_ca_requires_https");
        }
        if let Some(base) = &origin {
            let u = reqwest::Url::parse(base).map_err(|_| "sdk_gateway_origin_invalid")?;
            let local = matches!(
                u.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            );
            if !(u.scheme() == "https" || (u.scheme() == "http" && local))
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
                || !matches!(u.path(), "" | "/")
            {
                return Err("sdk_gateway_origin_invalid");
            }
        }
        let mut builder = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10));
        if let Some(ca) = ca {
            for root in crate::transport::trust_roots(ca)? {
                builder = builder.add_root_certificate(root);
            }
        }
        let client = builder.build().map_err(|_| "sdk_knowledge_unavailable")?;
        Ok(Self { client, origin })
    }
    async fn admit(&self, kind: &str, bridge: &Value) -> Result<()> {
        let origin = self
            .origin
            .as_deref()
            .ok_or("sdk_knowledge_access_denied")?;
        let body = if kind == "capabilities" {
            json!({"run_id":bridge["run_id"]})
        } else {
            json!({})
        };
        let mut response = self
            .client
            .post(format!(
                "{}/internal/v1/sdk-tools/{kind}/admit",
                origin.trim_end_matches('/')
            ))
            .bearer_auth(
                bridge["grant"]
                    .as_str()
                    .ok_or("sdk_knowledge_access_denied")?,
            )
            .timeout(Duration::from_secs(if kind == "capabilities" {
                50
            } else {
                35
            }))
            .json(&body)
            .send()
            .await
            .map_err(|_| "sdk_knowledge_unavailable")?;
        if response.status().is_redirection() {
            return Err("sdk_knowledge_access_denied");
        }
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err("sdk_knowledge_access_denied");
        }
        if !response.status().is_success() {
            return Err("sdk_knowledge_unavailable");
        }
        let mut bytes = Vec::new();
        while let Some(part) = response
            .chunk()
            .await
            .map_err(|_| "sdk_knowledge_unavailable")?
        {
            if bytes.len() + part.len() > MAX_EVENT {
                return Err("sdk_knowledge_unavailable");
            }
            bytes.extend_from_slice(&part);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "sdk_knowledge_unavailable")?;
        if value["authorized"] != true {
            return Err("sdk_knowledge_access_denied");
        }
        Ok(())
    }
    pub async fn capability(&self, launch: &Launch) -> Result<()> {
        if let Some(b) = launch.bridge("capability_bridge") {
            self.admit("capabilities", b).await?;
        }
        Ok(())
    }
    pub async fn reconcile_cancelled_pending(
        &self,
        launch: &Launch,
        request_id: &str,
    ) -> Result<bool> {
        let bridge = launch
            .bridge("capability_bridge")
            .ok_or("sdk_knowledge_access_denied")?;
        if request_id.is_empty()
            || request_id.len() > 255
            || request_id.chars().any(char::is_control)
        {
            return Err("sdk_knowledge_access_denied");
        }
        let origin = self
            .origin
            .as_deref()
            .ok_or("sdk_knowledge_access_denied")?;
        let mut response = self
            .client
            .post(format!(
                "{}/internal/v1/sdk-tools/capabilities/admit",
                origin.trim_end_matches('/')
            ))
            .bearer_auth(
                bridge["grant"]
                    .as_str()
                    .ok_or("sdk_knowledge_access_denied")?,
            )
            .timeout(Duration::from_secs(50))
            .json(&json!({"run_id":bridge["run_id"],"reconcile_pending_request_id":request_id}))
            .send()
            .await
            .map_err(|_| "sdk_knowledge_unavailable")?;
        if response.status().is_redirection() {
            return Err("sdk_knowledge_access_denied");
        }
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err("sdk_knowledge_access_denied");
        }
        if !response.status().is_success() {
            return Err("sdk_knowledge_unavailable");
        }
        let mut bytes = Vec::new();
        while let Some(part) = response
            .chunk()
            .await
            .map_err(|_| "sdk_knowledge_unavailable")?
        {
            if bytes.len() + part.len() > MAX_EVENT {
                return Err("sdk_knowledge_unavailable");
            }
            bytes.extend_from_slice(&part);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "sdk_knowledge_unavailable")?;
        Ok(value["authorized"] == true && value["pending_reconcilable"] == true)
    }
    pub async fn access(&self, launch: &Launch) -> Result<()> {
        self.capability(launch).await?;
        if let Some(b) = launch.bridge("read_bridge") {
            self.admit("knowledge", b).await?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_ca_cannot_downgrade_to_http_or_ignore_missing_file() {
        let ca = std::path::Path::new("/does-not-exist-sdk-callback-ca.pem");
        assert!(Authority::new_with_ca(None, Some(ca)).is_err());
        assert!(Authority::new_with_ca(Some("http://gateway:8080".into()), Some(ca)).is_err());
        assert!(Authority::new_with_ca(Some("http://127.0.0.1:8080".into()), Some(ca)).is_err());
        assert!(
            Authority::new_with_ca(Some("https://gateway-sdk-callback:8097".into()), Some(ca))
                .is_err()
        );
        assert!(Authority::new(Some("http://gateway:8080".into())).is_err());
    }
    #[test]
    fn origin_policy() {
        for u in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com?secret=x",
        ] {
            assert!(Authority::new(Some(u.into())).is_err());
        }
        for u in [
            "https://example.com",
            "http://127.0.0.1:8000",
            "http://[::1]:8000",
        ] {
            assert!(Authority::new(Some(u.into())).is_ok());
        }
    }
}
