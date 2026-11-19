//! Closed, redacted MCP management schemas from `src/api/schemas/mcp.py`.
//! Validation preserves patch omission versus explicit null. No submitted value
//! enters an error; URL spelling is preserved like urllib rather than rewritten
//! by a WHATWG URL serializer. DNS policy is a separate, mandatory boundary.
use crate::rate_config_api::{boolean, integer};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use std::net::{IpAddr, Ipv4Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpBodyKind {
    ServerCreate,
    ServerUpdate,
    ConnectionCreate,
    ChannelGrant,
    Discovery,
}

pub fn field_error(location: impl IntoIterator<Item = String>, kind: &str) -> Value {
    json!({"loc":location.into_iter().collect::<Vec<_>>(),"type":kind})
}
fn at(field: &str) -> Vec<String> {
    vec!["body".into(), field.into()]
}
fn string(value: &Value, min: usize, max: usize) -> Result<String, &'static str> {
    let value = value.as_str().ok_or("string_type")?;
    let length = value.chars().count();
    if length < min {
        Err("string_too_short")
    } else if length > max {
        Err("string_too_long")
    } else {
        Ok(value.to_owned())
    }
}
fn literal(value: &Value, variants: &[&str]) -> Result<Value, &'static str> {
    value
        .as_str()
        .filter(|v| variants.contains(v))
        .map(|v| json!(v))
        .ok_or("literal_error")
}
pub fn uuid_value(value: &Value) -> Result<Value, &'static str> {
    let raw = value.as_str().ok_or("uuid_type")?;
    uuid::Uuid::parse_str(raw)
        .map(|v| json!(v.to_string()))
        .map_err(|_| "uuid_parsing")
}
fn bounded_integer(value: &Value, min: i64, max: i64) -> Result<Value, &'static str> {
    let value = integer(value)?;
    if value < min {
        Err("greater_than_equal")
    } else if value > max {
        Err("less_than_equal")
    } else {
        Ok(json!(value))
    }
}
fn secret_ref(value: &str) -> bool {
    let Some((scheme, tail)) = value.split_once("://") else {
        return false;
    };
    !tail.is_empty()
        && !value.chars().any(crate::usage_parser::python_whitespace)
        && scheme
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
        && scheme
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"+.-".contains(&c))
        && !matches!(scheme, "http" | "https")
}

