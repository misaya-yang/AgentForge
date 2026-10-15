//! Identity-partitioned LangGraph wait-response cache over the existing semantic_cache table.
use super::{
    common::{hex, text},
    transparent_proxy::ProxyRequest,
};
use axum::{
    body::Bytes,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::BTreeMap;
pub struct ResponseCache {
    pub pool: PgPool,
    pub enabled: bool,
}
pub struct CachedProxyResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}
pub fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return "/".into();
    }
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    if path == "/" {
        path
    } else {
        path.trim_end_matches('/').into()
    }
}
fn stable(value: &Value) -> String {
    crate::migration_content::python_json(value, false, false)
}
impl ResponseCache {
    pub fn should_use_cache(request: &ProxyRequest, stream: bool) -> bool {
        let path = normalize_path(&request.path);
        let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
        !stream
            && request.config.cache_enabled
            && request.method.as_str() == "POST"
            && (path == "/runs/wait"
                || matches!(parts.as_slice(),["threads",id,"runs","wait"] if !id.is_empty()))
    }
    pub fn hash(request: &ProxyRequest, parsed: &Value) -> (String, Option<String>) {
        let value = if parsed.is_object() {
            Some(parsed.clone())
        } else {
            serde_json::from_slice::<Value>(&request.body).ok()
        };
        let (body, model) = if let Some(value) = value {
            let model = crate::usage_parser::extract_model(&value)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty());
            (stable(&value), model)
        } else if request.body.is_empty() {
            ("{}".into(), None)
        } else {
            (STANDARD.encode(&request.body), None)
        };
        let model = model.or_else(|| request.config.default_model.clone());
        let identity = request
            .api_key_id
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or(if request.user.user_id.is_empty() {
                "anonymous"
            } else {
                &request.user.user_id
            })
            .trim();
        let mut query = BTreeMap::new();
        for (key, value) in &request.query {
            query.insert(key.clone(), value.clone());
        }
        let payload = json!({"tenant_id":if request.user.tenant_id.is_empty(){"default"}else{&request.user.tenant_id},"identity":if identity.is_empty(){"anonymous"}else{identity},"service_id":request.config.service_id,"service_name":request.config.service_name,"assistant_id":request.config.assistant_id.as_deref().unwrap_or(""),"graph_id":request.config.graph_id.as_deref().unwrap_or(""),"model":model.as_deref().unwrap_or(""),"method":request.method.as_str(),"path":normalize_path(&request.path),"query":query,"body":body});
        (hex(&Sha256::digest(stable(&payload).as_bytes())), model)
    }
    pub async fn get(
        &self,
        request: &ProxyRequest,
        parsed: &Value,
        stream: bool,
    ) -> (&'static str, Option<String>, Option<CachedProxyResponse>) {
        if !self.enabled || !Self::should_use_cache(request, stream) {
            return ("BYPASS", None, None);
        }
        let (hash, _) = Self::hash(request, parsed);
        let row=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(c) FROM semantic_cache c WHERE service_id=$1 AND input_hash=$2 AND (expires_at IS NULL OR expires_at>NOW())").bind(&request.config.service_id).bind(&hash).fetch_optional(&self.pool).await;
        let row = match row {
            Err(_) => return ("BYPASS", Some(hash), None),
            Ok(None) => return ("MISS", Some(hash), None),
            Ok(Some(row)) => row,
        };
        if sqlx::query("UPDATE semantic_cache SET hit_count=hit_count+1,last_hit_at=NOW() WHERE service_id=$1 AND input_hash=$2").bind(&request.config.service_id).bind(&hash).execute(&self.pool).await.is_err(){return("BYPASS",Some(hash),None);}
        let output = &row["output_data"];
        if !output.is_object() || row["metadata"]["cache_schema"] != "proxy-response-v1" {
            return ("MISS", Some(hash), None);
        }
        let body = match STANDARD.decode(text(output, "body_base64")) {
            Ok(body) => Bytes::from(body),
            Err(_) => return ("MISS", Some(hash), None),
        };
        let mut headers = HeaderMap::new();
        if let Some(map) = output["headers"].as_object() {
            for (name, value) in map {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(value.as_str().unwrap_or("")),
                ) {
                    headers.insert(name, value);
                }
            }
        }
        let status = StatusCode::from_u16(
            output["status_code"]
                .as_u64()
                .filter(|v| *v > 0)
                .unwrap_or(200) as u16,
        )
        .unwrap_or(StatusCode::OK);
        (
            "HIT",
            Some(hash),
            Some(CachedProxyResponse {
                status,
                headers,
                body,
            }),
        )
    }
    pub async fn save(
        &self,
        request: &ProxyRequest,
        parsed: &Value,
        hash: Option<&str>,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
        stream: bool,
    ) {
        if !self.enabled
            || status.as_u16() >= 400
            || body.len() > 512 * 1024
            || !Self::should_use_cache(request, stream)
        {
            return;
        }
        let (calculated, model) = Self::hash(request, parsed);
        let hash = hash.unwrap_or(&calculated);
        let headers = headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str(), v)))
            .collect::<BTreeMap<_, _>>();
        let output = json!({"status_code":status.as_u16(),"headers":headers,"body_base64":STANDARD.encode(body)});
        let path = normalize_path(&request.path);
        let metadata = json!({"cache_schema":"proxy-response-v1","tenant_id":if request.user.tenant_id.is_empty(){"default"}else{&request.user.tenant_id},"user_id":if request.user.user_id.is_empty(){"anonymous"}else{&request.user.user_id},"api_key_id":request.api_key_id.as_deref().unwrap_or(""),"service_id":request.config.service_id,"path":path,"method":request.method.as_str(),"model":model.unwrap_or_default()});
        let input = format!("{} {path}", request.method);
        let output_text = String::from_utf8_lossy(body)
            .chars()
            .take(2000)
            .collect::<String>();
        let expires =
            chrono::Utc::now() + chrono::Duration::seconds(request.config.cache_ttl.max(1));
        let _=sqlx::query("INSERT INTO semantic_cache(service_id,input_hash,input_text,output_text,output_data,metadata,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(service_id,input_hash) DO UPDATE SET output_text=EXCLUDED.output_text,output_data=EXCLUDED.output_data,metadata=EXCLUDED.metadata,expires_at=EXCLUDED.expires_at,hit_count=semantic_cache.hit_count+1,last_hit_at=NOW()").bind(&request.config.service_id).bind(hash).bind(input).bind(output_text).bind(output).bind(metadata).bind(expires).execute(&self.pool).await;
    }
}
