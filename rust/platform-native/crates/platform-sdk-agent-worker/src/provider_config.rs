//! Operator-owned provider endpoint configuration. Request bodies never select a destination.
use crate::contract::Result;

pub fn optional_base_url(name: &'static str) -> Result<Option<String>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| "sdk_provider_origin_invalid")?;
    if value.is_empty() {
        return Ok(None);
    }
    validate_base_url(&value)?;
    Ok(Some(value))
}

pub fn validate_base_url(value: &str) -> Result<()> {
    if value.len() > 2048
        || value.trim() != value
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err("sdk_provider_origin_invalid");
    }
    let url = reqwest::Url::parse(value).map_err(|_| "sdk_provider_origin_invalid")?;
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err("sdk_provider_origin_invalid");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_base_urls_allow_https_and_loopback_http() {
        assert!(validate_base_url("https://provider.example/compatible-mode/v1/").is_ok());
        assert!(validate_base_url("http://127.0.0.1:9000/v1").is_ok());
        assert!(validate_base_url("http://[::1]:9000/v1").is_ok());
    }

    #[test]
    fn operator_base_urls_reject_credentials_and_ambiguous_routes() {
        for value in [
            "http://provider.example/v1",
            "https://user:pass@provider.example/v1",
            "https://provider.example/v1?key=secret",
            "https://provider.example/v1#fragment",
            "file:///tmp/provider",
            "https://provider.example\\@127.0.0.1/v1",
        ] {
            assert_eq!(validate_base_url(value), Err("sdk_provider_origin_invalid"));
        }
    }
}
