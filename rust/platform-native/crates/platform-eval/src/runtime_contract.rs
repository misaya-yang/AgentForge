//! Exact public V2 observation rules from the shared runtime_contract module.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
fn pystr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", repr(&Value::String(k.clone())), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(n) => n.to_string(),
    }
}
fn repr(v: &Value) -> String {
    if let Value::String(s) = v {
        let quote = if s.contains('\'') && !s.contains('"') {
            '"'
        } else {
            '\''
        };
        let escaped = s
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
            .replace(quote, &format!("\\{quote}"));
        format!("{quote}{escaped}{quote}")
    } else {
        pystr(v)
    }
}
fn identity(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| &v[*k])
        .find(|v| truth(v))
        .map(pystr)
        .unwrap_or_default()
}
fn unique(
    map: &mut BTreeMap<String, Value>,
    id: String,
    data: &Value,
    label: &str,
    failures: &mut Vec<String>,
) {
    if id.is_empty() {
        failures.push(format!("{label} lacks identity"))
    } else if map.contains_key(&id) {
        failures.push(format!("duplicate {label}: {id}"))
    } else {
        map.insert(id, data.clone());
    }
}
pub fn validate_runtime_observation(thread: &Value, events: &[Value]) -> Vec<String> {
    let mut failures = vec![];
    if thread["runtime"]["owner"] != "agent_runtime" {
        failures.push("thread.runtime.owner must be agent_runtime".into());
    }
    let (mut sequences, mut types) = (Vec::<i128>::new(), vec![]);
    let (mut starts, mut results, mut ends, mut child_starts, mut child_results) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let (mut compact, mut retained, mut output, mut root, mut terminals) = (
        false,
        Vec::<String>::new(),
        String::new(),
        String::new(),
        Vec::<(String, Value)>::new(),
    );
    for event in events {
        let seq = &event["sequence"];
        if let Some(n) = seq
            .as_i64()
            .map(i128::from)
            .or_else(|| seq.as_u64().map(i128::from))
            .or_else(|| seq.as_bool().map(|x| i128::from(x as u8)))
        {
            sequences.push(n)
        } else {
            failures.push("event.sequence must be an integer".into());
        }
        let envelope = &event["event"];
        let raw = &envelope["payload"];
        let kind = identity(raw, &["event_type"]);
        let mut data = raw["data"].as_object().cloned().unwrap_or_default();
        if !data.get("run_id").is_some_and(truth) && truth(&envelope["turn_id"]) {
            data.insert("turn_id".into(), envelope["turn_id"].clone());
        }
        let data = Value::Object(data);
        if kind.is_empty() {
            failures.push("event payload must contain event_type".into());
            continue;
        }
        types.push(kind.clone());
        if kind == "run_started" {
            let run = identity(&data, &["run_id", "turn_id"]);
            if run.is_empty() {
                failures.push("run_started lacks run identity".into())
            } else if !root.is_empty() && root != run {
                failures.push("run identity changed within observation".into())
            } else {
                root = run;
            }
        }
        if kind == "text_delta" {
            output.push_str(&identity(&data, &["content", "delta"]));
        }
        match kind.as_str() {
            "tool_call_start" | "compat/v1/tool_call_start" | "tool_call" => unique(
                &mut starts,
                identity(&data, &["call_id", "tool_call_id", "item_id"]),
                &data,
                "tool call start",
                &mut failures,
            ),
            "tool_call_result" | "compat/v1/tool_call_result" | "tool_result" => unique(
                &mut results,
                identity(&data, &["call_id", "tool_call_id", "item_id"]),
                &data,
                "tool result",
                &mut failures,
            ),
            "tool_call_end" | "compat/v1/tool_call_end" => unique(
                &mut ends,
                identity(&data, &["call_id", "tool_call_id", "item_id"]),
                &data,
                "tool call end",
                &mut failures,
            ),
            "subagent_started" | "child_turn_started" => unique(
                &mut child_starts,
                identity(&data, &["agent_id", "child_id", "thread_id"]),
                &data,
                "child start",
                &mut failures,
            ),
            "subagent_finished" | "child_turn_finished" => unique(
                &mut child_results,
                identity(&data, &["agent_id", "child_id", "thread_id"]),
                &data,
                "child result",
                &mut failures,
            ),
            "compaction_completed" | "context_compaction_completed" | "context_compaction" => {
                if kind == "context_compaction" {
                    if data["compacted"] != true {
                        failures.push("context_compaction must confirm compacted=true".into());
                    }
                } else {
                    compact = true
                }
                if let Some(ids) = data["retained_item_ids"].as_array() {
                    retained = ids.iter().filter(|v| truth(v)).map(pystr).collect();
                }
            }
            _ => {}
        }
        if [
            "run_finished",
            "run_error",
            "run_stopped",
            "run_completed",
            "cancelled",
        ]
        .contains(&kind.as_str())
        {
            terminals.push((kind, data));
        }
    }
    if sequences.windows(2).any(|p| p[0] >= p[1]) {
        failures.push("event sequences must be strictly increasing and unique".into());
    }
    if !types.iter().any(|t| t == "run_started") {
        failures.push("run_started is required".into());
    }
    if terminals.len() != 1 {
        failures.push("exactly one terminal event is required".into());
    } else {
        let (kind, data) = &terminals[0];
        let status = identity(data, &["status"]);
        let allowed = match kind.as_str() {
            "run_finished" | "run_completed" => status == "succeeded",
            "run_error" => ["failed", "cancelled"].contains(&status.as_str()),
            _ => status == "cancelled",
        };
        if !allowed {
            failures.push(format!(
                "invalid terminal status: {kind}/{}",
                if status.is_empty() {
                    "missing"
                } else {
                    &status
                }
            ));
        }
        if !root.is_empty() && identity(data, &["run_id", "turn_id"]) != root {
            failures.push("terminal run identity does not match run_started".into());
        }
    }
    let started = starts.keys().cloned().collect::<BTreeSet<_>>();
    for (label, values) in [("tool results", &results), ("tool ends", &ends)] {
        let actual = values.keys().cloned().collect::<BTreeSet<_>>();
        let missing = started.difference(&actual).cloned().collect::<Vec<_>>();
        let orphan = actual.difference(&started).cloned().collect::<Vec<_>>();
        if !missing.is_empty() {
            failures.push(format!("unpaired {label}: {}", missing.join(",")));
        }
        if !orphan.is_empty() {
            failures.push(format!("orphan {label}: {}", orphan.join(",")));
        }
    }
    if child_starts.keys().ne(child_results.keys()) {
        failures.push("child lifecycle receipts must pair exactly".into());
    }
    if compact && retained.is_empty() {
        failures.push("compaction must emit retained item ids".into());
    }
    if let Some((kind, data)) = terminals.first() {
        let stopped = ["run_stopped", "cancelled"].contains(&kind.as_str())
            || kind == "run_error" && data["status"] == "cancelled";
        if stopped && (starts.keys().ne(results.keys()) || starts.keys().ne(ends.keys())) {
            failures.push("stopped turns must close every published tool call".into());
        }
        if ["run_finished", "run_completed"].contains(&kind.as_str()) && output.is_empty() {
            failures.push("successful candidate must emit visible output".into());
        }
    }
    failures
}
pub fn assert_runtime_observation(
    thread: &Value,
    events: &[Value],
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let failures = validate_runtime_observation(thread, events);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn ev(n: Value, kind: &str, data: Value) -> Value {
        json!({"sequence":n,"event":{"turn_id":"r","payload":{"event_type":kind,"data":data}}})
    }
    #[test]
    fn public_success_and_source_scalar_coercions() {
        let t = json!({"runtime":{"owner":"agent_runtime"}});
        let e = vec![
            ev(json!(true), "run_started", json!({})),
            ev(json!(2), "text_delta", json!({"content":7})),
            ev(
                json!(3),
                "compaction_completed",
                json!({"retained_item_ids":[7]}),
            ),
            ev(json!(4), "run_completed", json!({"status":"succeeded"})),
        ];
        assert_eq!(validate_runtime_observation(&t, &e), Vec::<String>::new());
    }
    #[test]
    fn missing_contract_reports_source_order() {
        assert_eq!(
            validate_runtime_observation(&json!({}), &[]),
            vec![
                "thread.runtime.owner must be agent_runtime",
                "run_started is required",
                "exactly one terminal event is required"
            ]
        );
    }
    #[test]
    fn stopped_tool_receipts_are_not_prose() {
        let t = json!({"runtime":{"owner":"agent_runtime"}});
        let e = vec![
            ev(json!(1), "run_started", json!({})),
            ev(json!(2), "tool_call_start", json!({"call_id":"c"})),
            ev(json!(3), "run_stopped", json!({"status":"cancelled"})),
        ];
        assert_eq!(
            validate_runtime_observation(&t, &e),
            vec![
                "unpaired tool results: c",
                "unpaired tool ends: c",
                "stopped turns must close every published tool call"
            ]
        );
    }
}
