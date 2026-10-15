//! The two canonical memory request models and required DELETE query field.
//! Unknown model fields are ignored; lengths count Unicode scalar characters.
//! Wire scope retains the shared decoder's 128-level nesting limit and Rust
//! Unicode-scalar strings; Python's lone-surrogate strings are not representable.
use crate::{
    assistant_memory_types::AssistantMemoryError, local_nodes_types::LocalNodesError,
    rate_config_api::boolean,
};
use axum::http::HeaderMap;
use serde_json::{Map, Value, json};
use std::{borrow::Cow, collections::HashSet};

#[derive(Default)]
pub struct MemoryBody {
    value: Value,
    /// The shared decoder represents Python's nonfinite numbers with a string
    /// absent from every original string literal. Preserve that distinction
    /// through validation: ignored extras are valid, but an error input with a
    /// nonfinite number fails JSONResponse serialization in the source.
    original_strings: Option<HashSet<String>>,
}
impl MemoryBody {
    pub fn from_value(value: Value) -> Self {
        Self {
            value,
            original_strings: None,
        }
    }
    fn nonfinite(&self, value: &Value) -> bool {
        let Some(strings) = &self.original_strings else {
            return false;
        };
        fn contains(value: &Value, strings: &HashSet<String>) -> bool {
            match value {
                Value::String(value) => !strings.contains(value),
                Value::Array(values) => values.iter().any(|v| contains(v, strings)),
                Value::Object(values) => values.values().any(|v| contains(v, strings)),
                _ => false,
            }
        }
        contains(value, strings)
    }
    fn invalid(&self, errors: Vec<Value>) -> AssistantMemoryError {
        if errors.iter().any(|error| self.nonfinite(&error["input"])) {
            AssistantMemoryError::Internal
        } else {
            AssistantMemoryError::Http {
                status: 422,
                detail: json!(errors),
            }
        }
    }
    fn object(&self) -> Result<&Map<String, Value>, AssistantMemoryError> {
        if self.value.is_null() {
            return Err(self.invalid(vec![
                json!({"type":"missing","loc":["body"],"msg":"Field required","input":null}),
            ]));
        }
        self.value.as_object().ok_or_else(|| {
            self.invalid(vec![json!({
                "type":"model_attributes_type","loc":["body"],
                "msg":"Input should be a valid dictionary or object to extract fields from",
                "input":self.value,
            })])
        })
    }
}

/// Reuse the existing Python JSON syntax and Content-Type implementation rather
/// than introducing a subtly different malformed-JSON contract for this family.
pub fn parse_body(bytes: &[u8], headers: &HeaderMap) -> Result<MemoryBody, AssistantMemoryError> {
    let originally_empty = bytes.is_empty();
    let bytes = if crate::http_content_type::is_json_content_type(headers) {
        json_encoding(bytes)?
    } else {
        Cow::Borrowed(bytes)
    };
    let bytes = bytes.as_ref();
    if !originally_empty && bytes.is_empty() {
        return Err(AssistantMemoryError::Http {
            status: 422,
            detail: json!([{
                "type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},
                "ctx":{"error":"Expecting value"}
            }]),
        });
    }
    let (value, nonfinite) =
        crate::local_nodes_api::json_body(bytes, headers).map_err(|error| match error {
            LocalNodesError::Http { status, detail } => {
                AssistantMemoryError::Http { status, detail }
            }
            LocalNodesError::Cancelled => AssistantMemoryError::Cancelled,
            LocalNodesError::Repository(_) | LocalNodesError::Internal => {
                AssistantMemoryError::Internal
            }
        })?;
    let original_strings = nonfinite.then(|| {
        let mut strings = HashSet::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        if let Ok(value) = serde_json::from_slice::<String>(&bytes[start..i]) {
                            strings.insert(value);
                        }
                        break;
                    }
                    _ => i += 1,
                }
            }
        }
        strings
    });
    Ok(MemoryBody {
        value,
        original_strings,
    })
}

