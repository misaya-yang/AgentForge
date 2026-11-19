use super::*;
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
    time::Instant,
};
#[derive(Clone)]
pub(super) struct Fragment {
    pub location: String,
    pub text: String,
    pub assigned_secret: bool,
}
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b[A-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Z0-9](?:[A-Z0-9.-]*[A-Z0-9])?\.[A-Z]{2,63}\b",
    )
    .unwrap()
});
static SSN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([0-9]{3})-([0-9]{2})-([0-9]{4})\b").unwrap());
static CARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:[0-9][ -]?){12,18}[0-9]\b").unwrap());
static PRIVATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN (?:RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----").unwrap()
});
static CLOUD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b").unwrap());
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:api[_-]?key|access[_-]?token|client[_-]?secret|password)\s*[=:]\s*["']?([A-Za-z0-9_+/=.-]{20,256})"#).unwrap()
});
fn luhn(s: &str) -> bool {
    let d = s
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect::<Vec<_>>();
    if !(13..=19).contains(&d.len()) || d.iter().all(|v| *v == d[0]) {
        return false;
    }
    d.iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let n = if i % 2 == 1 { d * 2 } else { *d };
            u32::from(if n > 9 { n - 9 } else { n })
        })
        .sum::<u32>()
        % 10
        == 0
}
fn entropy(s: &str) -> bool {
    let mut counts = [0usize; 128];
    for b in s.bytes() {
        counts[usize::from(b)] += 1;
    }
    let n = s.len() as f64;
    let e = counts
        .into_iter()
        .filter(|c| *c > 0)
        .map(|c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum::<f64>();
    e >= 3.5
}
fn detected(d: Detector, s: &str) -> bool {
    match d {
        Detector::Email => EMAIL.is_match(s),
        Detector::UsSsn => SSN.captures_iter(s).any(|c| {
            !matches!(&c[1], "000" | "666")
                && c[1].parse::<u16>().is_ok_and(|v| v < 900)
                && &c[2] != "00"
                && &c[3] != "0000"
        }),
        Detector::PaymentCard => CARD.find_iter(s).any(|m| luhn(m.as_str())),
        Detector::PrivateKey => PRIVATE.is_match(s),
        Detector::CloudAccessKey => CLOUD.is_match(s),
        Detector::CredentialAssignment => ASSIGN.captures_iter(s).any(|c| entropy(&c[1])),
    }
}
pub(super) fn evaluate(
    p: &CompiledPolicy,
    direction: Direction,
    fragments: Vec<Fragment>,
    start: Instant,
) -> Report {
    let mut report = Report {
        schema: "agentforge.guardrail_decision/v1".into(),
        policy_pin: p.pin.clone(),
        policy_version: p.policy.version.clone(),
        direction,
        decision: Decision::Allow,
        reason: "within_policy".into(),
        stages: vec![],
        elapsed_us: 0,
        audit_state: "pending".into(),
        audit_us: None,
        holdback_us: 0,
    };
    for stage in p
        .stages
        .iter()
        .filter(|s| s.policy.directions.contains(&direction))
    {
        let stage_start = Instant::now();
        let mut findings = vec![];
        let mut matched = BTreeSet::new();
        let mut score = 0u32;
        for f in &fragments {
            if start.elapsed().as_millis() > u128::from(p.policy.budget_ms) {
                report.decision = Decision::BudgetExceeded;
                report.reason = "latency_budget".into();
                break;
            }
            for r in &stage.rules {
                if !matched.contains(&r.id)
                    && r.all.iter().all(|r| r.is_match(&f.text))
                    && r.any.iter().filter(|r| r.is_match(&f.text)).count() >= r.min_any
                {
                    score = score.saturating_add(u32::from(r.score));
                    matched.insert(r.id.clone());
                    findings.push(Finding {
                        rule: r.id.clone(),
                        location: f.location.clone(),
                    });
                }
            }
            for detector in &stage.policy.detectors {
                let id = format!("builtin:{detector:?}");
                if !matched.contains(&id)
                    && ((*detector == Detector::CredentialAssignment && f.assigned_secret)
                        || detected(*detector, &f.text))
                {
                    matched.insert(id.clone());
                    score = score.saturating_add(1);
                    findings.push(Finding {
                        rule: id,
                        location: f.location.clone(),
                    });
                }
            }
        }
        if report.decision == Decision::BudgetExceeded {
            break;
        }
        let triggered = score >= u32::from(stage.policy.threshold);
        if triggered && stage.policy.mode == Mode::Enforce {
            report.decision = Decision::Block;
            report.reason = "stage_threshold".into();
        }
        report.stages.push(StageReport {
            stage: stage.policy.stage,
            mode: stage.policy.mode,
            score,
            triggered,
            findings,
            elapsed_us: micros(stage_start),
        });
    }
    report.elapsed_us = micros(start);
    if report.elapsed_us > u64::from(p.policy.budget_ms) * 1000 {
        report.decision = Decision::BudgetExceeded;
        report.reason = "latency_budget".into();
    }
    report
}
pub(super) fn fragments(v: &Value) -> LlmResult<Vec<Fragment>> {
    fn walk(v: &Value, path: &str, out: &mut Vec<Fragment>, bytes: &mut usize) -> LlmResult<()> {
        if out.len() > 4096 {
            return Err(LlmError::new(413, "llm_guardrails_fragment_limit"));
        }
        match v {
            Value::String(s) => {
                *bytes = bytes.saturating_add(s.len());
                if *bytes > 4_194_304 {
                    return Err(LlmError::new(413, "llm_guardrails_text_limit"));
                }
                out.push(Fragment {
                    location: path.chars().take(256).collect(),
                    text: s.clone(),
                    assigned_secret: false,
                });
                if (s.starts_with('{') || s.starts_with('['))
                    && path.matches("/embedded_json").count() < 4
                {
                    if let Ok(embedded) = serde_json::from_str::<Value>(s) {
                        walk(&embedded, &format!("{path}/embedded_json"), out, bytes)?;
                    }
                }
            }
            Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    walk(v, &format!("{path}/{i}"), out, bytes)?
                }
            }
            Value::Object(m) => {
                for (i, (k, v)) in m.iter().enumerate() {
                    let field = if [
                        "messages",
                        "content",
                        "text",
                        "input",
                        "system",
                        "tools",
                        "function",
                        "arguments",
                        "choices",
                        "delta",
                        "tool_calls",
                        "output",
                        "response",
                        "reasoning",
                        "thinking",
                        "partial_json",
                    ]
                    .contains(&k.as_str())
                    {
                        k.clone()
                    } else {
                        format!("field_{i}")
                    };
                    if ![
                        "messages",
                        "content",
                        "text",
                        "input",
                        "system",
                        "tools",
                        "function",
                        "arguments",
                        "choices",
                        "delta",
                        "tool_calls",
                        "output",
                        "response",
                        "reasoning",
                        "thinking",
                        "partial_json",
                    ]
                    .contains(&k.as_str())
                    {
                        *bytes = bytes.saturating_add(k.len());
                        out.push(Fragment {
                            location: format!("{path}/key_{i}"),
                            text: k.clone(),
                            assigned_secret: false,
                        });
                    }
                    if [
                        "password",
                        "api_key",
                        "apiKey",
                        "access_token",
                        "client_secret",
                        "private_key",
                    ]
                    .contains(&k.as_str())
                    {
                        if let Some(secret) = v.as_str().filter(|s| !s.is_empty()) {
                            out.push(Fragment {
                                location: format!("{path}/field_{i}"),
                                text: secret.into(),
                                assigned_secret: true,
                            });
                        }
                    }
                    walk(v, &format!("{path}/{field}"), out, bytes)?
                }
            }
            Value::Number(n) => out.push(Fragment {
                location: path.into(),
                text: n.to_string(),
                assigned_secret: false,
            }),
            _ => {}
        }
        Ok(())
    }
    let mut out = vec![];
    walk(v, "", &mut out, &mut 0)?;
    Ok(out)
}
/// Full buffered SSE is parsed before release. Known text/argument deltas are
/// joined by logical choice/block so splitting a secret across frames is scanned.
pub(super) fn output(bytes: &[u8], sse: bool) -> LlmResult<Vec<Fragment>> {
    if !sse {
        return fragments(
            &serde_json::from_slice::<Value>(bytes)
                .map_err(|_| LlmError::new(502, "llm_guardrails_output_invalid"))?,
        );
    }
    let raw = std::str::from_utf8(bytes)
        .map_err(|_| LlmError::new(502, "llm_guardrails_output_invalid"))?
        .replace("\r\n", "\n");
    let mut out = vec![];
    let mut scalar = BTreeMap::<String, String>::new();
    let mut joined = BTreeMap::<String, String>::new();
    for event in raw.split("\n\n") {
        for line in event.lines().filter_map(|l| l.strip_prefix(':')) {
            out.push(Fragment {
                location: "/sse/comment".into(),
                text: line.into(),
                assigned_secret: false,
            });
        }
        let data = event
            .lines()
            .filter_map(|l| {
                l.strip_prefix("data:")
                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data.trim() == "[DONE]" {
            continue;
        }
        let v: Value = serde_json::from_str(&data)
            .map_err(|_| LlmError::new(502, "llm_guardrails_output_invalid"))?;
        for f in fragments(&v)? {
            if f.assigned_secret {
                out.push(f.clone());
            }
            let text = scalar.entry(f.location).or_default();
            text.push_str(&f.text);
            text.push('\n');
        }
        if let Some(choices) = v["choices"].as_array() {
            for choice in choices {
                let i = choice["index"]
                    .as_u64()
                    .ok_or(LlmError::new(502, "llm_guardrails_output_invalid"))?;
                for field in ["content", "reasoning_content", "refusal"] {
                    if let Some(t) = choice["delta"][field].as_str() {
                        joined
                            .entry(format!("choice/{i}/{field}"))
                            .or_default()
                            .push_str(t)
                    }
                }
                if let Some(parts) = choice["delta"]["content"].as_array() {
                    for part in parts {
                        if let Some(t) = part["text"].as_str() {
                            joined
                                .entry(format!("choice/{i}/content"))
                                .or_default()
                                .push_str(t)
                        }
                    }
                }
                if let Some(t) = choice["delta"]["function_call"]["arguments"].as_str() {
                    joined
                        .entry(format!("choice/{i}/function_arguments"))
                        .or_default()
                        .push_str(t)
                }
                if let Some(tools) = choice["delta"]["tool_calls"].as_array() {
                    for tool in tools {
                        if let (Some(j), Some(t)) = (
                            tool["index"].as_u64(),
                            tool["function"]["arguments"].as_str(),
                        ) {
                            joined
                                .entry(format!("choice/{i}/tool/{j}/arguments"))
                                .or_default()
                                .push_str(t)
                        }
                    }
                }
            }
        }
        let i = v["index"].as_u64().unwrap_or(0);
        for field in ["text", "thinking", "partial_json"] {
            if let Some(t) = v["delta"][field]
                .as_str()
                .or_else(|| v["content_block"][field].as_str())
            {
                joined
                    .entry(format!("block/{i}/{field}"))
                    .or_default()
                    .push_str(t)
            }
        }
        if out.len() > 4096 {
            return Err(LlmError::new(413, "llm_guardrails_fragment_limit"));
        }
    }
    out.extend(scalar.into_iter().map(|(location, text)| Fragment {
        location,
        text,
        assigned_secret: false,
    }));
    out.extend(joined.into_iter().map(|(location, text)| Fragment {
        location,
        text,
        assigned_secret: false,
    }));
    Ok(out)
}
fn micros(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}
