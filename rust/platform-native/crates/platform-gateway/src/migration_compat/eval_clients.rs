//! Bounded evaluation clients. Construction is inert and configuration is injected.
use super::{
    common::{CompatError, hex, text},
    eval::normalize_ragas_results,
};
use ai_platform_auth::{JwtSigner, TokenCodec};
use hmac::{Hmac, Mac};
use jsonwebtoken::Algorithm;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

pub struct EvalJwtSigner {
    signer: JwtSigner,
    audience: Option<String>,
    issuer: Option<String>,
}
impl EvalJwtSigner {
    pub fn new(
        secret: &[u8],
        algorithm: Algorithm,
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Result<Self, CompatError> {
        let invalid = || CompatError::new(503, "Eval judge JWT configuration invalid");
        if algorithm != Algorithm::HS256 {
            return Err(invalid());
        }
        let codec =
            TokenCodec::new(secret, audience.clone(), issuer.clone()).map_err(|_| invalid())?;
        Ok(Self {
            signer: codec.signer(),
            audience,
            issuer,
        })
    }

    pub fn from_signer(
        signer: JwtSigner,
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Self {
        Self {
            signer,
            audience,
            issuer,
        }
    }
    pub fn token(&self, tenant: &str, delegation: Option<&Value>) -> Result<String, CompatError> {
        let user = if let Some(delegation) = delegation {
            if text(delegation, "tenant_id") != tenant
                || text(delegation, "actor") != "eval-worker"
                || ["subject", "run_id", "job_id"]
                    .iter()
                    .any(|k| text(delegation, k).is_empty())
            {
                return Err(CompatError::new(403, "AGENT_EVAL_DELEGATION_INVALID"));
            }
            text(delegation, "subject")
        } else {
            "eval-worker"
        };
        let now = chrono::Utc::now().timestamp();
        let mut payload = json!({"sub":user,"user_id":user,"tenant_id":tenant,"roles":["user"],"permissions":[],"tier":"normal","iat":now,"exp":now+300});
        if let Some(delegation) = delegation {
            payload["act"] = json!({"sub":"eval-worker","purpose":"eval_candidate","tenant_id":tenant,"run_id":delegation["run_id"],"job_id":delegation["job_id"]});
        }
        if let Some(audience) = &self.audience {
            payload["aud"] = json!(audience);
        }
        if let Some(issuer) = &self.issuer {
            payload["iss"] = json!(issuer);
        }
        self.signer
            .sign(&payload)
            .map_err(|_| CompatError::new(503, "Eval judge JWT signing failed"))
    }
}
#[derive(Clone, Default)]
pub struct LlmCompleteContext {
    pub tenant_id: String,
    pub trace_family: String,
    pub trace_id: Option<String>,
    pub provider_id: Option<String>,
    pub model_ref: Option<Value>,
}
pub struct EvalGatewayLlmClient {
    pub enabled: bool,
    base: reqwest::Url,
    http: reqwest::Client,
    pub signer: Arc<EvalJwtSigner>,
    pub default_model: String,
    pub system_tenant: String,
}
fn configured_origin(base: &str) -> Result<reqwest::Url, CompatError> {
    let url =
        reqwest::Url::parse(base).map_err(|_| CompatError::new(503, "Invalid eval service URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CompatError::new(503, "Invalid eval service URL"));
    }
    Ok(url)
}
impl EvalGatewayLlmClient {
    pub fn new(
        base: &str,
        signer: Arc<EvalJwtSigner>,
        timeout: Duration,
        enabled: bool,
        default_model: String,
        system_tenant: String,
    ) -> Result<Self, CompatError> {
        Ok(Self {
            base: configured_origin(base)?,
            http: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(timeout)
                .build()?,
            signer,
            enabled,
            default_model,
            system_tenant,
        })
    }
    pub async fn complete(
        &self,
        model: &str,
        prompt: &str,
        context: &LlmCompleteContext,
    ) -> Result<String, CompatError> {
        if !self.enabled {
            return Err(CompatError::new(503, "Eval LLM judge disabled"));
        }
        let tenant = if context.tenant_id.is_empty() {
            &self.system_tenant
        } else {
            &context.tenant_id
        };
        let model = if model.is_empty() {
            &self.default_model
        } else {
            model
        };
        let mut body = json!({"input":prompt,"model":model,"temperature":0,"max_output_tokens":512,"tools":[],"tool_choice":"none","parallel_tool_calls":false,"stream":false});
        if let Some(reference) = &context.model_ref {
            body["expected_model_ref"] = reference.clone();
        }
        if let Some(provider) = &context.provider_id {
            body["provider_id"] = json!(provider);
        }
        let mut url = self.base.clone();
        url.set_path(&format!(
            "{}/v1/responses",
            url.path().trim_end_matches('/')
        ));
        let response = self
            .http
            .post(url)
            .bearer_auth(self.signer.token(tenant, None)?)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(CompatError::new(
                502,
                format!(
                    "Gateway Agent Runtime judge failed with HTTP {}",
                    status.as_u16()
                ),
            ));
        }
        let payload = bounded_json(response, 4 * 1024 * 1024).await?;
        let content = text(&payload, "output_text").trim();
        if content.is_empty() {
            return Err(CompatError::new(
                502,
                "Gateway Agent Runtime judge returned empty content",
            ));
        }
        Ok(content.into())
    }
}
pub struct KbRagasClient {
    base: reqwest::Url,
    http: reqwest::Client,
    secret: Option<Vec<u8>>,
    key_id: String,
}
impl KbRagasClient {
    pub fn new(
        base: &str,
        secret: Option<Vec<u8>>,
        key_id: String,
        timeout: Duration,
    ) -> Result<Self, CompatError> {
        if key_id.contains(':') || key_id.is_empty() {
            return Err(CompatError::new(503, "Invalid KB signer key ID"));
        }
        Ok(Self {
            base: configured_origin(base)?,
            http: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(timeout)
                .build()?,
            secret,
            key_id,
        })
    }
    pub async fn evaluate_retrieval(
        &self,
        query: &str,
        contexts: &[String],
        answer: Option<&str>,
        metrics: Option<&[String]>,
        ground_truth: Option<&str>,
        llm_config: Option<&Value>,
    ) -> Result<Value, CompatError> {
        let selector = if let Some(config) = llm_config.filter(|v| !v.is_null()) {
            let map = config.as_object().ok_or_else(|| {
                CompatError::new(
                    422,
                    "KB RAGAS judge selector only accepts provider and model",
                )
            })?;
            if map
                .keys()
                .any(|key| !matches!(key.as_str(), "provider" | "model"))
            {
                return Err(CompatError::new(
                    422,
                    "KB RAGAS judge selector only accepts provider and model",
                ));
            }
            json!(
                map.iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<serde_json::Map<_, _>>()
            )
        } else {
            Value::Null
        };
        let body=serde_json::to_vec(&json!({"query":query,"contexts":contexts,"answer":answer,"metrics":metrics,"ground_truth":ground_truth,"llm_config":selector})).map_err(|_|CompatError::new(422,"Invalid RAGAS payload"))?;
        let path = "/api/v1/internal/eval/ragas";
        let mut url = self.base.clone();
        url.set_path(&format!("{}{}", url.path().trim_end_matches('/'), path));
        let mut request = self
            .http
            .post(url)
            .header("content-type", "application/json");
        if let Some(secret) = &self.secret {
            let id = uuid::Uuid::new_v4().simple().to_string();
            let timestamp = chrono::Utc::now().timestamp_millis().to_string();
            let hash = hex(&Sha256::digest(&body));
            let canonical = [
                "gateway",
                "knowledge-service",
                "POST",
                path,
                "",
                &hash,
                "",
                &id,
                &timestamp,
                &self.key_id,
            ]
            .join("\n");
            let mut mac = Hmac::<Sha256>::new_from_slice(secret)
                .map_err(|_| CompatError::new(503, "KB signer unavailable"))?;
            mac.update(canonical.as_bytes());
            request = request
                .header(
                    "x-gateway-secret",
                    format!(
                        "v2:{}:{id}:{timestamp}:{hash}:{}",
                        self.key_id,
                        hex(&mac.finalize().into_bytes())
                    ),
                )
                .header("x-request-id", id);
        }
        let response = request.body(body).send().await?;
        if !response.status().is_success() {
            return Err(CompatError::new(
                502,
                format!(
                    "knowledge-service RAGAS eval failed with HTTP {}",
                    response.status().as_u16()
                ),
            ));
        }
        normalize_ragas_results(&bounded_json(response, 4 * 1024 * 1024).await?)
    }
}
pub async fn bounded_json(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Value, CompatError> {
    let mut body = vec![];
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > max {
            return Err(CompatError::new(502, "Eval service response exceeds limit"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| CompatError::new(502, "Eval service returned invalid JSON"))
}
