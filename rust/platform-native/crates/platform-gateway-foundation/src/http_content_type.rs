//! FastAPI Content-Type classification via Python email.message.Message.
//!
//! Starlette decodes raw header bytes as Latin-1. Python splits at the first
//! semicolon, strips whitespace, lowercases, and falls back to text/plain unless
//! the media type contains exactly one slash. It does not validate MIME tokens or
//! remove comments/quotes. Keep that behavior rather than using a stricter MIME
//! parser. Missing/empty headers are non-JSON under strict_content_type=True.
use axum::http::{HeaderMap, HeaderValue, header::CONTENT_TYPE};

pub fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers.get(CONTENT_TYPE).is_some_and(is_json_media_type)
}

/// Classify one present header; callers own explicit missing-header policies.
pub(crate) fn is_json_media_type(value: &HeaderValue) -> bool {
    let mut media = value
        .as_bytes()
        .split(|byte| *byte == b';')
        .next()
        .unwrap_or_default();
    // Python str.strip whitespace representable in Starlette's Latin-1 string.
    let whitespace = |byte: u8| matches!(byte, 0x09..=0x0d | 0x1c..=0x20 | 0x85 | 0xa0);
    while media.first().is_some_and(|byte| whitespace(*byte)) {
        media = &media[1..];
    }
    while media.last().is_some_and(|byte| whitespace(*byte)) {
        media = &media[..media.len() - 1];
    }
    let Some(slash) = media.iter().position(|byte| *byte == b'/') else {
        return false;
    };
    let (maintype, rest) = media.split_at(slash);
    let subtype = &rest[1..];
    if subtype.contains(&b'/') || !maintype.eq_ignore_ascii_case(b"application") {
        return false;
    }
    subtype.eq_ignore_ascii_case(b"json")
        || subtype
            .get(subtype.len().saturating_sub(5)..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(b"+json"))
}

