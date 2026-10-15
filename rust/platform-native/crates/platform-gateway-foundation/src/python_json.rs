use serde_json::{Value, json};

/// Python's deterministic JSON spelling, including float notation and optional
/// ensure_ascii escaping, shared by proof, share-preview and request hashes.
pub fn python_json(value: &Value, ascii: bool, spaced: bool) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}{}{}",
                        python_json(&json!(key), ascii, spaced),
                        if spaced { ": " } else { ":" },
                        python_json(&map[key], ascii, spaced)
                    ))
                    .collect::<Vec<_>>()
                    .join(if spaced { ", " } else { "," })
            )
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|v| python_json(v, ascii, spaced))
                .collect::<Vec<_>>()
                .join(if spaced { ", " } else { "," })
        ),
        Value::String(text) => {
            let encoded = serde_json::to_string(text).expect("string serialization");
            if !ascii {
                return encoded;
            }
            let mut out = String::new();
            for c in encoded.chars() {
                if c.is_ascii() {
                    out.push(c);
                } else {
                    let mut buffer = [0u16; 2];
                    for unit in c.encode_utf16(&mut buffer) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
            out
        }
        Value::Number(n) if n.is_f64() => {
            crate::assistant_catalog::python_float(n.as_f64().expect("finite JSON number"))
        }
        _ => value.to_string(),
    }
}