/// Parsed authority is public for the persisted-destination boundary. A malformed
/// port may pass URL-shape validation, as in urllib; it fails when used as a target.
#[derive(Clone, Debug)]
pub struct McpUrlParts {
    pub host: String,
    pub port: String,
    pub authority: String,
    pub path: String,
    pub query: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpUrlError;
pub fn split_https_url(input: &str) -> Result<McpUrlParts, McpUrlError> {
    let cleaned = input
        .trim_start_matches(|c: char| (c as u32) <= 0x20)
        .replace(['\t', '\r', '\n'], "");
    let (scheme, rest) = cleaned.split_once("://").ok_or(McpUrlError)?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(McpUrlError);
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    if !fragment.is_empty() {
        return Err(McpUrlError);
    }
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty()
        || authority.contains('@')
        || authority
            .chars()
            .any(|c| NFKC_DELIMITERS.contains(&(c as u32)))
    {
        return Err(McpUrlError);
    }
    let (host, port) = if let Some(tail) = authority.strip_prefix('[') {
        let (host, tail) = tail.split_once(']').ok_or(McpUrlError)?;
        let future = host
            .strip_prefix('v')
            .and_then(|s| s.split_once('.'))
            .is_some_and(|(version, suffix)| {
                !version.is_empty()
                    && version.bytes().all(|c| c.is_ascii_hexdigit())
                    && !suffix.is_empty()
            });
        let valid_scope = host
            .split_once('%')
            .is_none_or(|(_, scope)| !scope.is_empty() && !scope.contains('%'));
        if !future
            && (!valid_scope
                || !matches!(
                    host.split('%').next().unwrap_or("").parse::<IpAddr>(),
                    Ok(IpAddr::V6(_))
                ))
        {
            return Err(McpUrlError);
        }
        (
            host,
            if tail.is_empty() {
                ""
            } else {
                tail.strip_prefix(':').ok_or(McpUrlError)?
            },
        )
    } else {
        if authority.contains(['[', ']']) {
            return Err(McpUrlError);
        }
        authority.split_once(':').unwrap_or((authority, ""))
    };
    if host.is_empty() {
        return Err(McpUrlError);
    }
    let remainder = &rest[authority_end..];
    let (path, query) = remainder.split_once('?').unwrap_or((remainder, ""));
    let host = match host.split_once('%') {
        Some((address, scope)) => format!("{}%{scope}", address.to_lowercase()),
        None => host.to_lowercase(),
    };
    Ok(McpUrlParts {
        host,
        port: port.into(),
        authority: authority.into(),
        path: path.into(),
        query: query.into(),
    })
}
pub fn persisted_destination(input: &str) -> Result<(String, u16), McpUrlError> {
    let parts = split_https_url(input)?;
    let port = if parts.port.is_empty() {
        443
    } else {
        if !parts.port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(McpUrlError);
        }
        let parsed = parts.port.parse::<u16>().map_err(|_| McpUrlError)?;
        // Python `parsed.port or 443` deliberately maps zero to the default.
        if parsed == 0 { 443 } else { parsed }
    };
    Ok((parts.host, port))
}
fn https_url(value: &str) -> Result<Value, &'static str> {
    let parts = split_https_url(value).map_err(|_| "value_error")?;
    if let Ok(ip) = parts.host.split('%').next().unwrap_or("").parse::<IpAddr>()
        && !schema_global_ip(ip)
    {
        return Err("value_error");
    }
    Ok(json!(value.trim_end_matches('/')))
}
fn origins(values: &[Value]) -> Result<Value, &'static str> {
    let mut out = Vec::new();
    for value in values {
        let parts =
            split_https_url(value.as_str().ok_or("string_type")?).map_err(|_| "value_error")?;
        if !matches!(parts.path.as_str(), "" | "/") || !parts.query.is_empty() {
            return Err("value_error");
        }
        let origin = json!(format!("https://{}", parts.authority));
        if !out.contains(&origin) {
            out.push(origin);
        }
    }
    Ok(Value::Array(out))
}
const NFKC_DELIMITERS: &[u32] = &[
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2a74, 0xfe13, 0xfe16, 0xfe55, 0xfe56,
    0xfe5f, 0xfe6b, 0xff03, 0xff0f, 0xff1a, 0xff1f, 0xff20,
];
// Source Python 3.12 ipaddress tables, including backported global exceptions.
fn v4_private(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    if matches!(n, 0xc0000009 | 0xc000000a) {
        return false;
    }
    [
        (0x00000000u32, 8),
        (0x0a000000, 8),
        (0x7f000000, 8),
        (0xa9fe0000, 16),
        (0xac100000, 12),
        (0xc0000000, 24),
        (0xc00000aa, 31),
        (0xc0000200, 24),
        (0xc0a80000, 16),
        (0xc6120000, 15),
        (0xc6336400, 24),
        (0xcb007100, 24),
        (0xf0000000, 4),
    ]
    .iter()
    .any(|&(base, bits)| n >> (32 - bits) == base >> (32 - bits))
}
pub fn schema_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !v4_private(ip) && u32::from(ip) >> 22 != 0x64400000u32 >> 22,
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return schema_global_ip(IpAddr::V4(v4));
            }
            let n = u128::from(ip);
            let contains = |base: u128, bits: u32| n >> (128 - bits) == base >> (128 - bits);
            let except = contains((0x20010001u128 << 96) | 1, 128)
                || contains((0x20010001u128 << 96) | 2, 128)
                || contains(0x20010003u128 << 96, 32)
                || contains(0x200100040112u128 << 80, 48)
                || contains(0x20010020u128 << 96, 28)
                || contains(0x20010030u128 << 96, 28);
            except
                || ![
                    (0, 128),
                    (1, 128),
                    (0x0064ff9b0001u128 << 80, 48),
                    (0x0100u128 << 112, 64),
                    (0x20010000u128 << 96, 23),
                    (0x20010db8u128 << 96, 32),
                    (0x2002u128 << 112, 16),
                    (0x3fffu128 << 112, 20),
                    (0xfc00u128 << 112, 7),
                    (0xfe80u128 << 112, 10),
                ]
                .iter()
                .any(|&(base, bits)| contains(base, bits))
        }
    }
}
/// safe_fetch additionally rejects reserved and multicast records. Preserve its
/// source asymmetry: CGNAT DNS records pass this policy, while literal URL schema
/// validation rejects them through is_global. Runtime repeats destination checks.
pub fn safe_destination_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !v4_private(ip) && !ip.is_multicast() && !ip.is_broadcast(),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return safe_destination_ip(IpAddr::V4(v4));
            }
            // Python marks IPv6 outside 2000::/3 reserved (except multicast,
            // which is independently rejected); this also rejects NAT64.
            schema_global_ip(IpAddr::V6(ip))
                && !ip.is_multicast()
                && (ip.segments()[0] & 0xe000) == 0x2000
        }
    }
}

