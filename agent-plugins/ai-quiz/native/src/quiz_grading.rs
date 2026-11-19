//! Quiz scoring owned by the ai-quiz plugin; no provider/model call.
use ai_platform_python_compat::{lower, upper};
pub use ai_platform_python_compat::{python_str, python_trim, unicode_member};
use serde_json::{Map, Value, json};

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoring_keeps_unicode_multi_answer_and_pending_short_answer_semantics() {
        let qs = vec![
            json!({"id":"one","question_num":1,"question_type":"mc_single","correct_answer":["SS"],"explanation":"fixture"}),
            json!({"id":"two","question_num":2,"question_type":"mc_multi","correct_answer":["a","b"]}),
            json!({"id":"three","question_num":3,"question_type":"true_false","correct_answer":["ΟΣ"]}),
            json!({"id":"four","question_num":4,"question_type":"short_answer","correct_answer":["reference"]}),
        ];
        let answers = serde_json::from_value(
            json!({"one":"\u{2003}ß\u{2003}","two":"B, A","three":"ος","four":"reference"}),
        )
        .unwrap();
        let result = grade(&qs, &answers);
        assert_eq!(result["correct_count"], 3);
        assert_eq!(result["total_score"], 0.75);
        assert_eq!(result["per_question"][3]["needs_ai_grading"], true);
        assert_eq!(result["per_question"][3]["correct"], false);
    }
    #[test]
    fn empty_duplicate_and_binary64_rounding_cases_remain_stable() {
        assert_eq!(rounded_score(1, 6), 0.1667);
        assert_eq!(rounded_score(1, 32), 0.0312);
        assert_eq!(rounded_score(0, 0), 0.0);
        let qs = vec![
            json!({"id":"m","question_num":1,"question_type":"mc_multi","correct_answer":["A","B"]}),
        ];
        let answers = serde_json::from_value(json!({"m":"A,A,B"})).unwrap();
        assert_eq!(grade(&qs, &answers)["correct_count"], 0);
        assert_eq!(grade(&[], &Map::new())["total_count"], 0);
    }
}
