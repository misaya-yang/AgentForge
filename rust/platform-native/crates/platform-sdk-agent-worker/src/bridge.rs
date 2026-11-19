//! Read-only authority checks. No redirect, inherited proxy, key forwarding, or retry.
use crate::workload_identity::WorkloadSend;
use crate::contract::{Launch, MAX_EVENT, Result};
use serde_json::{Value, json};
use std::time::Duration;
#[derive(Clone)]
pub struct Authority {
    client: reqwest::Client,
    origin: Option<String>,
    #[cfg(test)]
    fixture_model_plane: bool,
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
        if let Some(identity) = crate::workload_identity::WorkloadIdentity::from_environment()? {
            identity.validate_endpoint(origin.as_deref().ok_or("sdk_gateway_origin_invalid")?)?;
            builder = identity.apply(builder);
        }
        let client = builder.build().map_err(|_| "sdk_knowledge_unavailable")?;
        Ok(Self {
            client,
            origin,
            #[cfg(test)]
            fixture_model_plane: false,
        })
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
            .workload_send()
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
            .workload_send()
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

impl Authority {
    #[cfg(test)]
    pub fn with_model_fixtures(mut self) -> Self {
        self.fixture_model_plane = true;
        self
    }
    pub async fn model_lease(&self, launch: &Launch) -> Result<crate::model_plane::ModelPlane> {
        #[cfg(test)]
        if self.fixture_model_plane {
            return Ok(crate::model_plane::ModelPlane {
                schema: "sdk-model-plane/v1".into(),
                url: "http://127.0.0.1/internal/v1/sdk-tools/models/execute".into(),
                revocation_url: "http://127.0.0.1/internal/v1/sdk-tools/models/revocations".into(),
                lease: "a".repeat(64),
                run_id: launch.body["capability_bridge"]["run_id"]
                    .as_str()
                    .unwrap_or("00000000-0000-0000-0000-000000000001")
                    .into(),
                expires_at_epoch_ms: crate::model_plane::now_ms() + 60_000,
                binding_sha256: crate::model_plane::launch_binding(&launch.body),
            });
        }
        let bridge = launch
            .bridge("capability_bridge")
            .ok_or("sdk_model_lease_required")?;
        let origin = self.origin.as_deref().ok_or("sdk_gateway_origin_invalid")?;
        let mut response = self
            .client
            .post(format!(
                "{}/internal/v1/sdk-tools/models/lease",
                origin.trim_end_matches('/')
            ))
            .bearer_auth(bridge["grant"].as_str().ok_or("sdk_model_lease_required")?)
            .json(&json!({"run_id":bridge["run_id"]}))
            .timeout(Duration::from_secs(10))
            .workload_send()
            .await
            .map_err(|_| "sdk_model_lease_unavailable")?;
        if !response.status().is_success() {
            return Err("sdk_model_lease_not_admitted");
        }
        let mut bytes = vec![];
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "sdk_model_lease_unavailable")?
        {
            if bytes.len() + chunk.len() > 16_384 {
                return Err("sdk_model_lease_invalid");
            }
            bytes.extend_from_slice(&chunk);
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Lease {
            schema: String,
            expires_at_epoch_ms: u64,
            binding_sha256: String,
        }
        let lease: Lease = serde_json::from_slice(&bytes).map_err(|_| "sdk_model_lease_invalid")?;
        let plane = crate::model_plane::ModelPlane {
            schema: lease.schema,
            url: format!(
                "{}/internal/v1/sdk-tools/models/execute",
                origin.trim_end_matches('/')
            ),
            revocation_url: format!(
                "{}/internal/v1/sdk-tools/models/revocations",
                origin.trim_end_matches('/')
            ),
            lease: bridge["grant"]
                .as_str()
                .ok_or("sdk_model_lease_required")?
                .to_owned(),
            run_id: bridge["run_id"]
                .as_str()
                .ok_or("sdk_model_lease_required")?
                .to_owned(),
            expires_at_epoch_ms: lease.expires_at_epoch_ms,
            binding_sha256: lease.binding_sha256,
        };
        plane.validate_launch(&launch.body)?;
        Ok(plane)
    }
    /// A disconnect, malformed push or missing heartbeat revokes local execution.
    /// The original ten-second live-authority check remains an independent backstop.
    pub async fn watch_revocation(&self, launch: &Launch) -> Result<()> {
        #[cfg(test)]
        if self.fixture_model_plane {
            return std::future::pending().await;
        }
        let plane = launch
            .model_plane
            .as_ref()
            .ok_or("sdk_model_lease_required")?;
        let mut response = self
            .client
            .post(&plane.revocation_url)
            .bearer_auth(&plane.lease)
            .json(&json!({"run_id":plane.run_id}))
            .timeout(plane.remaining()?)
            .workload_send()
            .await
            .map_err(|_| "sdk_revocation_stream_unavailable")?;
        if !response.status().is_success() {
            return Err("sdk_revocation_stream_rejected");
        }
        let mut pending = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(15), response.chunk())
                .await
                .map_err(|_| "sdk_revocation_stream_unavailable")?
                .map_err(|_| "sdk_revocation_stream_unavailable")?;
            let Some(chunk) = next else {
                return Err("sdk_execution_revoked");
            };
            pending.extend_from_slice(&chunk);
            if pending.len() > 16_384 {
                return Err("sdk_revocation_event_invalid");
            }
            while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let text = std::str::from_utf8(&line)
                    .map_err(|_| "sdk_revocation_event_invalid")?
                    .trim();
                let Some(data) = text.strip_prefix("data:") else {
                    continue;
                };
                let event: Value =
                    serde_json::from_str(data).map_err(|_| "sdk_revocation_event_invalid")?;
                if event["run_id"] != plane.run_id {
                    return Err("sdk_revocation_event_invalid");
                }
                match event["type"].as_str() {
                    Some("sdk.lease_heartbeat") => {}
                    Some("sdk.lease_revoked") => return Err("sdk_execution_revoked"),
                    _ => return Err("sdk_revocation_event_invalid"),
                }
            }
        }
    }
}