pub(crate) fn datetime(value: &Value) -> Result<Value, &'static str> {
    if value.is_boolean() || (!value.is_number() && !value.is_string()) {
        return Err("datetime_type");
    }
    let raw = value.as_str();
    let timestamp = value
        .as_f64()
        .or_else(|| raw.and_then(|s| s.parse::<f64>().ok()));
    if let Some(mut seconds) = timestamp {
        if !seconds.is_finite() {
            return Err("datetime_parsing");
        }
        if seconds.abs() > 20_000_000_000.0 {
            seconds /= 1000.0;
        }
        let mut integral = seconds.floor() as i64;
        let mut micros = ((seconds - seconds.floor()) * 1_000_000.0).round() as u32;
        if micros == 1_000_000 {
            integral = integral.checked_add(1).ok_or("datetime_parsing")?;
            micros = 0;
        }
        let nanos = micros * 1000;
        let date = DateTime::<Utc>::from_timestamp(integral, nanos).ok_or("datetime_parsing")?;
        if !(1..=9999).contains(&date.year()) {
            return Err("datetime_parsing");
        }
        return Ok(json!(date.to_rfc3339_opts(
            if nanos == 0 {
                SecondsFormat::Secs
            } else {
                SecondsFormat::Micros
            },
            true
        )));
    }
    let raw = raw.ok_or("datetime_type")?;
    if let Ok(date) = DateTime::parse_from_rfc3339(raw) {
        if !(1..=9999).contains(&date.year()) || date.timestamp_subsec_nanos() >= 1_000_000_000 {
            return Err("datetime_from_date_parsing");
        }
        return Ok(json!(date.to_rfc3339_opts(
            if date.timestamp_subsec_micros() == 0 {
                SecondsFormat::Secs
            } else {
                SecondsFormat::Micros
            },
            true
        )));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dt%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(date) = NaiveDateTime::parse_from_str(raw, format) {
            if !(1..=9999).contains(&date.year())
                || date.and_utc().timestamp_subsec_nanos() >= 1_000_000_000
            {
                return Err("datetime_from_date_parsing");
            }
            return Ok(json!(if date.and_utc().timestamp_subsec_micros() == 0 {
                date.format("%Y-%m-%dT%H:%M:%S").to_string()
            } else {
                date.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
            }));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        if !(1..=9999).contains(&date.year()) {
            return Err("datetime_from_date_parsing");
        }
        return Ok(json!(format!("{}T00:00:00", date.format("%Y-%m-%d"))));
    }
    Err("datetime_from_date_parsing")
}

