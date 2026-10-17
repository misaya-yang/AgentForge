//! RFC9728 protected-resource metadata. No OAuth token exchange or issuer fetch.
use reqwest::{Client, Url};
use serde_json::Value;
use std::time::Duration;
fn https(value: &str) -> Result<Url, &'static str> {
    let url = Url::parse(value).map_err(|_| "MCP_OAUTH_METADATA_INVALID")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err("MCP_OAUTH_METADATA_INVALID");
    }
    Ok(url)
}
pub fn resource_indicator(resource: &str) -> Result<(String, String), &'static str> {
    https(resource)?;
    Ok(("resource".into(), resource.into()))
}
pub fn validate(document: &Value, resource: &str) -> Result<(), &'static str> {
    resource_indicator(resource)?;
    if document["resource"].as_str() != Some(resource) {
        return Err("MCP_OAUTH_RESOURCE_MISMATCH");
    }
    let issuers = document["authorization_servers"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 8)
        .ok_or("MCP_OAUTH_METADATA_INVALID")?;
    for issuer in issuers {
        https(issuer.as_str().ok_or("MCP_OAUTH_METADATA_INVALID")?)?;
    }
    if let Some(methods) = document.get("bearer_methods_supported") {
        let methods = methods.as_array().ok_or("MCP_OAUTH_METADATA_INVALID")?;
        if !methods.iter().any(|m| m == "header") {
            return Err("MCP_OAUTH_TOKEN_METHOD_UNSUPPORTED");
        }
    }
    Ok(())
}
/// Only same-origin metadata is fetched by this limited broker, retaining the
/// exact existing DNS pin. Cross-origin metadata requires a separately admitted
/// resolver/pin and fails closed here. No credential is sent on discovery GETs.
pub fn candidates(endpoint: &Url, explicit: Option<&str>) -> Result<Vec<Url>, &'static str> {
    if let Some(explicit) = explicit {
        let url = https(explicit)?;
        if url.origin() != endpoint.origin() {
            return Err("MCP_OAUTH_METADATA_ORIGIN_DENIED");
        }
        return Ok(vec![url]);
    }
    let mut path = endpoint.clone();
    path.set_query(None);
    path.set_fragment(None);
    path.set_path(&format!(
        "/.well-known/oauth-protected-resource{}",
        endpoint.path().trim_end_matches('/')
    ));
    let mut root = endpoint.clone();
    root.set_query(None);
    root.set_fragment(None);
    root.set_path("/.well-known/oauth-protected-resource");
    if path == root {
        Ok(vec![root])
    } else {
        Ok(vec![path, root])
    }
}
pub fn challenge_url(header: &str) -> Result<Option<String>, &'static str> {
    if header.len() > 4096 || header.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("MCP_OAUTH_CHALLENGE_INVALID");
    }
    let Some((scheme, rest)) = header.trim_start().split_once(' ') else {
        return Ok(None);
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Ok(None);
    }
    let mut found = None;
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in rest.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quoted = !quoted
        } else if byte == b',' && !quoted {
            parts.push(&rest[start..index]);
            start = index + 1;
        }
    }
    if quoted || escaped {
        return Err("MCP_OAUTH_CHALLENGE_INVALID");
    }
    parts.push(&rest[start..]);
    for part in parts {
        let Some((name, value)) = part.trim().split_once('=') else {
            return Err("MCP_OAUTH_CHALLENGE_INVALID");
        };
        if name.eq_ignore_ascii_case("resource_metadata") {
            if found.is_some()
                || !value.starts_with('"')
                || !value.ends_with('"')
                || value.len() < 3
            {
                return Err("MCP_OAUTH_CHALLENGE_INVALID");
            }
            let value = &value[1..value.len() - 1];
            if value.contains(['"', '\\']) {
                return Err("MCP_OAUTH_CHALLENGE_INVALID");
            }
            https(value)?;
            found = Some(value.into());
        }
    }
    Ok(found)
}
pub async fn fetch(
    client: &Client,
    endpoint: &Url,
    explicit: Option<&str>,
    resource: &str,
    timeout: Duration,
) -> Result<Value, &'static str> {
    let timeout = timeout.min(Duration::from_secs(10));
    for url in candidates(endpoint, explicit)? {
        let result = tokio::time::timeout(timeout, async {
            let mut response = client
                .get(url)
                .header("accept", "application/json")
                .timeout(timeout)
                .send()
                .await
                .map_err(|_| "MCP_OAUTH_METADATA_UNAVAILABLE")?;
            if response.status().as_u16() == 404 {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err("MCP_OAUTH_METADATA_REJECTED");
            }
            let mut bytes = vec![];
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "MCP_OAUTH_METADATA_UNAVAILABLE")?
            {
                if chunk.len() > 131072usize.saturating_sub(bytes.len()) {
                    return Err("MCP_OAUTH_METADATA_INVALID");
                }
                bytes.extend_from_slice(&chunk);
            }
            let document =
                serde_json::from_slice(&bytes).map_err(|_| "MCP_OAUTH_METADATA_INVALID")?;
            validate(&document, resource)?;
            Ok(Some(document))
        })
        .await
        .map_err(|_| "MCP_OAUTH_METADATA_UNAVAILABLE")??;
        if let Some(document) = result {
            return Ok(document);
        }
    }
    Err("MCP_OAUTH_METADATA_UNAVAILABLE")
}
#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