#[cfg(test)]
mod model_plane_boundary_tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
    };
    fn launch() -> Launch {
        Launch::parse(json!({"protocol":crate::contract::PROTOCOL,"framework":"openai_agents","service_id":"s","tenant_id":"t","user_id":"u","request_id":"q","session_id":"session","agent":{"model":"m","instructions":"","max_turns":2},"input":"hello","stream":true,
            "capability_bridge":{"grant":"a".repeat(64),"run_id":"00000000-0000-0000-0000-000000000001","tools":[],"timeout_seconds":300}})).unwrap()
    }
    async fn lease(headers: HeaderMap, Json(body): Json<Value>) -> Response {
        assert_eq!(
            headers["authorization"],
            format!("Bearer {}", "a".repeat(64))
        );
        assert!(!headers.contains_key("x-sdk-provider-key"));
        assert_eq!(
            body,
            json!({"run_id":"00000000-0000-0000-0000-000000000001"})
        );
        Json(json!({"schema":"sdk-model-plane/v1","expires_at_epoch_ms":crate::model_plane::now_ms()+30_000,"binding_sha256":crate::model_plane::launch_binding(&launch().body)})).into_response()
    }
    async fn revoke() -> Response {
        ([("content-type","text/event-stream")],Body::from("data: {\"type\":\"sdk.lease_heartbeat\",\"run_id\":\"00000000-0000-0000-0000-000000000001\"}\n\ndata: {\"type\":\"sdk.lease_revoked\",\"run_id\":\"00000000-0000-0000-0000-000000000001\"}\n\n")).into_response()
    }
    #[tokio::test]
    async fn real_http_model_lease_and_revocation_never_use_provider_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/internal/v1/sdk-tools/models/lease", post(lease))
            .route("/internal/v1/sdk-tools/models/revocations", post(revoke));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let authority = Authority::new(Some(origin.clone())).unwrap();
        let mut launch = launch();
        let plane = authority.model_lease(&launch).await.unwrap();
        assert_eq!(
            plane.url,
            format!("{origin}/internal/v1/sdk-tools/models/execute")
        );
        launch.model_plane = Some(plane);
        assert_eq!(
            authority.watch_revocation(&launch).await.unwrap_err(),
            "sdk_execution_revoked"
        );
        server.abort();
    }
    #[tokio::test]
    async fn rejected_model_authority_never_returns_an_engine_launch_envelope() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority =
            Authority::new(Some(format!("http://{}", listener.local_addr().unwrap()))).unwrap();
        let router = Router::new().route(
            "/internal/v1/sdk-tools/models/lease",
            post(|| async { StatusCode::FORBIDDEN }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        assert_eq!(
            authority.model_lease(&launch()).await.err(),
            Some("sdk_model_lease_not_admitted")
        );
        server.abort();
    }
}
