pub fn redact_preview(value: &str, limit: usize) -> String {
    let bearer = regex::Regex::new(r"(?i)\bauthorization\s*[:=]\s*bearer\s+[A-Za-z0-9._~+/=-]+")
        .expect("static regex");
    let secret=regex::Regex::new(r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)\s*[:=]\s*["']?[^"'\s,;}]+"#).expect("static regex");
    let first = bearer.replace_all(value, "Authorization: Bearer [redacted]");
    let value = secret.replace_all(&first, "${1}=[redacted]");
    if value.chars().count() <= limit {
        value.into_owned()
    } else {
        format!(
            "{}...[truncated]",
            value.chars().take(limit).collect::<String>()
        )
    }
}
