//! Validate JSON once, then patch only explicitly authorized top-level values.
//! Unknown fields, numeric spelling, ordering and whitespace remain byte-exact.
use super::*;
use std::{collections::BTreeMap, ops::Range};
pub(super) struct RawObject {
    pub value: Value,
    spans: BTreeMap<String, Range<usize>>,
    close: usize,
}
fn invalid() -> LlmError {
    LlmError::new(400, "llm_json_invalid_or_ambiguous")
}
fn whitespace(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
}
fn string_end(bytes: &[u8], mut i: usize) -> LlmResult<usize> {
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err(invalid())
}
fn value_end(bytes: &[u8], mut i: usize) -> LlmResult<usize> {
    if bytes[i] == b'"' {
        return string_end(bytes, i);
    }
    if matches!(bytes[i], b'{' | b'[') {
        let mut depth = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i = string_end(bytes, i)?;
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i + 1);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        return Err(invalid());
    }
    while i < bytes.len() && !matches!(bytes[i], b',' | b'}') && !bytes[i].is_ascii_whitespace() {
        i += 1
    }
    Ok(i)
}
impl RawObject {
    pub fn parse(bytes: &[u8]) -> LlmResult<Self> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        let mut spans = BTreeMap::new();
        let mut i = 0;
        whitespace(bytes, &mut i);
        i += 1;
        loop {
            whitespace(bytes, &mut i);
            if bytes.get(i) == Some(&b'}') {
                return Ok(Self {
                    value,
                    spans,
                    close: i,
                });
            }
            if bytes.get(i) != Some(&b'"') {
                return Err(invalid());
            }
            let end = string_end(bytes, i)?;
            let key: String = serde_json::from_slice(&bytes[i..end]).map_err(|_| invalid())?;
            i = end;
            whitespace(bytes, &mut i);
            i += 1;
            whitespace(bytes, &mut i);
            let start = i;
            i = value_end(bytes, i)?;
            if spans.insert(key, start..i).is_some() {
                return Err(invalid());
            }
            whitespace(bytes, &mut i);
            if bytes.get(i) == Some(&b',') {
                i += 1;
            }
        }
    }
    pub fn patch(&self, bytes: &Bytes, changes: &BTreeMap<String, Value>) -> LlmResult<Bytes> {
        if changes.is_empty() {
            return Ok(bytes.clone());
        }
        let mut replacements = Vec::new();
        let mut added = Vec::new();
        for (key, value) in changes {
            if self.value.get(key) == Some(value) {
                continue;
            }
            let encoded = serde_json::to_vec(value).map_err(|_| invalid())?;
            if let Some(range) = self.spans.get(key) {
                replacements.push((range.clone(), encoded))
            } else {
                added.push(format!(
                    "{}:{}",
                    serde_json::to_string(key).map_err(|_| invalid())?,
                    String::from_utf8(encoded).map_err(|_| invalid())?
                ));
            }
        }
        if !added.is_empty() {
            let separator = if self.spans.is_empty() { "" } else { "," };
            replacements.push((
                self.close..self.close,
                format!("{separator}{}", added.join(",")).into_bytes(),
            ));
        }
        replacements.sort_by_key(|(range, _)| range.start);
        let mut out = Vec::new();
        let mut offset = 0;
        for (range, value) in replacements {
            out.extend_from_slice(&bytes[offset..range.start]);
            out.extend(value);
            offset = range.end;
        }
        out.extend_from_slice(&bytes[offset..]);
        Ok(Bytes::from(out))
    }
}
