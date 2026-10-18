//! Trusted window boundary is checked both by Tauri ACL and on every command.
use url::Url;
pub fn gateway(raw: &str) -> Result<Url, &'static str> {
    ai_platform_client::Client::new(raw).map_err(|_| "invalid Gateway origin")?;
    let url = Url::parse(raw).map_err(|_| "invalid Gateway origin")?;
    // Candidates also require TLS: loopback HTTP allowed by the SDK is not an
    // exception to the desktop origin/cookie policy.
    if url.scheme() != "https" {
        return Err("HTTPS Gateway required");
    }
    Ok(url)
}
pub fn trusted_companion(label: &str, url: &Url) -> bool {
    label == "companion"
        && url.path() == "/native/index.html"
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (url.scheme() == "http"
                && url.host_str() == Some("tauri.localhost")
                && url.port().is_none()))
}
pub fn console_navigation(origin: &Url, url: &Url) -> bool {
    origin.origin() == url.origin() && url.username().is_empty() && url.password().is_none()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_and_other_windows_never_gain_native_authority() {
        for value in [
            "https://tauri.localhost/native/index.html",
            "https://gateway.test/native/index.html",
            "tauri://localhost/native/index.html?x=1",
            "tauri://localhost/console",
            "http://tauri.localhost:8080/native/index.html",
            "tauri://localhost:123/native/index.html",
        ] {
            assert!(!trusted_companion("companion", &Url::parse(value).unwrap()));
        }
        let asset = Url::parse("tauri://localhost/native/index.html").unwrap();
        assert!(trusted_companion("companion", &asset));
        assert!(!trusted_companion("console", &asset));
        assert!(!trusted_companion("console-offline", &asset));
    }
    #[test]
    fn gateway_and_navigation_are_exact_origins() {
        for raw in [
            "http://127.0.0.1",
            "https://user@gateway.test",
            "https://gateway.test/api",
            "https://gateway.test/?token=x",
        ] {
            assert!(gateway(raw).is_err());
        }
        let origin = gateway("https://gateway.test:9443").unwrap();
        assert!(console_navigation(
            &origin,
            &origin.join("/api/auth/device").unwrap()
        ));
        assert!(!console_navigation(
            &origin,
            &Url::parse("https://gateway.test/").unwrap()
        ));
        assert!(!console_navigation(
            &origin,
            &Url::parse("https://gateway.test.evil/").unwrap()
        ));
    }
}
