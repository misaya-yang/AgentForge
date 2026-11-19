//! urllib-compatible display/fingerprint URL splitting. These helpers never
//! authorize or fetch a destination; network admission remains in safe_fetch.
#[derive(Debug)]
pub(super) struct Parts {
    pub scheme: String,
    pub authority: Option<String>,
    pub path: String,
    pub query: String,
}
pub(super) fn split(raw: &str) -> Option<Parts> {
    let raw: String = raw
        .trim_start_matches(|c: char| c.is_ascii() && c <= ' ')
        .chars()
        .filter(|c| !['\r', '\n', '\t'].contains(c))
        .collect();
    let (scheme, rest) = if let Some(i) = raw.find(':').filter(|i| *i > 0) {
        let candidate = &raw[..i];
        if candidate.as_bytes()[0].is_ascii_alphabetic()
            && candidate
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
        {
            (candidate.to_ascii_lowercase(), &raw[i + 1..])
        } else {
            (String::new(), raw.as_str())
        }
    } else {
        (String::new(), raw.as_str())
    };
    let (authority, rest) = if let Some(rest) = rest.strip_prefix("//") {
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        (Some(rest[..end].to_owned()), &rest[end..])
    } else {
        (None, rest)
    };
    if let Some(authority) = &authority {
        // Match urlsplit's bracket validation without WHATWG host/path rewriting.
        if authority.contains('[') || authority.contains(']') {
            let host = authority.rsplit('@').next()?;
            let end = host.find(']')?;
            let literal = host.strip_prefix('[')?.get(..end - 1)?;
            if literal.starts_with('v') {
                let (version, address) = literal[1..].split_once('.')?;
                if version.is_empty()
                    || !version.bytes().all(|c| c.is_ascii_hexdigit())
                    || address.is_empty()
                {
                    return None;
                }
            } else if literal
                .split('%')
                .next()?
                .parse::<std::net::Ipv6Addr>()
                .is_err()
            {
                return None;
            }
        }
    }
    let rest = rest.split_once('#').map_or(rest, |(head, _)| head);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Some(Parts {
        scheme,
        authority,
        path: path.into(),
        query: query.into(),
    })
}
pub(super) fn render(parts: &Parts, query: &str) -> String {
    let mut out = String::new();
    if !parts.scheme.is_empty() {
        out.push_str(&parts.scheme);
        out.push(':');
    }
    if let Some(authority) = &parts.authority {
        out.push_str("//");
        out.push_str(authority);
    }
    out.push_str(&parts.path);
    if !query.is_empty() {
        out.push('?');
        out.push_str(query);
    }
    out
}
fn unquote(raw: &str) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn quote(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(&byte) {
            out.push(byte as char);
        } else if byte == b' ' {
            out.push('+');
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}
pub(super) fn redact_query(query: &str) -> String {
    let sensitive = regex::Regex::new(
        r"(?i)(?:token|secret|password|passwd|credential|api[_-]?key|signature|sig|auth|code)",
    )
    .unwrap();
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            let key = unquote(key);
            let value = if sensitive.is_match(&key) {
                "***".into()
            } else {
                unquote(value)
            };
            format!("{}={}", quote(&key), quote(&value))
        })
        .collect::<Vec<_>>()
        .join("&")
}
pub(super) fn secret_free_base_url(raw: &str) -> Option<String> {
    let mut parts = split(raw.trim())?;
    if parts.scheme.is_empty() {
        return None;
    }
    let authority = parts.authority.as_deref()?.rsplit('@').next()?;
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        (host, rest.strip_prefix(':').filter(|p| !p.is_empty()))
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, (!port.is_empty()).then_some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    if port.is_some_and(|p| !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let port = port.map(str::parse::<u16>).transpose().ok()?;
    // urllib lowercases the hostname while retaining an IPv6 scope's case.
    let host = match host.split_once('%') {
        Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
        None => host.to_lowercase(),
    };
    let mut host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    if let Some(port) = port {
        host.push_str(&format!(":{port}"));
    }
    parts.authority = Some(host);
    Some(render(&parts, ""))
}
