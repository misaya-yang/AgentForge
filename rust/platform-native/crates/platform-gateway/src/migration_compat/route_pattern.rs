//! Source route-pattern contract used for per-upstream endpoint breaker keys.
//! Pure canonicalization avoids the source's unbounded concrete-path memo cache.
use regex::Regex;
use std::sync::OnceLock;
const UUID: &str = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
fn patterns() -> &'static Vec<(Regex, String)> {
    static P: OnceLock<Vec<(Regex, String)>> = OnceLock::new();
    P.get_or_init(|| {
        let mut p = Vec::new();
        for tail in ["health/live", "health/ready", "health"] {
            p.push((
                format!(r"^(?:/api/v\d+/[^/]+)?/{tail}$"),
                format!("/{tail}"),
            ));
        }
        for tail in ["chat/stream", "chat"] {
            p.push((
                format!(r"^(?:/api/v\d+/assistant)?/{tail}$"),
                format!("/{tail}"),
            ));
        }
        for tail in ["/messages", "/history", ""] {
            p.push((
                format!(r"^(?:/api/v\d+/assistant)?/sessions/{UUID}{tail}$"),
                format!("/sessions/{{id}}{tail}"),
            ));
        }
        p.push((
            r"^(?:/api/v\d+/assistant)?/sessions$".into(),
            "/sessions".into(),
        ));
        for tail in ["/retrieve", "/documents", ""] {
            p.push((
                format!(r"^/api/v\d+/knowledge/datasets/{UUID}{tail}$"),
                format!("/api/v1/knowledge/datasets/{{id}}{tail}"),
            ));
        }
        p.push((
            r"^/api/v\d+/knowledge/datasets$".into(),
            "/api/v1/knowledge/datasets".into(),
        ));
        p.into_iter()
            .map(|(pattern, target)| (Regex::new(&pattern).expect("static route regex"), target))
            .collect()
    })
}
pub fn extract_route_pattern(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    for (pattern, target) in patterns() {
        if pattern.is_match(path) {
            return target.clone();
        }
    }
    static U: OnceLock<Regex> = OnceLock::new();
    static I: OnceLock<Regex> = OnceLock::new();
    let uuid = U.get_or_init(|| Regex::new(&format!("^{UUID}$")).expect("UUID regex"));
    let integer = I.get_or_init(|| Regex::new(r"^\d+$").expect("integer regex"));
    path.split('/')
        .enumerate()
        .map(|(index, segment)| {
            if index > 0 && uuid.is_match(segment) {
                "{uuid}"
            } else if index > 0 && integer.is_match(segment) {
                "{id}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}
