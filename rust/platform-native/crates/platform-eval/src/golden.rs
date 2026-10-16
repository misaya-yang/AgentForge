use crate::golden_validation::{case_errors, repr};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub fn name(call: &Value) -> String {
    call["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| call["tool_name"].as_str())
        .unwrap_or("")
        .trim()
        .trim_start_matches("tool:")
        .to_string()
}
pub fn arguments(call: &Value) -> Option<Value> {
    let v = call
        .get("arguments")
        .or_else(|| call.get("args"))
        .or_else(|| call.get("input_preview"))?;
    if v.is_object() {
        Some(v.clone())
    } else {
        v.as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .filter(Value::is_object)
    }
}
pub fn terminal(status: &str) -> Option<&str> {
    Some(match status {
        "budget_rejected" | "invalid_arguments" => "rejected",
        "completed" => "succeeded",
        "error" => "failed",
        "cancelled"
        | "deduplicated"
        | "denied"
        | "failed"
        | "not_executed"
        | "side_effect_unknown"
        | "succeeded"
        | "timeout" => status,
        _ => return None,
    })
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn calls(replay: &Value) -> Option<Vec<&Value>> {
    if let Some(c) = replay.get("tool_calls") {
        return c.as_array().map(|a| a.iter().collect());
    }
    if let Some(turns) = replay["turns"].as_array() {
        let mut out = vec![];
        for t in turns {
            if !t.is_object() {
                return None;
            }
            if let Some(c) = t.get("tool_calls").filter(|v| !v.is_null()) {
                for c in c.as_array()? {
                    if !c.is_object() || c["dispatch_state"] != "pending_approval" {
                        out.push(c);
                    }
                }
            }
        }
        return Some(out);
    }
    replay["spans"].as_array().map(|a| {
        a.iter()
            .filter(|s| s.is_object() && s["span_kind"] == "tool_execution")
            .collect()
    })
}
fn subset(expected: &Value, actual: &Value) -> bool {
    if let Some(m) = expected.as_object() {
        actual.is_object()
            && m.iter()
                .all(|(k, v)| actual.get(k).is_some_and(|a| subset(v, a)))
    } else {
        expected == actual
    }
}
fn strings(v: &Value) -> Vec<String> {
    if let Some(s) = v.as_str() {
        if s.trim().is_empty() {
            vec![]
        } else {
            vec![s.trim().to_string()]
        }
    } else {
        arr(v)
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
            })
            .collect()
    }
}
fn sensitive(v: &Value) -> bool {
    let text = if let Some(s) = v.as_str() {
        s.to_string()
    } else {
        v.to_string()
    };
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let patterns=PATTERNS.get_or_init(||[r#"(?i)([?&](?:key|api_key)=)[^&#\s"']+"#,r"(?i)\bauthorization\s*[:=]\s*bearer\s+[A-Za-z0-9._~+/=-]+",r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+",r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)\s*[:=]\s*["']?[^"'\s,;}]+"#,r#"(?i)(["']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)["']?\s*:\s*)["'][^"']+["']"#,r#"(?i)\b(postgres|postgresql|mysql|redis)://[^\s"']+"#,r"\bsk-[A-Za-z0-9_*.\-]{6,}",r"\bAIza[A-Za-z0-9_\-]{20,}",r"\bAQ\.[A-Za-z0-9._\-]{6,}"].into_iter().map(|p|regex::Regex::new(p).unwrap()).collect());
    let mut redacted = text.clone();
    for (pattern, replacement) in patterns.iter().zip([
        "${1}[redacted]",
        "Authorization: Bearer [redacted]",
        "Bearer [redacted]",
        "${1}=[redacted]",
        "${1}\"[redacted]\"",
        "${1}://[redacted]",
        "sk-[redacted]",
        "AIza[redacted]",
        "AQ.[redacted]",
    ]) {
        redacted = pattern.replace_all(&redacted, replacement).into_owned();
    }
    redacted != text
}
fn assertions(assertions: &[Value], replay: &Value) -> Vec<String> {
    let mut f = vec![];
    let output = replay["output_preview"].as_str().unwrap_or("");
    for a in assertions {
        if !a.is_object() {
            f.push("invalid assertion".into());
            continue;
        }
        let kind = a["type"].as_str().unwrap_or("");
        let needle = a["value"].as_str().unwrap_or("").trim();
        match kind {
            "output_equals" => {
                if needle.is_empty() || output.trim() != needle {
                    f.push("output_equals differs from the required complete response".into());
                }
            }
            "output_contains" => {
                if needle.is_empty() || !output.to_lowercase().contains(&needle.to_lowercase()) {
                    f.push(format!("output_contains missing '{needle}'"));
                }
            }
            "output_not_contains" => {
                if needle.is_empty() || output.to_lowercase().contains(&needle.to_lowercase()) {
                    f.push(format!("output_not_contains found '{needle}'"));
                }
            }
            "required_span_kind" => {
                if needle.is_empty() || !arr(&replay["span_kinds"]).contains(&json!(needle)) {
                    f.push(format!("required_span_kind missing '{needle}'"));
                }
            }
            "no_sensitive_output" => {
                if sensitive(replay) {
                    f.push("no_sensitive_output detected sensitive replay payload".into());
                }
            }
            "latency_ms_lt" | "total_tokens_lt" | "cost_cents_lt" => {
                let key = match kind {
                    "latency_ms_lt" => "total_latency_ms",
                    "total_tokens_lt" => "total_tokens",
                    _ => "total_cost_cents",
                };
                let actual = replay[key]
                    .as_f64()
                    .or_else(|| replay[key].as_str().and_then(|s| s.parse().ok()));
                let limit = a["value"].as_f64().unwrap_or(0.);
                match actual {
                    Some(actual) if actual.is_finite() && actual >= 0. => {
                        if actual >= limit {
                            f.push(format!("{kind} {actual} >= {limit}"));
                        }
                    }
                    _ => f.push(format!("{kind} requires numeric evidence")),
                }
            }
            "tool_called" | "tool_not_called" => {
                if let Some(c) = calls(replay).filter(|c| c.iter().all(|v| v.is_object())) {
                    let called = c.iter().any(|c| name(c) == needle);
                    if kind == "tool_called" && !called {
                        f.push(format!("tool_called missing '{needle}'"));
                    } else if kind == "tool_not_called" && called {
                        f.push(format!("tool_not_called observed '{needle}'"));
                    }
                } else {
                    f.push(format!(
                        "{kind} requires valid tool_calls or spans evidence"
                    ));
                }
            }
            "failure_mode_absent" => {
                let scalar = replay["failure_mode"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty());
                if !replay["failure_modes"].is_array() && scalar.is_none() {
                    f.push("failure_mode_absent missing evidence".into());
                } else if !needle.is_empty()
                    && (scalar == Some(needle)
                        || arr(&replay["failure_modes"]).contains(&json!(needle)))
                {
                    f.push(format!("failure_mode_absent observed '{needle}'"));
                }
            }
            _ => f.push(format!("unsupported assertion type '{kind}'")),
        }
    }
    f
}
fn tools(expected: &[Value], replay: &Value) -> Vec<String> {
    if expected.is_empty() {
        return vec![];
    }
    let Some(calls) = calls(replay) else {
        return vec!["expected tools require tool_calls or spans evidence".into()];
    };
    if calls.iter().any(|c| !c.is_object()) {
        return vec!["tool_calls evidence contains a non-object entry".into()];
    }
    let mut statuses: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let results = if replay.get("tool_calls").is_some() {
        arr(&replay["tool_results"]).iter().collect::<Vec<_>>()
    } else {
        arr(&replay["turns"])
            .iter()
            .flat_map(|t| arr(&t["tool_results"]))
            .collect()
    };
    for result in results {
        if let Some(id) = result["tool_call_id"].as_str().filter(|s| !s.is_empty()) {
            let s = result["status"].as_str().unwrap_or("");
            statuses
                .entry(id)
                .or_default()
                .push(terminal(s).unwrap_or(s));
        }
    }
    let mut f = vec![];
    for expected in expected {
        if !expected.is_object() {
            f.push("invalid tool expectation".into());
            continue;
        }
        let name = expected["name"].as_str().unwrap_or("").trim();
        let matched = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| self::name(c) == name)
            .map(|(i, c)| (i + 1, *c))
            .collect::<Vec<_>>();
        if expected["forbidden"] == true {
            if !matched.is_empty() {
                f.push(format!("forbidden tool called '{name}'"));
            }
            continue;
        }
        if expected["max_calls"]
            .as_u64()
            .is_some_and(|n| matched.len() as u64 > n)
        {
            f.push(format!(
                "tool '{name}' called {} > {}",
                matched.len(),
                expected["max_calls"]
            ));
        }
        let required = expected.get("required").is_none_or(|v| v == true);
        if matched.is_empty() {
            if required {
                f.push(format!("required tool not called '{name}'"));
            }
            continue;
        }
        let mut candidates = matched.clone();
        if let Some(order) = expected.get("order") {
            candidates.retain(|(i, _)| order.as_u64() == Some(*i as u64));
        }
        if let Some(status) = expected["status"].as_str() {
            let expected_status = terminal(status).unwrap_or(status);
            let mut invalid = false;
            candidates.retain(|(_, c)| {
                let id = c["call_id"]
                    .as_str()
                    .or_else(|| c["id"].as_str())
                    .or_else(|| c["tool_call_id"].as_str())
                    .unwrap_or("");
                let found = statuses.get(id).map(Vec::as_slice).unwrap_or(&[]);
                let raw = c["status"].as_str().unwrap_or("");
                let call_status = terminal(raw);
                if found.len() > 1 || found.len() == 1 && call_status.is_some_and(|s| found[0] != s)
                {
                    invalid = true;
                    return false;
                }
                let actual = found.first().copied().or(call_status).unwrap_or(raw);
                actual == expected_status
            });
            if invalid {
                f.push(format!(
                    "tool '{name}' has duplicate or conflicting status evidence"
                ));
            }
        }
        if let Some(args) = expected.get("arguments_subset") {
            candidates.retain(|(_, c)| arguments(c).is_some_and(|v| subset(args, &v)));
        }
        if candidates.is_empty() {
            f.push(format!(
                "tool '{name}' did not match expected order, status, or arguments"
            ));
        }
    }
    f
}
pub fn evaluate_case(case: &Value, observation: Option<&Value>) -> Value {
    let validation = case_errors(case);
    let replay = observation.unwrap_or(&case["metadata"]["replay"]);
    let stateful = case["expected_trajectory"]["stateful"]
        .as_object()
        .is_some_and(|m| !m.is_empty());
    let failed = |message: String| json!({"case_id":case["case_id"],"score":0.0,"passed":false,"critical":case["metadata"]["critical"]==true,"trajectory_pass":false,"stateful_pass":if stateful{json!(false)}else{Value::Null},"failures":[message]});
    if !validation.is_empty() {
        return failed(format!(
            "invalid behavior contract: {}",
            validation.join("; ")
        ));
    }
    if !replay.as_object().is_some_and(|m| !m.is_empty()) {
        return failed("missing replay observation".into());
    }
    let a = arr(&case["assertions"]);
    let mut failures = assertions(a, replay);
    let trajectory = &case["expected_trajectory"];
    let mut runtime = vec![];
    if let Some(r) = trajectory["runtime"].as_object() {
        for (k, expected) in r {
            let actual = match k.as_str() {
                "expected_exit_reason" => &replay["exit_reason"],
                "requires_sandbox_profile" => &replay["sandbox_profile"],
                _ => &replay[k],
            };
            match k.as_str() {
                "requires_gateway_decision" => {
                    if expected == true
                        && (replay["gateway_decision"].is_null()
                            || replay["gateway_decision"] == false
                            || replay["gateway_decision"] == "")
                    {
                        runtime.push("requires_gateway_decision missing gateway_decision".into());
                    }
                }
                "requires_arguments_hash" => {
                    if expected == true && replay["arguments_hash_present"] != true {
                        runtime
                            .push("requires_arguments_hash missing arguments_hash_present".into());
                    }
                }
                _ => {
                    if actual != expected {
                        runtime.push(format!(
                            "{k} expected {}, got {}",
                            repr(expected),
                            repr(actual)
                        ));
                    }
                }
            }
        }
    }
    let sf = crate::golden_stateful::evaluate(case, replay);
    let tf = tools(arr(&trajectory["tools"]), replay);
    let missing = strings(&trajectory["required_span_kinds"])
        .into_iter()
        .filter(|s| !arr(&replay["span_kinds"]).contains(&json!(s)))
        .collect::<Vec<_>>();
    let ta = a
        .iter()
        .filter(|a| {
            ["required_span_kind", "tool_called", "tool_not_called"]
                .contains(&a["type"].as_str().unwrap_or(""))
        })
        .cloned()
        .collect::<Vec<_>>();
    let trajectory_pass = missing.is_empty()
        && runtime.is_empty()
        && sf.is_empty()
        && tf.is_empty()
        && assertions(&ta, replay).is_empty();
    let stateful_pass = sf.is_empty();
    let adapter = &replay["adapter_evidence"];
    if adapter["source"] == "canonical_assistant_producer"
        && adapter["integrity"]["status"] != "verified"
    {
        failures.push(format!(
            "canonical producer adapter integrity is {}",
            adapter["integrity"]["status"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown")
        ));
    }
    failures.extend(runtime);
    failures.extend(sf);
    failures.extend(tf);
    let status = replay["status"].as_str().unwrap_or("");
    if status.is_empty() {
        failures.push("missing replay status".into());
    } else if status != "succeeded" {
        failures.push(format!("status={status}"));
    }
    let output = replay["output_preview"]
        .as_str()
        .unwrap_or("")
        .to_lowercase();
    let mut required = strings(&case["expected_output"]["contains"]);
    if required.is_empty() {
        required = strings(&case["expected_output"]["output_preview"]);
    }
    for text in required {
        if !output.contains(&text.to_lowercase()) {
            failures.push(format!("missing expected text '{text}'"));
        }
    }
    for text in strings(&case["expected_output"]["not_contains"]) {
        if output.contains(&text.to_lowercase()) {
            failures.push(format!("found forbidden text '{text}'"));
        }
    }
    if !missing.is_empty() {
        failures.push(format!("missing spans {}", missing.join(",")));
    }
    if sensitive(replay) {
        failures.push("sensitive replay payload".into());
    }
    json!({"case_id":case["case_id"],"score":if failures.is_empty(){1.0}else{0.0},"passed":failures.is_empty(),"critical":case["metadata"]["critical"]==true,"trajectory_pass":trajectory_pass,"stateful_pass":if stateful{json!(stateful_pass)}else{Value::Null},"failures":failures})
}
pub fn evaluate_cases(cases: &[Value], observations: Option<&BTreeMap<String, Value>>) -> Value {
    let empty = json!({});
    let results = cases
        .iter()
        .map(|c| {
            evaluate_case(
                c,
                observations.map(|obs| {
                    obs.get(c["case_id"].as_str().unwrap_or(""))
                        .unwrap_or(&empty)
                }),
            )
        })
        .collect::<Vec<_>>();
    let total = results.len();
    let failed = results.iter().filter(|c| c["passed"] != true).count();
    let trajectory_failed = results
        .iter()
        .filter(|c| c["trajectory_pass"] != true)
        .count();
    let critical = results
        .iter()
        .filter(|c| c["critical"] == true)
        .collect::<Vec<_>>();
    let critical_failed = critical.iter().filter(|c| c["passed"] != true).count();
    let stateful = results
        .iter()
        .filter(|c| !c["stateful_pass"].is_null())
        .collect::<Vec<_>>();
    let stateful_failed = stateful
        .iter()
        .filter(|c| c["stateful_pass"] != true)
        .count();
    json!({"schema_version":"eval-gate-metrics/v2","case_count":total,"score_sum":(total-failed)as f64,"failed_case_count":failed,"overall_score":(total-failed)as f64/total.max(1)as f64,"pass_rate":(total-failed)as f64/total.max(1)as f64,"trajectory_case_count":total,"trajectory_failed_count":trajectory_failed,"trajectory_pass_rate":(total-trajectory_failed)as f64/total.max(1)as f64,"critical_case_count":critical.len(),"critical_failed_count":critical_failed,"critical_pass_rate":if critical.is_empty(){None}else{Some((critical.len()-critical_failed)as f64/critical.len()as f64)},"stateful_case_count":stateful.len(),"stateful_failed_count":stateful_failed,"stateful_pass_rate":if stateful.is_empty(){None}else{Some((stateful.len()-stateful_failed)as f64/stateful.len()as f64)},"cases":results})
}
pub fn summarize(cases: &[Value]) -> Value {
    let mut splits = BTreeMap::<String, usize>::new();
    let mut difficulties = BTreeMap::<String, usize>::new();
    let mut tags = BTreeMap::<String, usize>::new();
    let mut critical = 0;
    for c in cases {
        *splits
            .entry(
                c["split"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("regression")
                    .into(),
            )
            .or_default() += 1;
        *difficulties
            .entry(
                c["metadata"]["difficulty"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("unspecified")
                    .into(),
            )
            .or_default() += 1;
        if c["metadata"]["critical"] == true {
            critical += 1;
        }
        for t in arr(&c["metadata"]["tags"]) {
            if let Some(t) = t.as_str().filter(|s| !s.is_empty()) {
                *tags.entry(t.into()).or_default() += 1;
            }
        }
    }
    json!({"case_count":cases.len(),"critical_count":critical,"by_split":splits,"by_difficulty":difficulties,"tags":tags})
}
