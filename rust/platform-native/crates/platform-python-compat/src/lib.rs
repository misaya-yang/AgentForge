//! Pinned CPython value formatting and Unicode semantics shared by host adapters.
pub mod unicode;
use serde_json::Value;
pub fn unicode_member(c: char, ranges: &[(u32, u32)]) -> bool {
    let n = c as u32;
    let at = ranges.partition_point(|(_, end)| *end < n);
    ranges
        .get(at)
        .is_some_and(|(start, end)| *start <= n && n <= *end)
}
pub fn python_trim(text: &str) -> &str {
    text.trim_matches(|c| unicode_member(c, unicode::SPACE))
}
fn mapped(c: char, table: &[(u32, &str)], out: &mut String) {
    match table.binary_search_by_key(&(c as u32), |(key, _)| *key) {
        Ok(i) => out.push_str(table[i].1),
        Err(_) => out.push(c),
    }
}
pub fn upper(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        mapped(c, unicode::UPPER, &mut out);
    }
    out
}
pub fn lower(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().copied().enumerate() {
        // Python's context-sensitive Final_Sigma rule; per-codepoint lower is insufficient.
        if c == '\u{03a3}' {
            let before = chars[..i]
                .iter()
                .rev()
                .copied()
                .find(|c| !unicode_member(*c, unicode::CASE_IGNORABLE));
            let after = chars[i + 1..]
                .iter()
                .copied()
                .find(|c| !unicode_member(*c, unicode::CASE_IGNORABLE));
            if before.is_some_and(|c| unicode_member(c, unicode::CASED))
                && !after.is_some_and(|c| unicode_member(c, unicode::CASED))
            {
                out.push('\u{03c2}');
                continue;
            }
        }
        mapped(c, unicode::LOWER, &mut out);
    }
    out
}
fn repr_string(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        if c == quote || c == '\\' {
            out.push('\\');
            out.push(c);
        } else {
            match c {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if !unicode_member(c, unicode::PRINTABLE) => {
                    let n = c as u32;
                    if n <= 0xff {
                        out.push_str(&format!("\\x{n:02x}"));
                    } else if n <= 0xffff {
                        out.push_str(&format!("\\u{n:04x}"));
                    } else {
                        out.push_str(&format!("\\U{n:08x}"));
                    }
                }
                c => out.push(c),
            }
        }
    }
    out.push(quote);
    out
}
fn python_repr(value: &Value) -> String {
    match value {
        Value::String(s) => repr_string(s),
        _ => python_str(value),
    }
}
// serde_json's shortest binary64 digits use the same round-trip objective as
// CPython; normalize its fixed/scientific boundary and exponent padding.
fn python_float(source: &str) -> String {
    let (sign, unsigned) = source.strip_prefix('-').map_or(("", source), |s| ("-", s));
    let (mantissa, exp) = unsigned
        .split_once('e')
        .map_or((unsigned, 0), |(m, e)| (m, e.parse::<i32>().unwrap_or(0)));
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = all.bytes().take_while(|b| *b == b'0').count();
    if leading == all.len() {
        return format!("{sign}0.0");
    }
    let digits = all[leading..].trim_end_matches('0');
    let exponent = decimal - leading as i32 - 1 + exp;
    if !(-4..16).contains(&exponent) {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        return format!(
            "{sign}{}{tail}e{}{abs:02}",
            &digits[..1],
            if exponent < 0 { '-' } else { '+' },
            abs = exponent.unsigned_abs()
        );
    }
    if exponent < 0 {
        return format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize));
    }
    let split = exponent as usize + 1;
    if split >= digits.len() {
        format!("{sign}{digits}{}.0", "0".repeat(split - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..split], &digits[split..])
    }
}
pub fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if !n.is_f64() {
                return n.to_string();
            }
            python_float(&n.to_string())
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", repr_string(k), python_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn value_rendering_preserves_python_compatibility_without_quiz_dependency() {
        assert_eq!(
            python_str(&json!([true, null, "a\nb"])),
            "[True, None, 'a\\nb']"
        );
        assert_eq!(upper("ß"), "SS");
        assert_eq!(lower("ΟΣ"), "ος");
        assert_eq!(python_trim("\u{2003} text \u{2003}"), "text");
        assert_eq!(python_str(&json!(1e-7)), "1e-07");
    }
}
