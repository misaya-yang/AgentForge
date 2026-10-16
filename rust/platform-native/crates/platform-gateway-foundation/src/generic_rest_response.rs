//! Byte-level HTTP response behavior of HTTPConnector and GenericRESTAdapter.
use super::{AdapterFailure, RestResponse};
use serde_json::Value;

/// Python json.loads(bytes) detects Unicode independently of Content-Type.
/// Non-JSON responses first attempt the same JSON decode, then use httpx text.
pub fn decode_http_response_bytes(
    status: u16,
    content_type: &str,
    bytes: &[u8],
) -> Result<RestResponse, AdapterFailure> {
    if !(200..300).contains(&status) {
        return Err(AdapterFailure::HttpStatus(status));
    }
    let parsed = json_text(bytes).and_then(|text| serde_json::from_str::<Value>(&text).ok());
    match parsed {
        Some(value) => Ok(RestResponse::Json(value)),
        None if content_type.contains("application/json") => Err(AdapterFailure::Other),
        None => Ok(RestResponse::Json(
            serde_json::json!({"raw": response_text(content_type, bytes)?}),
        )),
    }
}

fn json_text(bytes: &[u8]) -> Option<String> {
    let (bytes, width, little) = if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        (&bytes[4..], 4, false)
    } else if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
        (&bytes[4..], 4, true)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (&bytes[2..], 2, false)
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (&bytes[2..], 2, true)
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8(bytes[3..].to_vec()).ok();
    } else if bytes.len() >= 4 && bytes[0] == 0 {
        (bytes, if bytes[1] == 0 { 4 } else { 2 }, false)
    } else if bytes.len() >= 4 && bytes[1] == 0 {
        (
            bytes,
            if bytes[2] == 0 && bytes[3] == 0 { 4 } else { 2 },
            true,
        )
    } else if bytes.len() == 2 && bytes[0] == 0 {
        (bytes, 2, false)
    } else if bytes.len() == 2 && bytes[1] == 0 {
        (bytes, 2, true)
    } else {
        return String::from_utf8(bytes.to_vec()).ok();
    };
    if bytes.len() % width != 0 {
        return None;
    }
    if width == 2 {
        let words = bytes.chunks_exact(2).map(|pair| {
            let pair = [pair[0], pair[1]];
            if little {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        char::decode_utf16(words)
            .collect::<Result<String, _>>()
            .ok()
    } else {
        bytes
            .chunks_exact(4)
            .map(|word| {
                let word = [word[0], word[1], word[2], word[3]];
                char::from_u32(if little {
                    u32::from_le_bytes(word)
                } else {
                    u32::from_be_bytes(word)
                })
            })
            .collect()
    }
}

fn normalized_codec(label: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '.' {
            if separator && !result.is_empty() {
                result.push('_');
            }
            separator = false;
            result.push(c);
        } else {
            separator = true;
        }
    }
    result
}
fn unicode_text(
    bytes: &[u8],
    width: usize,
    endian: Option<bool>,
) -> Result<String, AdapterFailure> {
    let (bytes, little) = if let Some(little) = endian {
        (bytes, little)
    } else if width == 2 && bytes.starts_with(&[0xff, 0xfe]) {
        (&bytes[2..], true)
    } else if width == 2 && bytes.starts_with(&[0xfe, 0xff]) {
        (&bytes[2..], false)
    } else if width == 4 && bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
        (&bytes[4..], true)
    } else if width == 4 && bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        (&bytes[4..], false)
    } else if bytes.len() < width {
        return Ok(if bytes.is_empty() {
            String::new()
        } else {
            "\u{fffd}".into()
        });
    } else {
        return Err(AdapterFailure::Other);
    };
    if width == 2 {
        let word = |i| {
            let pair = [bytes[i], bytes[i + 1]];
            if little {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        };
        let mut result = String::new();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let first = word(i);
            if (0xd800..=0xdbff).contains(&first) {
                // The incremental source decoder replaces a truncated surrogate
                // and its incomplete following code unit as one invalid span.
                if i + 3 >= bytes.len() {
                    result.push('\u{fffd}');
                    i = bytes.len();
                    break;
                }
                let second = word(i + 2);
                if (0xdc00..=0xdfff).contains(&second) {
                    let scalar =
                        0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00;
                    result.push(char::from_u32(scalar).expect("surrogate pair"));
                    i += 4;
                    continue;
                }
            }
            result.push(char::from_u32(u32::from(first)).unwrap_or('\u{fffd}'));
            i += 2;
        }
        if i < bytes.len() {
            result.push('\u{fffd}');
        }
        return Ok(result);
    }
    let mut result: String = {
        bytes
            .chunks_exact(4)
            .map(|v| {
                let v = [v[0], v[1], v[2], v[3]];
                char::from_u32(if little {
                    u32::from_le_bytes(v)
                } else {
                    u32::from_be_bytes(v)
                })
                .unwrap_or('\u{fffd}')
            })
            .collect()
    };
    if bytes.len() % width != 0 {
        result.push('\u{fffd}');
    }
    Ok(result)
}
fn response_text(content_type: &str, bytes: &[u8]) -> Result<String, AdapterFailure> {
    let charset = content_type.split(';').skip(1).find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).to_ascii_lowercase())
    });
    let Some(label) = charset else {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    };
    let normalized = normalized_codec(&label);
    match normalized.as_str() {
        "utf_16" | "utf16" | "u16" => return unicode_text(bytes, 2, None),
        "utf_16_le" | "utf_16le" | "unicodelittleunmarked" => {
            return unicode_text(bytes, 2, Some(true));
        }
        "utf_16_be" | "utf_16be" | "unicodebigunmarked" => {
            return unicode_text(bytes, 2, Some(false));
        }
        "utf_32" | "utf32" | "u32" => return unicode_text(bytes, 4, None),
        "utf_32_le" | "utf_32le" => return unicode_text(bytes, 4, Some(true)),
        "utf_32_be" | "utf_32be" => return unicode_text(bytes, 4, Some(false)),
        "utf_8_sig" => {
            return Ok(String::from_utf8_lossy(
                bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
            )
            .into_owned());
        }
        _ => {}
    }
    // Source codec tables differ from WHATWG (notably undefined Windows bytes).
    // Immutable errors=replace tables have no Python runtime dependency.
    static TABLES: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("generic_rest_codecs.json"))
            .expect("embedded codec tables")
    });
    if let Some(canonical) = TABLES["aliases"][&normalized].as_str() {
        if let Some(mapping) = TABLES["maps"][canonical].as_str() {
            let characters: Vec<char> = mapping.chars().collect();
            return Ok(bytes.iter().map(|b| characters[*b as usize]).collect());
        }
    }
    if let Some(decoded) = crate::generic_rest_multibyte::decode(&normalized, bytes) {
        return Ok(decoded);
    }
    Ok(encoding_rs::Encoding::for_label(label.as_bytes())
        .map(|encoding| encoding.decode_without_bom_handling(bytes).0.into_owned())
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()))
}
