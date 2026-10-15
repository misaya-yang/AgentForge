//! Exact safe /version projection from the unified compatibility manifest.
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
#[derive(Clone)]
pub struct ReleaseIdentity {
    pub manifest_path: PathBuf,
    pub service_id: String,
    pub runtime_release: Option<String>,
    pub runtime_git: Option<String>,
    pub runtime_image: Option<String>,
}
impl ReleaseIdentity {
    pub fn gateway(environment: &BTreeMap<String, String>) -> Self {
        Self {
            manifest_path: environment
                .get("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/release/compatibility-manifest.json")),
            service_id: "gateway".into(),
            runtime_release: environment.get("AI_PLATFORM_RELEASE_ID").cloned(),
            runtime_git: environment.get("AI_PLATFORM_GIT_SHA").cloned(),
            runtime_image: environment.get("AI_PLATFORM_IMAGE_DIGEST").cloned(),
        }
    }
    pub fn project(&self, raw: &[u8]) -> Result<Value, ReleaseUnavailable> {
        let manifest: Value = serde_json::from_slice(raw).map_err(|_| ReleaseUnavailable)?;
        if !manifest.is_object()
            || manifest["schema_version"] != "ai-platform/compatibility-manifest/v1"
            || !matches!(
                manifest["status"].as_str(),
                Some("release_candidate" | "released")
            )
        {
            return Err(ReleaseUnavailable);
        }
        let release = manifest["release_id"].as_str().ok_or(ReleaseUnavailable)?;
        let sha = manifest["source"]["git_sha"]
            .as_str()
            .filter(|s| lower_hex(s, 40))
            .ok_or(ReleaseUnavailable)?;
        let record = manifest["services"]
            .get(&self.service_id)
            .filter(|v| v.is_object())
            .ok_or(ReleaseUnavailable)?;
        let digest = record["image_digest"]
            .as_str()
            .filter(|s| s.strip_prefix("sha256:").is_some_and(|s| lower_hex(s, 64)))
            .ok_or(ReleaseUnavailable)?;
        let expected = &record["identity"];
        if !expected.is_object() || expected["mode"] != "http_version" {
            return Err(ReleaseUnavailable);
        }
        let payload = json!({"schema_version":"ai-platform/service-version/v1","service_id":self.service_id,"release_id":release,"git_sha":sha,"image_digest":digest,"manifest_sha256":format!("{:x}",Sha256::digest(raw))});
        for key in ["service_id", "release_id", "git_sha", "image_digest"] {
            if expected.get(key) != payload.get(key) {
                return Err(ReleaseUnavailable);
            }
        }
        if self.runtime_release.as_deref().filter(|s| !s.is_empty()) != Some(release)
            || self.runtime_git.as_deref().filter(|s| !s.is_empty()) != Some(sha)
            || self.runtime_image.as_deref().filter(|s| !s.is_empty()) != Some(digest)
        {
            return Err(ReleaseUnavailable);
        }
        Ok(payload)
    }
    pub async fn read(&self) -> Result<Value, ReleaseUnavailable> {
        let raw = tokio::fs::read(&self.manifest_path)
            .await
            .map_err(|_| ReleaseUnavailable)?;
        self.project(&raw)
    }
}
#[derive(Debug)]
pub struct ReleaseUnavailable;
fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
async fn version(State(state): State<Arc<ReleaseIdentity>>) -> Response {
    match state.read().await{Ok(payload)=>Json(payload).into_response(),Err(_)=>(StatusCode::SERVICE_UNAVAILABLE,Json(json!({"schema_version":"ai-platform/service-version/v1","service_id":state.service_id,"status":"unavailable","code":"RELEASE_MANIFEST_UNAVAILABLE"}))).into_response()}
}
pub fn router(identity: Arc<ReleaseIdentity>) -> Router {
    Router::new()
        .route("/version", get(version))
        .with_state(identity)
}
