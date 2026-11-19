//! Dedicated internal transport. The private reqwest client is never returned to
//! public web-fetch/provider code; every call checks an exact HTTPS origin + SPIFFE peer.
use super::{Error, Result, ServiceIdentity, tls::ClientCredentials};
use reqwest::{Method, Response, Url, header::HeaderMap};
use std::{collections::BTreeMap, time::Duration};
#[derive(Clone)]
pub struct WorkloadHttp {
    credentials: ClientCredentials,
    peers: BTreeMap<String, ServiceIdentity>,
    connect: Duration,
    total: Duration,
}
fn origin(raw: &str) -> Result<(String, Url)> {
    let url = Url::parse(raw).map_err(|_| Error("workload_origin_invalid"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || raw.trim() != raw
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return Err(Error("workload_origin_invalid"));
    }
    Ok((url.origin().ascii_serialization(), url))
}
impl WorkloadHttp {
    pub fn new(
        credentials: ClientCredentials,
        peers: impl IntoIterator<Item = (String, ServiceIdentity)>,
        connect: Duration,
        total: Duration,
    ) -> Result<Self> {
        if connect.is_zero() || total < connect || total > Duration::from_secs(3600) {
            return Err(Error("workload_timeout_invalid"));
        }
        let mut admitted = BTreeMap::new();
        for (raw, id) in peers {
            let (key, _) = origin(&raw)?;
            if admitted.insert(key, id).is_some() {
                return Err(Error("workload_duplicate_origin"));
            }
        }
        if admitted.is_empty() || admitted.len() > 64 {
            return Err(Error("workload_origins_invalid"));
        }
        Ok(Self {
            credentials,
            peers: admitted,
            connect,
            total,
        })
    }
    pub fn validate_endpoint(&self, raw: &str) -> Result<Url> {
        let url = Url::parse(raw).map_err(|_| Error("workload_endpoint_invalid"))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || raw.contains('\\')
            || raw.chars().any(char::is_control)
            || !self.peers.contains_key(&url.origin().ascii_serialization())
        {
            return Err(Error("workload_endpoint_not_admitted"));
        }
        Ok(url)
    }
    /// Business signatures and tenant/user proofs remain caller-owned headers.
    /// No redirects, proxy discovery, retry or public trust-store fallback exists.
    pub async fn send(
        &self,
        method: Method,
        endpoint: &str,
        headers: HeaderMap,
        body: Option<Vec<u8>>,
    ) -> Result<Response> {
        let url = self.validate_endpoint(endpoint)?;
        let identity = self
            .peers
            .get(&url.origin().ascii_serialization())
            .ok_or(Error("workload_endpoint_not_admitted"))?
            .clone();
        let tls = self.credentials.load(identity)?;
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .connect_timeout(self.connect)
            .timeout(self.total)
            .build()
            .map_err(|_| Error("workload_http_client_invalid"))?;
        let mut request = client.request(method, url).headers(headers);
        if let Some(body) = body {
            request = request.body(body)
        }
        request
            .send()
            .await
            .map_err(|_| Error("workload_http_request_failed"))
    }
}
#[cfg(test)]
mod tests {
    use super::super::secrets::CredentialFiles;
    use super::*;
    #[test]
    fn credential_transport_rejects_public_url_credentials_and_authority_changes() {
        let id =
            ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/gateway").unwrap();
        let credentials = ClientCredentials::new(
            CredentialFiles::new("/synthetic/unread".into()).unwrap(),
            id.clone(),
        );
        let transport = WorkloadHttp::new(
            credentials,
            [("https://gateway-internal:8443".into(), id)],
            Duration::from_secs(1),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(
            transport
                .validate_endpoint("https://gateway-internal:8443/internal/signed?revision=1")
                .is_ok()
        );
        for endpoint in [
            "http://gateway-internal:8443/internal",
            "https://gateway-internal:8443.evil.example/internal",
            "https://public.example/internal",
            "https://user:secret@gateway-internal:8443/internal",
            "https://gateway-internal:8443/internal#secret",
        ] {
            assert!(transport.validate_endpoint(endpoint).is_err());
        }
    }
}
