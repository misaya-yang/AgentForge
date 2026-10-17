//! Concrete fetch and image policy adapters. Raster codecs are explicit because
//! Pillow's decoding, Lanczos and Gaussian rounding are part of the source contract.
use super::images::{FetchedImage, ImageReferenceFetch};
use super::{ContentError, ContentFuture};
use ai_platform_service_primitives::dns_policy::{
    ConnectorDnsResolver, NativeConnectorDnsResolver, oauth_form_safe_address,
};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

pub struct PinnedImageFetch {
    pub resolver: Arc<dyn ConnectorDnsResolver>,
    pub allowed_hosts: Option<HashSet<String>>,
    pub max_redirects: usize,
}
impl Default for PinnedImageFetch {
    fn default() -> Self {
        Self {
            resolver: Arc::new(NativeConnectorDnsResolver),
            allowed_hosts: None,
            max_redirects: 3,
        }
    }
}
impl ImageReferenceFetch for PinnedImageFetch {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        max_bytes: usize,
        timeout: Duration,
    ) -> ContentFuture<'a, FetchedImage> {
        Box::pin(async move {
            let invalid = || ContentError::new(422, "invalid_image_url");
            if max_bytes == 0 {
                return Err(invalid());
            }
            let mut current = reqwest::Url::parse(url).map_err(|_| invalid())?;
            let mut seen = HashSet::new();
            for hop in 0..=self.max_redirects {
                if !matches!(current.scheme(), "http" | "https")
                    || !seen.insert(current.as_str().to_owned())
                {
                    return Err(invalid());
                }
                let host = current
                    .host_str()
                    .ok_or_else(invalid)?
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if self.allowed_hosts.as_ref().is_some_and(|hosts| {
                    !hosts.iter().any(|suffix| {
                        let suffix = suffix.to_lowercase();
                        let suffix = suffix.trim_start_matches('.');
                        host == suffix || host.ends_with(&format!(".{suffix}"))
                    })
                }) {
                    return Err(invalid());
                }
                let port = current.port_or_known_default().ok_or_else(invalid)?;
                let addresses = if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
                    vec![ip]
                } else {
                    self.resolver
                        .resolve(&host, port)
                        .await
                        .map_err(|_| invalid())?
                };
                if addresses.is_empty() || addresses.iter().any(|a| !oauth_form_safe_address(*a)) {
                    return Err(invalid());
                }
                let address = addresses[0];
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .connect_timeout(timeout)
                    .read_timeout(timeout)
                    .no_proxy()
                    .resolve(&host, SocketAddr::new(address, port))
                    .build()
                    .map_err(|_| invalid())?;
                // Keep the logical URL host for TLS SNI and use source's original Host header.
                let mut response = client
                    .get(current.clone())
                    .header(reqwest::header::HOST, &host)
                    .send()
                    .await
                    .map_err(|_| invalid())?;
                if response.status().is_redirection() {
                    if hop == self.max_redirects {
                        return Err(invalid());
                    }
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(invalid)?;
                    current = current.join(location).map_err(|_| invalid())?;
                    continue;
                }
                if !response.status().is_success() {
                    return Err(invalid());
                }
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
                    if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                        return Err(ContentError::new(413, "image exceeds maximum size"));
                    }
                    bytes.extend_from_slice(&chunk)
                }
                return Ok(FetchedImage {
                    bytes,
                    content_type,
                    status,
                });
            }
            Err(invalid())
        })
    }
}
