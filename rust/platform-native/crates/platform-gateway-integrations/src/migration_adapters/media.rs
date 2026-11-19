use super::*;
use base64::{
    Engine, alphabet,
    engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig},
};
use reqwest::{Url, header::HeaderMap};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
pub(super) struct MediaAdapter {
    pub http: HttpTransport,
    pub service: ServiceHandle,
    pub kind: Kind,
}
fn decode_base64(text: &str) -> Result<Vec<u8>, AdapterError> {
    // Python's b64decode(validate=False) ignores nonalphabet characters.
    if !text.is_ascii() {
        return Err(validation("invalid_base64_media"));
    }
    let filtered: Vec<u8> = text
        .bytes()
        .filter(|b| b.is_ascii_alphanumeric() || matches!(*b, b'+' | b'/' | b'='))
        .collect();
    GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    )
    .decode(filtered)
    .map_err(|_| validation("invalid_base64_media"))
}
impl MediaAdapter {
    async fn invoke_media(
        &self,
        request: &UnifiedRequest,
    ) -> Result<UnifiedResponse, AdapterError> {
        let params = request.parameters.clone().unwrap_or_default();
        let get = |key: &str, default: Value| params.get(key).cloned().unwrap_or(default);
        let output = match self.kind {
            Kind::Tts => {
                let text = first_text(request, "text input is required")?;
                let response=self.http.post_json("/v1/audio/speech",&json!({"model":get("model",json!("tts-1")),"input":text,"voice":get("voice",json!("alloy")),"response_format":get("format",json!("mp3"))}),HeaderMap::new()).await?;
                let (is_json, bytes) = response.body().await?;
                if is_json {
                    let response: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| validation("provider_response_json_invalid"))?;
                    let data = response
                        .get("data")
                        .filter(|v| !v.is_null())
                        .ok_or_else(|| validation("tts response missing audio"))?;
                    let mut item = content(ContentType::Audio, data.clone());
                    item.mime_type = Some("audio/mpeg".into());
                    item
                } else {
                    binary(ContentType::Audio, bytes, "audio/mpeg")
                }
            }
            Kind::StableDiffusion => {
                let prompt = first_text(request, "text prompt is required")?;
                let config = self.service.connector().snapshot();
                let endpoint = config
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .unwrap_or("/txt2img");
                let response=self.http.post_json(endpoint,&json!({"prompt":prompt,"negative_prompt":get("negative_prompt",json!("")),"width":get("width",json!(512)),"height":get("height",json!(512)),"steps":get("steps",json!(20)),"cfg_scale":get("cfg_scale",json!(7.0)),"seed":get("seed",json!(-1))}),HeaderMap::new()).await?.json().await?;
                let encoded = response
                    .get("images")
                    .and_then(|v| v.get(0))
                    .and_then(Value::as_str)
                    .ok_or_else(|| validation("stable diffusion response missing images"))?;
                let mut item = binary(ContentType::Image, decode_base64(encoded)?, "image/png");
                item.metadata = Some(Map::from_iter([(
                    "seed".into(),
                    response.get("seed").cloned().unwrap_or(Value::Null),
                )]));
                let mut result = UnifiedResponse::new(&request.request_id, "success", vec![item]);
                result.usage = Some(Map::from_iter([(
                    "inference_time".into(),
                    response
                        .get("inference_time")
                        .cloned()
                        .unwrap_or(Value::Null),
                )]));
                return Ok(result);
            }
            Kind::Whisper => {
                let item = request
                    .inputs
                    .iter()
                    .find(|i| i.content_type == ContentType::Audio)
                    .ok_or_else(|| validation("audio input is required"))?;
                let bytes = match &item.data {
                    Some(ContentData::Bytes(bytes)) => bytes.clone(),
                    Some(ContentData::Text(text)) if !text.is_empty() => {
                        decode_base64(text).unwrap_or_else(|_| text.as_bytes().to_vec())
                    }
                    _ => {
                        if let Some(url) = item.url.as_deref().filter(|v| !v.is_empty()) {
                            safe_fetch(url, 50 * 1024 * 1024).await?
                        } else {
                            return Err(validation("audio data or url is required"));
                        }
                    }
                };
                let boundary = format!("gateway-{}", uuid::Uuid::new_v4().simple());
                let mut body = Vec::new();
                for (name, value) in [
                    ("model", get("model", json!("whisper-1"))),
                    ("language", get("language", Value::Null)),
                    ("response_format", get("format", json!("json"))),
                ] {
                    let value = if value.is_null() {
                        String::new()
                    } else if let Value::Bool(b) = value {
                        if b { "true".into() } else { "false".into() }
                    } else {
                        pystr(&value)
                    };
                    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
                }
                body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
                body.extend(bytes);
                body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
                let response = self
                    .http
                    .send(
                        self.http
                            .client
                            .post(self.http.url("/v1/audio/transcriptions")?)
                            .header(
                                "content-type",
                                format!("multipart/form-data; boundary={boundary}"),
                            )
                            .body(body),
                    )
                    .await?;
                let status = response.response.status().as_u16();
                let (is_json, bytes) = response.body().await?;
                let text = if is_json {
                    let response: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| validation("provider_response_json_invalid"))?;
                    response
                        .get("text")
                        .filter(|v| !v.is_null())
                        .cloned()
                        .unwrap_or_else(|| json!(pystr(&response)))
                } else {
                    json!(format!(
                        "<Response [{status} {}]>",
                        reqwest::StatusCode::from_u16(status)
                            .ok()
                            .and_then(|s| s.canonical_reason())
                            .unwrap_or("")
                    ))
                };
                content(ContentType::Text, text)
            }
            _ => return Err(AdapterError::UnsupportedOperation),
        };
        Ok(UnifiedResponse::new(
            &request.request_id,
            "success",
            vec![output],
        ))
    }
}
impl ProtocolAdapter for MediaAdapter {
    fn invoke<'a>(&'a self, request: &'a UnifiedRequest) -> AdapterFuture<'a, UnifiedResponse> {
        Box::pin(async move { self.invoke_media(request).await })
    }
    fn health_check(&self) -> AdapterFuture<'_, bool> {
        Box::pin(async { Ok(self.http.health().await) })
    }
}
fn unsafe_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let n = u32::from(v);
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_multicast()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_documentation()
                || n >> 24 == 0
                || n >> 28 == 15
                || (n & 0xffffff00) == 0xc0000000
                || (n & 0xffffff00) == 0xc0586300
                || (n & 0xfffe0000) == 0xc6120000
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return unsafe_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xe000) != 0x2000
                || (s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                || s[0] == 0x2002
        }
    }
}
pub async fn validated_destination(url: &str) -> Result<(Url, Vec<SocketAddr>), AdapterError> {
    let url = Url::parse(url).map_err(|_| validation("URL must be http(s)"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(validation("URL must be http(s)"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| validation("URL missing host"))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url
        .port_or_known_default()
        .ok_or_else(|| validation("URL missing port"))?;
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| validation("destination DNS resolution failed"))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|addr| unsafe_ip(addr.ip())) {
        return Err(validation("destination rejected"));
    }
    Ok((url, addresses))
}
pub(super) async fn safe_fetch(raw: &str, max_bytes: usize) -> Result<Vec<u8>, AdapterError> {
    safe_fetch_with_policy(raw, max_bytes, 3, Duration::from_secs(30)).await
}
/// Fetch a user-controlled URL with DNS pinning and independent validation at
/// every redirect. Never uses service credentials or environment HTTP proxies.
/// The timeout applies to connection establishment and each response read, as
/// the canonical httpx timeout does, rather than imposing a whole-body deadline.
pub async fn safe_fetch_with_policy(
    raw: &str,
    max_bytes: usize,
    max_redirects: usize,
    timeout: Duration,
) -> Result<Vec<u8>, AdapterError> {
    if max_bytes == 0 {
        return Err(validation("max_bytes must be positive"));
    }
    let mut current = raw.to_owned();
    let mut seen = HashSet::new();
    for _ in 0..=max_redirects {
        if !seen.insert(current.clone()) {
            return Err(validation("redirect loop detected"));
        }
        let (url, addresses) = validated_destination(&current).await?;
        let host = url
            .host_str()
            .ok_or_else(|| validation("URL missing host"))?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .resolve_to_addrs(host, &addresses[..1])
            .build()
            .map_err(|_| validation("safe fetch client failed"))?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| validation("failed to fetch URL"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| validation("redirect without Location header"))?;
            current = url
                .join(location)
                .map_err(|_| validation("invalid redirect URL"))?
                .to_string();
            continue;
        }
        if !response.status().is_success() {
            return Err(validation("failed to fetch URL"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| validation("failed to fetch URL"))?
        {
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(DispatchError::AdapterValidationDetail(format!(
                    "payload exceeds {} MB cap",
                    max_bytes / (1024 * 1024)
                ))
                .into());
            }
            bytes.extend(chunk);
        }
        return Ok(bytes);
    }
    Err(
        DispatchError::AdapterValidationDetail(format!("exceeded {max_redirects} redirects"))
            .into(),
    )
}
