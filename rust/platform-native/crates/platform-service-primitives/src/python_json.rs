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
            python_float(n.as_f64().expect("finite JSON number"))
        }
        _ => value.to_string(),
    }
}


pub fn python_float(value: f64) -> String {
    let raw = serde_json::to_string(&value).expect("JSON values contain finite numbers");
    let (sign, unsigned) = if let Some(value) = raw.strip_prefix('-') {
        ("-", value)
    } else {
        ("", raw.as_str())
    };
    let (mantissa, exponent) = unsigned.split_once('e').map_or((unsigned, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("serializer exponent"))
    });
    let decimal_at = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let zeros = digits.bytes().take_while(|c| *c == b'0').count();
    if zeros == digits.len() {
        return format!("{sign}0.0");
    }
    let scientific_exponent = exponent + decimal_at - zeros as i32 - 1;
    digits.drain(..zeros);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    if !(-4..16).contains(&scientific_exponent) {
        let fraction = if digits.len() == 1 {
            String::new()
        } else {
            format!(".{}", &digits[1..])
        };
        return format!("{sign}{}{fraction}e{scientific_exponent:+03}", &digits[..1]);
    }
    let decimal = scientific_exponent + 1;
    if decimal <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-decimal) as usize), digits)
    } else if decimal as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(decimal as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..decimal as usize],
            &digits[decimal as usize..]
        )
    }
}