// json.loads(bytes) uses json.detect_encoding before decoding. Match its BOM
// order and zero-byte inference; a declared charset parameter is not consulted.
fn json_encoding(bytes: &[u8]) -> Result<Cow<'_, [u8]>, AssistantMemoryError> {
    enum Encoding {
        Utf8,
        Utf16(bool),
        Utf32(bool),
    }
    let (encoding, offset) = if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
        (Encoding::Utf32(true), 4)
    } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        (Encoding::Utf32(false), 4)
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (Encoding::Utf8, 3)
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (Encoding::Utf16(true), 2)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Encoding::Utf16(false), 2)
    } else if bytes.len() >= 4 && bytes[0] == 0 {
        (
            if bytes[1] == 0 {
                Encoding::Utf32(false)
            } else {
                Encoding::Utf16(false)
            },
            0,
        )
    } else if bytes.len() >= 4 && bytes[1] == 0 {
        (
            if bytes[2] == 0 && bytes[3] == 0 {
                Encoding::Utf32(true)
            } else {
                Encoding::Utf16(true)
            },
            0,
        )
    } else if bytes.len() == 2 && bytes[0] == 0 {
        (Encoding::Utf16(false), 0)
    } else if bytes.len() == 2 && bytes[1] == 0 {
        (Encoding::Utf16(true), 0)
    } else {
        (Encoding::Utf8, 0)
    };
    let bytes = &bytes[offset..];
    let invalid = || AssistantMemoryError::detail(400, "There was an error parsing the body");
    match encoding {
        Encoding::Utf8 => Ok(Cow::Borrowed(bytes)),
        Encoding::Utf16(little) => {
            if !bytes.len().is_multiple_of(2) {
                return Err(invalid());
            }
            let words = bytes.as_chunks::<2>().0.iter().map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            });
            let text: String = char::decode_utf16(words)
                .collect::<Result<_, _>>()
                .map_err(|_| invalid())?;
            Ok(Cow::Owned(text.into_bytes()))
        }
        Encoding::Utf32(little) => {
            if !bytes.len().is_multiple_of(4) {
                return Err(invalid());
            }
            let mut text = String::new();
            for b in bytes.as_chunks::<4>().0 {
                let value = if little {
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                } else {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
                };
                text.push(char::from_u32(value).ok_or_else(invalid)?);
            }
            Ok(Cow::Owned(text.into_bytes()))
        }
    }
}

fn problem(field: &str, kind: &str, message: impl Into<String>, input: &Value) -> Value {
    json!({"type":kind,"loc":["body",field],"msg":message.into(),"input":input})
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryPreferenceUpdate {
    pub enabled: bool,
}
impl MemoryPreferenceUpdate {
    pub fn parse(raw: &MemoryBody) -> Result<Self, AssistantMemoryError> {
        let object = raw.object()?;
        let Some(value) = object.get("enabled") else {
            return Err(raw.invalid(vec![problem(
                "enabled",
                "missing",
                "Field required",
                &raw.value,
            )]));
        };
        if raw.nonfinite(value) {
            return Err(AssistantMemoryError::Internal);
        }
        match boolean(value) {
            Ok(enabled) => Ok(Self { enabled }),
            Err(kind) => Err(raw.invalid(vec![problem(
                "enabled",
                kind,
                if kind == "bool_parsing" {
                    "Input should be a valid boolean, unable to interpret input"
                } else {
                    "Input should be a valid boolean"
                },
                value,
            )])),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryItemUpdate {
    pub key: String,
    pub value: String,
}
impl MemoryItemUpdate {
    pub fn parse(raw: &MemoryBody) -> Result<Self, AssistantMemoryError> {
        let object = raw.object()?;
        let mut errors = vec![];
        let mut values = [String::new(), String::new()];
        for (i, (field, max)) in [("key", 255), ("value", 4000)].into_iter().enumerate() {
            let Some(value) = object.get(field) else {
                errors.push(problem(field, "missing", "Field required", &raw.value));
                continue;
            };
            if raw.nonfinite(value) {
                return Err(AssistantMemoryError::Internal);
            }
            let Some(text) = value.as_str() else {
                errors.push(problem(
                    field,
                    "string_type",
                    "Input should be a valid string",
                    value,
                ));
                continue;
            };
            if let Some(error) = length_problem(text, max, json!(["body", field])) {
                errors.push(error);
            } else {
                values[i] = text.to_owned();
            }
        }
        if !errors.is_empty() {
            return Err(raw.invalid(errors));
        }
        let [key, value] = values;
        Ok(Self { key, value })
    }
}
fn length_problem(text: &str, max: usize, location: Value) -> Option<Value> {
    let len = text.chars().count();
    if len == 0 {
        Some(
            json!({"type":"string_too_short","loc":location,"msg":"String should have at least 1 character","input":text,"ctx":{"min_length":1}}),
        )
    } else if len > max {
        Some(
            json!({"type":"string_too_long","loc":location,"msg":format!("String should have at most {max} characters"),"input":text,"ctx":{"max_length":max}}),
        )
    } else {
        None
    }
}
fn decode_query_component(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let (Some(a), Some(b)) = (
                    (bytes[i + 1] as char).to_digit(16),
                    (bytes[i + 2] as char).to_digit(16),
                ) {
                    out.push((a * 16 + b) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
pub fn delete_key(query: &str) -> Result<String, AssistantMemoryError> {
    // QueryParams.get uses the last occurrence, including a final empty value.
    let key = query
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            (decode_query_component(name) == "key").then(|| decode_query_component(value))
        })
        .next_back();
    let Some(key) = key else {
        return Err(AssistantMemoryError::Http {
            status: 422,
            detail: json!([{
                "type":"missing","loc":["query","key"],"msg":"Field required","input":null
            }]),
        });
    };
    if let Some(error) = length_problem(&key, 255, json!(["query", "key"])) {
        return Err(AssistantMemoryError::Http {
            status: 422,
            detail: json!([error]),
        });
    }
    Ok(key)
}
