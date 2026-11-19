use super::*;
use futures_util::{Stream, StreamExt};
pub(super) struct Upstream {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Pin<Box<dyn Stream<Item = LlmResult<Bytes>> + Send>>,
}
pub(super) trait Transport: Send + Sync {
    fn send<'a>(&'a self, prepared: &'a Prepared) -> LlmFuture<'a, Upstream>;
}
pub(super) struct ReqwestTransport(reqwest::Client);
impl ReqwestTransport {
    pub fn new() -> LlmResult<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()
                .map_err(|_| LlmError::new(503, "llm_http_configuration_invalid"))?,
        ))
    }
}
impl Transport for ReqwestTransport {
    fn send<'a>(&'a self, prepared: &'a Prepared) -> LlmFuture<'a, Upstream> {
        Box::pin(async move {
            let response = self
                .0
                .post(prepared.url.clone())
                .headers(prepared.headers.clone())
                .timeout(prepared.timeout)
                .body(prepared.body.clone())
                .send()
                .await
                .map_err(|_| LlmError::new(502, "llm_upstream_outcome_unknown"))?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            Ok(Upstream {
                status,
                headers,
                body: Box::pin(
                    response
                        .bytes_stream()
                        .map(|r| r.map_err(|_| LlmError::new(502, "llm_upstream_outcome_unknown"))),
                ),
            })
        })
    }
}
pub(super) fn response_headers(headers: &HeaderMap) -> HeaderMap {
    let connection = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "connection"
                    | "keep-alive"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "te"
                    | "trailer"
                    | "transfer-encoding"
                    | "upgrade"
                    | "content-length"
                    | "set-cookie"
                    | "authorization"
                    | "x-api-key"
                    | "x-route-decision"
            ) && !connection.iter().any(|v| v == name.as_str())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
