//! Stable canonical evidence encoding shared with native signed envelopes.
use serde_json::Value;
use sha2::{Digest, Sha256};
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| format!("{}:{}", serde_json::to_string(k).unwrap(), canonical(&o[k])))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        Value::Number(n) if n.is_f64() => python_float(n.as_f64().expect("finite JSON number")),
        _ => value.to_string(),
    }
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn digest(value: &Value) -> String {
    sha256(canonical(value).as_bytes())
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
