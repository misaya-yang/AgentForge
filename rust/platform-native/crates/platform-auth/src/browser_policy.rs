//! Pure same-origin and synchronizer-token policy for browser credentials.
// SPDX-License-Identifier: Apache-2.0
use crate::service::ServiceError;
use axum::http::{HeaderMap, Method, Uri};
use sha2::{Digest, Sha256};
pub const COOKIE_NAME: &str = "__Host-agentforge_session";
pub const CSRF_HEADER: &str = "x-csrf-token";
#[derive(Clone)]
pub struct BrowserPolicy {
    origins: std::collections::BTreeSet<String>,
}
fn unique<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ServiceError> {
    if headers.get_all(name).iter().count() > 1 {
        return Err(ServiceError::Credentials);
    }
    headers
        .get(name)
        .map(|v| v.to_str().map_err(|_| ServiceError::Credentials))
        .transpose()
}
impl BrowserPolicy {
    pub fn new(origins: Vec<String>) -> Result<Self, ServiceError> {
        if origins.is_empty() || origins.len() > 16 {
            return Err(ServiceError::Invalid);
        }
        let mut normalized = std::collections::BTreeSet::new();
        for origin in origins {
            let uri: Uri = origin.parse().map_err(|_| ServiceError::Invalid)?;
            let host = uri.host().ok_or(ServiceError::Invalid)?;
            if host.contains('*') {
                return Err(ServiceError::Invalid);
            }
            if uri.scheme_str() != Some("https")
                || uri.authority().is_none()
                || uri.authority().is_some_and(|a| a.as_str().contains('@'))
                || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
                || origin.ends_with('/')
                || !normalized.insert(origin)
            {
                return Err(ServiceError::Invalid);
            }
        }
        Ok(Self {
            origins: normalized,
        })
    }
    pub(crate) fn origins(&self) -> Vec<String> {
        self.origins.iter().cloned().collect()
    }
    pub fn check_origin(&self, headers: &HeaderMap, required: bool) -> Result<(), ServiceError> {
        match unique(headers, "sec-fetch-site")? {
            Some("same-origin" | "same-site") => {}
            _ => return Err(ServiceError::Credentials),
        }
        match unique(headers, "origin")? {
            Some(origin) if self.origins.contains(origin) => Ok(()),
            None if !required => Ok(()),
            _ => Err(ServiceError::Credentials),
        }
    }
    pub fn check(
        &self,
        method: &Method,
        headers: &HeaderMap,
        csrf: &str,
    ) -> Result<(), ServiceError> {
        let unsafe_method = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
        let websocket = headers
            .get("upgrade")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
        self.check_origin(headers, unsafe_method || websocket)?;
        if unsafe_method {
            let supplied = unique(headers, CSRF_HEADER)?.ok_or(ServiceError::Credentials)?;
            let a = Sha256::digest(supplied.as_bytes());
            let b = Sha256::digest(csrf.as_bytes());
            if supplied.len() != 64
                || a.iter().zip(b.iter()).fold(0u8, |v, (a, b)| v | (a ^ b)) != 0
            {
                return Err(ServiceError::Credentials);
            }
        }
        Ok(())
    }
}
pub fn session_cookie(headers: &HeaderMap) -> Result<Option<String>, ServiceError> {
    let mut found = None;
    for cookie in headers.get_all("cookie") {
        for part in cookie
            .to_str()
            .map_err(|_| ServiceError::Credentials)?
            .split(';')
        {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            if key == COOKIE_NAME {
                if found.is_some()
                    || value.len() != 64
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(ServiceError::Credentials);
                }
                found = Some(value.to_owned());
            }
        }
    }
    Ok(found)
}
pub fn cookie(value: &str, ttl: u64) -> String {
    format!("{COOKIE_NAME}={value}; Path=/; Max-Age={ttl}; HttpOnly; Secure; SameSite=Lax")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csrf_origin_fetch_metadata_and_duplicate_cookie_negatives() {
        let p = BrowserPolicy::new(vec!["https://console.example".into()]).unwrap();
        let token = "a".repeat(64);
        let mut h = HeaderMap::new();
        h.insert("origin", "https://console.example".parse().unwrap());
        h.insert("sec-fetch-site", "same-origin".parse().unwrap());
        h.insert(CSRF_HEADER, token.parse().unwrap());
        assert!(p.check(&Method::POST, &h, &token).is_ok());
        h.remove(CSRF_HEADER);
        assert!(p.check(&Method::POST, &h, &token).is_err());
        assert!(p.check(&Method::GET, &h, &token).is_ok());
        h.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(p.check(&Method::GET, &h, &token).is_err());
        h.insert("sec-fetch-site", "same-origin".parse().unwrap());
        h.insert("origin", "https://console.example.evil".parse().unwrap());
        assert!(p.check(&Method::POST, &h, &token).is_err());
        h.insert(
            "cookie",
            format!("{COOKIE_NAME}={token}; {COOKIE_NAME}={token}")
                .parse()
                .unwrap(),
        );
        assert!(session_cookie(&h).is_err());
        let set = cookie(&token, 300);
        for attr in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
            assert!(set.contains(attr));
        }
        assert!(!set.contains("Domain="));
    }
    #[test]
    fn websocket_requires_allowed_origin_and_no_wildcards() {
        assert!(BrowserPolicy::new(vec!["https://*.example".into()]).is_err());
        assert!(BrowserPolicy::new(vec!["http://localhost:8080".into()]).is_err());
        let p = BrowserPolicy::new(vec!["https://console.example".into()]).unwrap();
        let mut h = HeaderMap::new();
        h.insert("sec-fetch-site", "same-origin".parse().unwrap());
        h.insert("upgrade", "websocket".parse().unwrap());
        assert!(p.check(&Method::GET, &h, "").is_err());
    }
}
