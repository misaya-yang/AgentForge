//! Deterministic Nginx header fixture, isolated from the production Gateway.
use crate::*;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
const BODY: &str = "<!doctype html><title>Embed fixture</title>";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; frame-ancestors https://allowed.example; object-src 'none'";
fn response(method: &str, path: &str) -> String {
    if method != "GET" {
        return "HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .into();
    }
    if !path.starts_with("/embed/agents/") {
        return "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into();
    }
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: {CSP}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
        BODY.len()
    )
}
fn handle(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = vec![];
    let mut part = [0u8; 1024];
    while bytes.len() <= 16384 {
        let n = stream.read(&mut part)?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&part[..n]);
        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            let line = std::str::from_utf8(&bytes)?
                .lines()
                .next()
                .ok_or("request line missing")?;
            let fields: Vec<_> = line.split_whitespace().collect();
            require(fields.len() == 3, "invalid request line")?;
            stream.write_all(response(fields[0], fields[1]).as_bytes())?;
            return Ok(());
        }
    }
    Err("request too large".into())
}
pub fn serve(args: &Args) -> Result<Value> {
    let bind = args
        .values
        .get("--bind")
        .map(String::as_str)
        .unwrap_or("0.0.0.0:8080");
    let listener = TcpListener::bind(bind)?;
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if active.fetch_add(1, Ordering::SeqCst) >= 32 {
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let active = active.clone();
        std::thread::spawn(move || {
            let _ = handle(stream);
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Err("fixture listener stopped".into())
}
fn location<'a>(source: &'a str, marker: &str) -> Result<&'a str> {
    let start = source.find(marker).ok_or("embed location missing")?;
    let brace = start + source[start..].find('{').ok_or("location brace missing")?;
    let mut depth = 0usize;
    for (index, c) in source[brace..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&source[start..brace + index + 1]);
                }
            }
            _ => {}
        }
    }
    Err("unterminated location".into())
}
pub fn check(root: &Path) -> Result<Value> {
    for file in [
        "web/nginx.conf",
        "deploy/helm/ai-gateway/templates/frontend-configmap.yaml",
    ] {
        let text = std::fs::read_to_string(root.join(file))?;
        let block = location(&text, "location /embed/agents/")?;
        require(
            block.contains("proxy_pass")
                && !block.contains("X-Frame-Options")
                && !block.contains("Content-Security-Policy")
                && block.contains("add_header X-Content-Type-Options"),
            format!("{file}: invalid dynamic embed header inheritance"),
        )?;
        require(
            text.contains("add_header X-Frame-Options \"SAMEORIGIN\" always;")
                && text.contains("frame-ancestors 'self'"),
            "hosted framing defaults missing",
        )?;
    }
    let ingress =
        std::fs::read_to_string(root.join("deploy/helm/ai-gateway/templates/ingress.yaml"))?;
    require(
        ingress.contains("- path: /embed/agents") && ingress.contains("-gateway"),
        "embed ingress route missing",
    )?;
    let routes = std::fs::read_to_string(
        root.join("rust/platform-native/crates/platform-gateway/src/migration_agent/routes.rs"),
    )?;
    require(
        routes.contains("/embed/agents/{public_id}"),
        "native embed route missing",
    )?;
    let public = std::fs::read_to_string(
        root.join("rust/platform-native/crates/platform-gateway/src/migration_agent/public.rs"),
    )?;
    require(
        public.contains("content-security-policy")
            && public.contains("frame-ancestors {}")
            && public.contains("allowed.join"),
        "native dynamic framing CSP missing",
    )?;
    let middleware = std::fs::read_to_string(
        root.join("rust/platform-native/crates/platform-gateway/src/migration_host/middleware.rs"),
    )?;
    require(
        middleware.contains("starts_with(\"/embed/agents/\")")
            && middleware.contains("if !embed")
            && middleware.contains("x-frame-options"),
        "native embed framing exemption missing",
    )?;
    Ok(json!({"result":"pass","gate":"agent-embed-headers","mode":"config-only"}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_response_contract() {
        let r = response("GET", "/embed/agents/test");
        assert!(r.contains("200 OK"));
        assert!(r.contains("frame-ancestors https://allowed.example"));
        assert!(r.contains("Cache-Control: no-store"));
        assert!(!r.contains("X-Frame-Options"));
        assert!(response("GET", "/other").contains("404"));
        // The historical BaseHTTPRequestHandler defined only do_GET.
        assert!(response("HEAD", "/embed/agents/test").contains("501 Not Implemented"));
        assert!(!response("HEAD", "/embed/agents/test").contains(BODY));
    }
    #[test]
    fn nested_locations() {
        assert_eq!(
            location("location /x { nested { a; } b; } trailing", "location /x").unwrap(),
            "location /x { nested { a; } b; }"
        );
    }
}