pub fn validate_body(kind: McpBodyKind, body: &Value) -> Result<Value, Vec<Value>> {
    let Some(input) = body.as_object() else {
        return Err(vec![field_error(
            ["body".into()],
            if body.is_null() {
                "missing"
            } else {
                "model_attributes_type"
            },
        )]);
    };
    let fields: &[&str] = match kind {
        McpBodyKind::ServerCreate => &[
            "name",
            "description",
            "base_url",
            "transport",
            "auth_method",
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "allowed_origins",
            "timeout_ms",
            "max_concurrency",
            "response_limit_bytes",
        ],
        McpBodyKind::ServerUpdate => &[
            "name",
            "description",
            "base_url",
            "auth_method",
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "allowed_origins",
            "timeout_ms",
            "max_concurrency",
            "response_limit_bytes",
            "enabled",
        ],
        McpBodyKind::ConnectionCreate => &[
            "principal_type",
            "owner_user_id",
            "secret_ref",
            "scopes",
            "audience",
            "expires_at",
        ],
        McpBodyKind::ChannelGrant => &["tool_id", "channel", "read_only_only"],
        McpBodyKind::Discovery => &["connection_id", "principal_type"],
    };
    let mut errors = Vec::new();
    let mut out = Map::new();
    let patch = kind == McpBodyKind::ServerUpdate;
    for &field in fields {
        let Some(value) = input.get(field) else {
            if patch {
                continue;
            }
            let default = match field {
                "description" => json!(""),
                "transport" => json!("streamable_http"),
                "auth_method" => json!("none"),
                "oauth_metadata_url" | "oauth_resource" | "oauth_audience" | "owner_user_id"
                | "secret_ref" | "audience" | "expires_at" => Value::Null,
                "timeout_ms" => json!(30000),
                "max_concurrency" => json!(5),
                "response_limit_bytes" => json!(1048576),
                "scopes" => json!([]),
                "read_only_only" => json!(true),
                _ => {
                    errors.push(field_error(at(field), "missing"));
                    continue;
                }
            };
            out.insert(field.into(), default);
            continue;
        };
        let nullable = patch
            || matches!(
                field,
                "oauth_metadata_url"
                    | "oauth_resource"
                    | "oauth_audience"
                    | "owner_user_id"
                    | "secret_ref"
                    | "audience"
                    | "expires_at"
            );
        if nullable && value.is_null() {
            out.insert(field.into(), Value::Null);
            continue;
        }
        let parsed = match field {
            "name" => string(value, 1, 128).and_then(|s| {
                if !s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                    || !s
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b" _.-".contains(&c))
                {
                    Err("string_pattern_mismatch")
                } else {
                    Ok(json!(s))
                }
            }),
            "description" => string(value, 0, 2000).map(Value::String),
            "base_url" | "oauth_metadata_url" | "oauth_resource" => {
                string(value, if field == "base_url" { 1 } else { 0 }, 2048)
                    .and_then(|s| https_url(&s))
            }
            "oauth_audience" | "audience" => string(value, 0, 2048).map(Value::String),
            "owner_user_id" => string(value, 1, 255).map(Value::String),
            "secret_ref" => string(value, 1, 2048).and_then(|s| {
                if secret_ref(&s) {
                    Ok(json!(s))
                } else {
                    Err("value_error")
                }
            }),
            "transport" => literal(value, &["streamable_http"]),
            "auth_method" => literal(value, &["none", "bearer", "oauth"]),
            "principal_type" => literal(value, &["service_account", "user_delegated"]),
            "channel" => literal(
                value,
                &["preview", "hosted_private", "hosted_public", "embed", "api"],
            ),
            "connection_id" | "tool_id" => uuid_value(value),
            "timeout_ms" => bounded_integer(value, 100, 120000),
            "max_concurrency" => bounded_integer(value, 1, 32),
            "response_limit_bytes" => bounded_integer(value, 1024, 8388608),
            "enabled" => boolean(value).map(|v| json!(v)),
            "read_only_only" => {
                if value == &json!(true) || value.as_f64() == Some(1.0) {
                    Ok(json!(true))
                } else {
                    Err("literal_error")
                }
            }
            "expires_at" => datetime(value),
            "allowed_origins" | "scopes" => {
                if let Some(values) = value.as_array() {
                    let max = if field == "scopes" { 128 } else { 64 };
                    if values.len() > max {
                        Err("too_long")
                    } else if field == "allowed_origins" && values.is_empty() {
                        Err("too_short")
                    } else {
                        let mut invalid = false;
                        for (index, item) in values.iter().enumerate() {
                            if !item.is_string() {
                                errors.push(field_error(
                                    ["body".into(), field.into(), index.to_string()],
                                    "string_type",
                                ));
                                invalid = true;
                            }
                        }
                        if invalid {
                            continue;
                        }
                        if field == "allowed_origins" {
                            origins(values)
                        } else {
                            let mut scopes = values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|s| s.trim_matches(crate::usage_parser::python_whitespace))
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>();
                            scopes.sort_unstable();
                            scopes.dedup();
                            Ok(json!(scopes))
                        }
                    }
                } else {
                    Err("list_type")
                }
            }
            _ => unreachable!(),
        };
        match parsed {
            Ok(value) => {
                out.insert(field.into(), value);
            }
            Err(kind) => errors.push(field_error(at(field), kind)),
        }
    }
    for field in input.keys() {
        if !fields.contains(&field.as_str()) {
            errors.push(field_error(at(field), "extra_forbidden"));
        }
    }
    if errors.is_empty() {
        if kind == McpBodyKind::ServerCreate {
            let oauth = ["oauth_metadata_url", "oauth_resource", "oauth_audience"].map(|key| {
                out.get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            });
            if (out["auth_method"] == "oauth" && !oauth.iter().all(|v| *v))
                || (out["auth_method"] != "oauth" && oauth.iter().any(|v| *v))
            {
                errors.push(field_error(["body".into()], "value_error"));
            }
        }
        if kind == McpBodyKind::ConnectionCreate
            && ((out["principal_type"] == "service_account" && !out["owner_user_id"].is_null())
                || (out["principal_type"] == "user_delegated" && out["owner_user_id"].is_null()))
        {
            errors.push(field_error(["body".into()], "value_error"));
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(errors)
    }
}
