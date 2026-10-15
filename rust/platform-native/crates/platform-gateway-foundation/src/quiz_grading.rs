//! Provider-free synchronous quiz grading, matching the canonical Python grader.
//! Unicode behavior is pinned to the source-golden CPython Unicode 15.0 tables.
use crate::quiz_unicode;
use serde_json::{Map, Value, json};

pub fn unicode_member(c: char, ranges: &[(u32, u32)]) -> bool {
    let n = c as u32;
    let at = ranges.partition_point(|(_, end)| *end < n);
    ranges
        .get(at)
        .is_some_and(|(start, end)| *start <= n && n <= *end)
}
pub fn python_trim(text: &str) -> &str {
    text.trim_matches(|c| unicode_member(c, quiz_unicode::SPACE))
}
fn mapped(c: char, table: &[(u32, &str)], out: &mut String) {
    match table.binary_search_by_key(&(c as u32), |(key, _)| *key) {
        Ok(i) => out.push_str(table[i].1),
        Err(_) => out.push(c),
    }
}
fn upper(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        mapped(c, quiz_unicode::UPPER, &mut out);
    }
    out
}
fn lower(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().copied().enumerate() {
        // Python's context-sensitive Final_Sigma rule; per-codepoint lower is insufficient.
        if c == '\u{03a3}' {
            let before = chars[..i]
                .iter()
                .rev()
                .copied()
                .find(|c| !unicode_member(*c, quiz_unicode::CASE_IGNORABLE));
            let after = chars[i + 1..]
                .iter()
                .copied()
                .find(|c| !unicode_member(*c, quiz_unicode::CASE_IGNORABLE));
            if before.is_some_and(|c| unicode_member(c, quiz_unicode::CASED))
                && !after.is_some_and(|c| unicode_member(c, quiz_unicode::CASED))
            {
                out.push('\u{03c2}');
                continue;
            }
        }
        mapped(c, quiz_unicode::LOWER, &mut out);
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
                c if !unicode_member(c, quiz_unicode::PRINTABLE) => {
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

/// Correctly round the *binary64 division result*, not the mathematical rational.
/// Integer arithmetic prevents a second rounding from `score * 10000.0`.
pub fn rounded_score(correct: usize, total: usize) -> f64 {
    if total == 0 || correct == 0 {
        return 0.0;
    }
    let value = correct as f64 / total as f64;
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let shift = 1075 - exponent;
    let scaled = u128::from(mantissa) * 10000;
    if shift >= 128 {
        return 0.0;
    }
    // A grade score is in [0,1], so this shift is always positive.
    let shift = shift as u32;
    let integral = scaled >> shift;
    let remainder = scaled & ((1u128 << shift) - 1);
    let midpoint = 1u128 << (shift - 1);
    let round_up = remainder > midpoint || (remainder == midpoint && integral & 1 == 1);
    (integral + u128::from(round_up)) as f64 / 10000.0
}
fn objective(kind: &str, raw: &Value, answer: &str) -> bool {
    let values: Vec<&Value> = match raw {
        Value::Array(v) => v.iter().collect(),
        _ => vec![raw],
    };
    if kind == "true_false" {
        let correct = values
            .first()
            .map(|v| lower(&python_str(v)))
            .unwrap_or_default();
        return python_trim(&lower(answer)) == python_trim(&correct);
    }
    if kind == "mc_multi" {
        let mut given: Vec<String> = answer
            .split(',')
            .map(python_trim)
            .filter(|s| !s.is_empty())
            .map(upper)
            .collect();
        let mut expected: Vec<String> = values
            .iter()
            .map(|v| python_trim(&upper(&python_str(v))).to_owned())
            .collect();
        given.sort();
        expected.sort();
        return given == expected;
    }
    let given = upper(answer);
    values
        .iter()
        .any(|v| python_trim(&given) == python_trim(&upper(&python_str(v))))
}

/// Source records must contain `id` and `question_num`; HTTP validates string answers.
/// No model registry is involved, so every short answer remains pending.
pub fn grade(questions: &[Value], answers: &Map<String, Value>) -> Value {
    let mut per_question = Vec::with_capacity(questions.len());
    let mut correct_count = 0usize;
    for question in questions {
        let id = python_str(&question["id"]);
        let answer = answers.get(&id).cloned().unwrap_or_else(|| json!(""));
        let raw = question
            .get("correct_answer")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let kind = question
            .get("question_type")
            .and_then(Value::as_str)
            .unwrap_or("mc_single");
        let pending = kind == "short_answer";
        let correct = !pending && objective(kind, &raw, answer.as_str().unwrap_or(""));
        correct_count += usize::from(correct);
        let display = if pending {
            raw.as_array()
                .and_then(|a| a.first())
                .cloned()
                .unwrap_or_else(|| json!(python_str(&raw)))
        } else {
            json!(match &raw {
                Value::Array(a) => a.iter().map(python_str).collect::<Vec<_>>().join(", "),
                _ => python_str(&raw),
            })
        };
        let mut item = json!({"question_num":question["question_num"],"question_id":id,"correct":correct,
            "user_answer":answer,"correct_answer":display,"explanation":question.get("explanation").cloned().unwrap_or_else(|| json!(""))});
        if pending {
            item["needs_ai_grading"] = json!(true);
        }
        per_question.push(item);
    }
    json!({"total_score":rounded_score(correct_count,questions.len()),"correct_count":correct_count,
        "total_count":questions.len(),"per_question":per_question})
}
