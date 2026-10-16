//! Evidence-preserving projection of recorded Assistant producer artifacts.
//! Missing, contradictory or unbound receipts never manufacture verified claims.
use crate::{
    Result,
    canonical::{canonical, digest, sha256},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const PRODUCER_EVIDENCE_SCHEMA_VERSION: &str = "assistant-producer-evidence/v1";
pub const ADAPTER_SCHEMA_VERSION: &str = "assistant-producer-observation/v1";
const BUDGET_SCHEMA: &str = "assistant-run-budget/v1";
const TURN_SCHEMA: &str = "assistant-turn-contract/v1";
const COMPONENTS: &[&str] = &[
    "binding",
    "plan",
    "tool_pairing",
    "budget",
    "hitl",
    "compaction",
    "security",
];
const CONTROLS: &[&str] = &[
    "_approval_id",
    "_middleware_approval_required",
    "_steer_payload",
];
const TERMINAL: &[&str] = &[
    "approval_required",
    "run_error",
    "run_finished",
    "side_effect_unknown",
];
const BOUND: &[&str] = &[
    "approval_required",
    "run_error",
    "run_finished",
    "side_effect_unknown",
    "approval_result",
    "run_budget_exceeded",
    "tool_call_end",
    "tool_call_result",
    "tool_call_start",
];
const SUCCESS: &[&str] = &["completed", "deduplicated", "succeeded"];
const TOOL_STATUS: &[&str] = &[
    "budget_rejected",
    "cancelled",
    "completed",
    "deduplicated",
    "denied",
    "error",
    "failed",
    "invalid_arguments",
    "not_executed",
    "side_effect_unknown",
    "succeeded",
    "timeout",
];
const LIMITS: &[&str] = &[
    "max_model_turns",
    "max_tool_calls",
    "max_parallel_tool_calls",
    "max_wall_time_seconds",
    "max_tool_result_bytes",
];
const USAGE: &[&str] = &[
    "model_turns",
    "tool_calls",
    "tool_result_bytes",
    "elapsed_ms",
];
const REMAINING: &[&str] = &[
    "model_turns",
    "tool_calls",
    "tool_result_bytes",
    "wall_time_ms",
];
#[derive(Clone)]
struct Event {
    turn: u64,
    index: usize,
    kind: String,
    data: Value,
}
impl Event {
    fn position(&self) -> (u64, usize) {
        (self.turn, self.index)
    }
}
struct Turn {
    index: u64,
    events: Vec<Event>,
    envelope: Value,
    snapshot: Value,
    checkpoint: Value,
}
fn text(v: &Value) -> Option<&str> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty())
}
fn obj(v: &Value) -> bool {
    v.as_object().is_some_and(|o| !o.is_empty())
}
fn object(v: &Value) -> Value {
    if v.is_object() { v.clone() } else { json!({}) }
}
fn integer(v: &Value) -> bool {
    v.as_u64().is_some()
}
fn positive(v: &Value) -> bool {
    v.as_u64().is_some_and(|n| n > 0)
}
fn number(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}
fn keys(v: &Value, expected: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == expected.len() && expected.iter().all(|k| o.contains_key(*k)))
}
fn component(status: &str, reason: impl Into<String>) -> Value {
    json!({"status":status,"reason":reason.into()})
}
fn finish(errors: Vec<String>, success: &str) -> Value {
    component(
        if errors.is_empty() {
            "verified"
        } else {
            "unknown"
        },
        if errors.is_empty() {
            success.into()
        } else {
            errors.join("; ")
        },
    )
}
fn push_array(target: &mut Value, key: &str, value: Value) {
    if !target[key].is_array() {
        target[key] = json!([])
    }
    target[key].as_array_mut().unwrap().push(value)
}
fn identity(v: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for k in ["run_id", "tenant_id"] {
        if let Some(s) = text(&v[k]) {
            out.insert(k.into(), s.into());
        }
    }
    let (s, t) = (text(&v["session_id"]), text(&v["thread_id"]));
    match (s, t) {
        (Some(s), Some(t)) if s != t => {
            out.insert("session_conflict".into(), format!("{s}!={t}"));
        }
        (Some(s), _) | (_, Some(s)) => {
            out.insert("session_id".into(), s.into());
        }
        _ => {}
    }
    out
}
fn coalesce(values: Vec<Value>, label: &str, errors: &mut Vec<String>) -> Value {
    let values: Vec<_> = values.into_iter().filter(obj).collect();
    if values.is_empty() {
        return Value::Null;
    }
    if values.iter().any(|v| canonical(v) != canonical(&values[0])) {
        errors.push(format!("conflicting {label} receipts"));
        Value::Null
    } else {
        values[0].clone()
    }
}
fn normalize(producer: &Value) -> (Vec<Turn>, Vec<String>) {
    let Some(raw) = producer["turns"].as_array() else {
        return (vec![], vec!["producer turns must be a list".into()]);
    };
    if raw.is_empty() {
        return (
            vec![],
            vec!["producer requires at least one canonical turn".into()],
        );
    }
    let mut turns = vec![];
    let mut errors: Vec<String> = vec![];
    let mut seen = BTreeSet::new();
    for (row, raw) in raw.iter().enumerate() {
        if !raw.is_object() {
            errors.push(format!("turn row {} must be an object", row + 1));
            continue;
        }
        let Some(index) = raw["turn_index"]
            .as_u64()
            .filter(|n| *n > 0 && seen.insert(*n))
        else {
            errors.push(format!(
                "turn row {} has invalid or duplicate turn_index",
                row + 1
            ));
            continue;
        };
        let mut events = vec![];
        let mut terminals = vec![];
        if let Some(raw_events) = raw["events"].as_array() {
            for (i, event) in raw_events.iter().enumerate() {
                let i = i + 1;
                if !event.is_object() {
                    errors.push(format!("turn {index} event {i} must be an object"));
                    continue;
                }
                let Some(kind) = text(&event["event_type"]) else {
                    errors.push(format!("turn {index} event {i} has no event_type"));
                    continue;
                };
                if !terminals.is_empty() {
                    errors.push(format!(
                        "turn {index} emitted {kind} after a terminal event"
                    ));
                }
                if BOUND.contains(&kind) && !event["data"].is_object() {
                    errors.push(format!("turn {index} event {i} requires an object payload"));
                }
                let e = Event {
                    turn: index,
                    index: i,
                    kind: kind.into(),
                    data: object(&event["data"]),
                };
                if TERMINAL.contains(&kind) {
                    terminals.push(e.clone())
                }
                events.push(e)
            }
        } else {
            errors.push(format!("turn {index} events must be a list"));
        }
        let mut candidates = vec![raw["terminal_envelope"].clone()];
        candidates.extend(events.iter().map(|e| e.data["terminal_envelope"].clone()));
        let envelope = coalesce(
            candidates,
            &format!("turn {index} terminal envelope"),
            &mut errors,
        );
        if terminals.len() != 1 {
            errors.push(format!("turn {index} requires exactly one terminal event"));
        } else if envelope.is_null() {
            errors.push(format!(
                "turn {index} terminal event has no terminal envelope"
            ));
        } else {
            let e = &terminals[0];
            if !obj(&e.data["terminal_envelope"]) {
                errors.push(format!(
                    "turn {index} terminal event has no terminal envelope payload"
                ));
            } else if canonical(&e.data["terminal_envelope"]) != canonical(&envelope) {
                errors.push(format!(
                    "turn {index} terminal event conflicts with terminal envelope"
                ));
            }
            let status = text(&envelope["status"]);
            let reason = text(&envelope["exit_reason"]);
            let valid = match e.kind.as_str() {
                "approval_required" => {
                    status == Some("blocked") && reason == Some("approval_pending")
                }
                "run_error" => {
                    (status == Some("cancelled") && reason == Some("cancelled"))
                        || (status == Some("failed")
                            && reason.is_some_and(|r| {
                                ![
                                    "approval_pending",
                                    "cancelled",
                                    "side_effect_unknown",
                                    "succeeded",
                                ]
                                .contains(&r)
                            }))
                }
                "run_finished" => {
                    status == Some("succeeded")
                        && reason.is_some_and(|r| ["max_iterations", "succeeded"].contains(&r))
                }
                "side_effect_unknown" => {
                    status == Some("blocked") && reason == Some("side_effect_unknown")
                }
                _ => false,
            };
            if !valid {
                errors.push(format!(
                    "turn {index} terminal type conflicts with envelope status/exit_reason"
                ));
            }
            if envelope["schema_version"] != TURN_SCHEMA {
                errors.push(format!(
                    "turn {index} terminal envelope requires schema {TURN_SCHEMA}"
                ));
            }
        }
        let mut candidates = vec![raw["context_snapshot"].clone()];
        candidates.extend(events.iter().map(|e| e.data["context_snapshot"].clone()));
        candidates.push(envelope["context_snapshot"].clone());
        let snapshot = coalesce(
            candidates,
            &format!("turn {index} context snapshot"),
            &mut errors,
        );
        if !snapshot.is_null() && snapshot["schema_version"] != TURN_SCHEMA {
            errors.push(format!(
                "turn {index} context snapshot requires schema {TURN_SCHEMA}"
            ));
        }
        let checkpoint = if raw["checkpoint"].is_object() {
            raw["checkpoint"].clone()
        } else {
            if !raw["checkpoint"].is_null() {
                errors.push(format!("turn {index} checkpoint must be an object"));
            }
            Value::Null
        };
        turns.push(Turn {
            index,
            events,
            envelope,
            snapshot,
            checkpoint,
        });
    }
    turns.sort_by_key(|t| t.index);
    if turns
        .iter()
        .enumerate()
        .any(|(i, t)| t.index != i as u64 + 1)
    {
        errors.push("turn_index values must be contiguous and one-based".into());
    }
    (turns, errors)
}
fn binding(trace: &Value, turns: &[Turn]) -> Vec<String> {
    let mut errors: Vec<String> = vec![];
    let expected = identity(trace);
    let complete = |v: &BTreeMap<String, String>| {
        v.len() == 3
            && ["run_id", "tenant_id", "session_id"]
                .iter()
                .all(|k| v.contains_key(*k))
    };
    if !complete(&expected) {
        errors.push("trace must bind one run_id, tenant_id, and session_id".into());
    }
    for t in turns {
        for (label, v) in [
            ("terminal envelope", &t.envelope),
            ("context snapshot", &t.snapshot),
            ("checkpoint", &t.checkpoint),
        ] {
            if !v.is_object() {
                continue;
            }
            let id = identity(v);
            if id.contains_key("session_conflict") {
                errors.push(format!(
                    "turn {} {label} has conflicting session/thread identity",
                    t.index
                ));
            }
            if !complete(&id) {
                errors.push(format!(
                    "turn {} {label} must independently bind run_id, tenant_id, and session_id",
                    t.index
                ));
            }
            for key in ["run_id", "tenant_id", "session_id"] {
                if id
                    .get(key)
                    .zip(expected.get(key))
                    .is_some_and(|(a, b)| a != b)
                {
                    errors.push(format!("turn {} {label} crosses {key}", t.index));
                }
            }
        }
        for e in &t.events {
            let id = identity(&e.data);
            if BOUND.contains(&e.kind.as_str())
                && !["run_id", "session_id"].iter().all(|k| id.contains_key(*k))
            {
                errors.push(format!(
                    "turn {} event {} must bind run_id and session_id",
                    t.index, e.index
                ));
            }
            if id.contains_key("session_conflict") {
                errors.push(format!(
                    "turn {} event {} has conflicting session",
                    t.index, e.index
                ));
            }
            for key in ["run_id", "tenant_id", "session_id"] {
                if id
                    .get(key)
                    .zip(expected.get(key))
                    .is_some_and(|(a, b)| a != b)
                {
                    errors.push(format!("turn {} event {} crosses {key}", t.index, e.index));
                }
            }
        }
    }
    errors
}
fn snapshot_errors(s: &Value, label: &str) -> Vec<String> {
    let mut e: Vec<String> = vec![];
    if !keys(
        s,
        &[
            "schema_version",
            "limits",
            "usage",
            "remaining",
            "exhausted",
            "reason",
        ],
    ) {
        return vec![format!("{label} has invalid snapshot fields")];
    }
    if s["schema_version"] != BUDGET_SCHEMA {
        e.push(format!("{label} has an unsupported schema"))
    }
    for (k, ks, msg) in [
        ("limits", LIMITS, "limit"),
        ("usage", USAGE, "usage"),
        ("remaining", REMAINING, "remaining"),
    ] {
        if !keys(&s[k], ks) {
            e.push(format!("{label} has invalid {msg} fields"))
        }
    }
    if !e.is_empty() {
        return e;
    }
    let (l, u, r) = (&s["limits"], &s["usage"], &s["remaining"]);
    for k in [
        "max_model_turns",
        "max_tool_calls",
        "max_parallel_tool_calls",
        "max_tool_result_bytes",
    ] {
        if !positive(&l[k]) {
            e.push(format!("{label} limit {k} must be a positive integer"))
        }
    }
    if !number(&l["max_wall_time_seconds"]).is_finite()
        || number(&l["max_wall_time_seconds"]) <= 0.0
    {
        e.push(format!(
            "{label} limit max_wall_time_seconds must be positive and finite"
        ))
    }
    for (k, v) in [("usage", u), ("remaining", r)] {
        let ks = if k == "usage" { USAGE } else { REMAINING };
        for key in ks {
            if !integer(&v[key]) {
                e.push(format!("{label} {k} {key} must be a non-negative integer"))
            }
        }
    }
    let reason = text(&s["reason"]);
    match s["exhausted"].as_bool() {
        None => e.push(format!("{label} exhausted must be boolean")),
        Some(true) => {
            if !reason.is_some_and(|r| {
                [
                    "model_turns_exhausted",
                    "tool_calls_exhausted",
                    "parallel_tool_calls_exhausted",
                    "wall_time_exhausted",
                    "tool_result_bytes_exhausted",
                ]
                .contains(&r)
            }) {
                e.push(format!("{label} has an unsupported exhaustion reason"))
            }
        }
        Some(false) => {
            if !s["reason"].is_null() {
                e.push(format!(
                    "{label} non-exhausted snapshot must not have a reason"
                ))
            }
        }
    }
    if !e.is_empty() {
        return e;
    }
    let wall = (number(&l["max_wall_time_seconds"]) * 1000.0).ceil();
    let mut bad = false;
    for (usage, limit, remaining) in [
        ("model_turns", "max_model_turns", "model_turns"),
        ("tool_calls", "max_tool_calls", "tool_calls"),
        (
            "tool_result_bytes",
            "max_tool_result_bytes",
            "tool_result_bytes",
        ),
    ] {
        let used = u[usage].as_u64().unwrap();
        let maximum = l[limit].as_u64().unwrap();
        if r[remaining].as_u64() != Some(maximum.saturating_sub(used)) {
            bad = true
        }
    }
    if number(&r["wall_time_ms"]) != (wall - number(&u["elapsed_ms"])).max(0.0) {
        bad = true
    }
    if bad {
        e.push(format!("{label} remaining counters are inconsistent"))
    }
    for (usage, limit) in [
        ("model_turns", "max_model_turns"),
        ("tool_calls", "max_tool_calls"),
        ("tool_result_bytes", "max_tool_result_bytes"),
    ] {
        if number(&u[usage]) > number(&l[limit]) {
            e.push(format!("{label} usage {usage} exceeds its limit"))
        }
    }
    if s["exhausted"] == true
        && reason == Some("model_turns_exhausted")
        && u["model_turns"] != l["max_model_turns"]
    {
        e.push(format!("{label} model-turn exhaustion is inconsistent"))
    }
    if s["exhausted"] == true
        && reason == Some("wall_time_exhausted")
        && number(&u["elapsed_ms"]) < wall
    {
        e.push(format!("{label} wall-time exhaustion is inconsistent"))
    }
    if s["exhausted"] == false && number(&u["elapsed_ms"]) > wall {
        e.push(format!("{label} elapsed wall time exceeds its limit"))
    }
    e
}
fn exhaustion_errors(d: &Value, s: &Value, label: &str) -> Vec<String> {
    let mut e: Vec<String> = vec![];
    let dimension = text(&d["dimension"]).unwrap_or("");
    let limit_key = match dimension {
        "model_turns" => "max_model_turns",
        "tool_calls" => "max_tool_calls",
        "parallel_tool_calls" => "max_parallel_tool_calls",
        "wall_time" => "max_wall_time_seconds",
        "tool_result_bytes" => "max_tool_result_bytes",
        _ => return vec![format!("{label} has an unsupported exhaustion dimension")],
    };
    let reason = format!("{dimension}_exhausted");
    if text(&d["reason"]) != Some(reason.as_str()) || s["reason"] != reason {
        e.push(format!(
            "{label} exhaustion dimension/reason is inconsistent"
        ))
    }
    if !s["limits"].is_object() || !s["usage"].is_object() {
        e.push(format!("{label} has no valid bound budget snapshot"));
        return e;
    }
    if dimension != "wall_time" {
        if ["limit", "used", "requested"]
            .iter()
            .any(|k| !integer(&d[k]))
        {
            e.push(format!(
                "{label} limit/used/requested must be non-negative integers"
            ));
            return e;
        }
    } else if ["limit", "used", "requested"]
        .iter()
        .any(|k| !number(&d[k]).is_finite() || number(&d[k]) < 0.0)
    {
        e.push(format!(
            "{label} wall-time receipt must use finite non-negative numbers"
        ));
        return e;
    }
    let (limit, used, requested) = (
        number(&d["limit"]),
        number(&d["used"]),
        number(&d["requested"]),
    );
    if limit != number(&s["limits"][limit_key]) {
        e.push(format!(
            "{label} exhaustion limit does not match the budget snapshot"
        ))
    }
    let (bad, name) = match dimension {
        "model_turns" => (
            used != number(&s["usage"][dimension]) || requested != used + 1.0 || requested <= limit,
            "model-turn",
        ),
        "tool_calls" => (
            used != number(&s["usage"][dimension]) || requested < used || requested <= limit,
            "tool-call",
        ),
        "tool_result_bytes" => (
            used != number(&s["usage"][dimension]) || requested < used || requested <= limit,
            "tool-result",
        ),
        "parallel_tool_calls" => (used != 0.0 || requested <= limit, "parallel-tool"),
        _ => (
            used != requested
                || requested < limit
                || !integer(&s["usage"]["elapsed_ms"])
                || number(&s["usage"]["elapsed_ms"]) < (used * 1000.0).ceil(),
            "wall-time",
        ),
    };
    if bad {
        e.push(format!(
            "{label} {name} exhaustion arithmetic is inconsistent"
        ))
    }
    e
}
fn budget_projection(turns: &[Turn], replay: &mut BTreeMap<u64, Value>, bound: bool) -> Value {
    let mut errors: Vec<String> = vec![];
    let mut previous: Option<&Value> = None;
    if turns.is_empty() {
        errors.push("no turns were recorded".into())
    }
    for t in turns {
        let i = t.index;
        let mut snapshots = vec![];
        let start = &t.snapshot["bootstrap"]["run_budget"];
        if start.is_object() {
            snapshots.push(("start", start))
        }
        let exceeded: Vec<_> = t
            .events
            .iter()
            .filter(|e| e.kind == "run_budget_exceeded")
            .collect();
        if exceeded.len() > 1 {
            errors.push(format!("turn {i} has duplicate budget exhaustion receipts"))
        }
        for e in &exceeded {
            let b = &e.data["budget"];
            if !b.is_object() {
                errors.push(format!(
                    "turn {i} budget exhaustion receipt has no budget snapshot"
                ));
                continue;
            }
            snapshots.push(("exceeded", b));
            let errs = snapshot_errors(b, &format!("turn {i} exceeded budget snapshot"));
            let valid = errs.is_empty();
            errors.extend(errs);
            let reason = text(&e.data["reason"]);
            if e.data["schema_version"] != BUDGET_SCHEMA
                || e.data["status"] != "exhausted"
                || reason.is_none()
                || b["exhausted"] != true
                || text(&b["reason"]) != reason
            {
                errors.push(format!(
                    "turn {i} has inconsistent budget exhaustion receipt"
                ))
            }
            if t.envelope["status"] != "failed"
                || t.envelope["exit_reason"] != "run_budget_exceeded"
            {
                errors.push(format!(
                    "turn {i} budget exhaustion conflicts with terminal envelope"
                ))
            }
            if valid {
                errors.extend(exhaustion_errors(
                    &e.data,
                    b,
                    &format!("turn {i} budget exhaustion receipt"),
                ))
            }
        }
        let terminal = &t.envelope["usage"]["run_budget"];
        if terminal.is_object() {
            snapshots.push(("terminal", terminal))
        }
        if !snapshots.iter().any(|(k, _)| *k == "start")
            || !snapshots
                .iter()
                .any(|(k, _)| ["exceeded", "terminal"].contains(k))
        {
            errors.push(format!(
                "turn {i} budget requires start and terminal/exceeded receipts"
            ))
        }
        let Some((_, budget)) = snapshots.last().copied() else {
            errors.push(format!("turn {i} has no run budget snapshot"));
            continue;
        };
        if let Some(e) = exceeded.first() {
            if budget["exhausted"] != true || text(&budget["reason"]) != text(&e.data["reason"]) {
                errors.push(format!(
                    "turn {i} terminal budget snapshot lost exhaustion state"
                ))
            }
        }
        if text(&t.envelope["exit_reason"]) == Some("run_budget_exceeded") && exceeded.len() != 1 {
            errors.push(format!(
                "turn {i} run_budget_exceeded terminal requires one exhaustion event"
            ))
        }
        for (n, (_, s)) in snapshots.iter().enumerate() {
            let errs = snapshot_errors(s, &format!("turn {i} run budget snapshot {}", n + 1));
            let valid = errs.is_empty();
            errors.extend(errs);
            if !valid {
                continue;
            }
            if let Some(p) = previous {
                if s["limits"] != p["limits"] {
                    errors.push("run budget limits changed across recorded snapshots".into())
                }
                for k in USAGE {
                    if number(&s["usage"][k]) < number(&p["usage"][k]) {
                        errors.push(format!("run budget usage {k} is not monotonic"))
                    }
                }
                for k in REMAINING {
                    if number(&s["remaining"][k]) > number(&p["remaining"][k]) {
                        errors.push(format!("run budget remaining {k} is not monotonic"))
                    }
                }
                if p["exhausted"] == true && s["exhausted"] != true {
                    errors.push("run budget exhaustion regressed to non-exhausted".into())
                }
            }
            previous = Some(s)
        }
        if !snapshot_errors(budget, &format!("turn {i} terminal budget snapshot")).is_empty() {
            continue;
        }
        let maximum = budget["limits"]["max_model_turns"].as_u64().unwrap();
        let iteration = budget["usage"]["model_turns"].as_u64().unwrap();
        let remaining = budget["remaining"]["model_turns"].as_u64().unwrap();
        if iteration == 0 || maximum.checked_sub(iteration) != Some(remaining) {
            errors.push(format!(
                "turn {i} has inconsistent model-turn budget evidence"
            ));
            continue;
        }
        replay.get_mut(&i).unwrap()["budget"] =
            json!({"iteration":iteration,"max_iterations":maximum,"remaining":remaining});
    }
    if !bound {
        errors.push("producer identity binding is not verified".into())
    }
    let mut seen = BTreeSet::new();
    errors.retain(|s| seen.insert(s.clone()));
    finish(errors, "canonical run_budget snapshots")
}
fn tool_name(v: &Value) -> Option<&str> {
    if truthy(&v["tool_name"]) {
        text(&v["tool_name"])
    } else {
        text(&v["name"])
    }
}
fn status(v: &Value) -> Option<&str> {
    text(v).filter(|s| TOOL_STATUS.contains(s))
}
fn tool_projection(
    turns: &[Turn],
    replay: &mut BTreeMap<u64, Value>,
    bound: bool,
) -> (Value, BTreeMap<String, Value>, BTreeMap<String, Event>) {
    let (mut starts, mut results, mut ends) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let mut order = vec![];
    let mut errors: Vec<String> = vec![];
    for e in turns.iter().flat_map(|t| &t.events) {
        if !["tool_call_start", "tool_call_result", "tool_call_end"].contains(&e.kind.as_str()) {
            continue;
        }
        let Some(id) = text(&e.data["tool_call_id"]) else {
            errors.push(format!("{} missing tool_call_id", e.kind));
            continue;
        };
        let target = match e.kind.as_str() {
            "tool_call_start" => &mut starts,
            "tool_call_result" => &mut results,
            _ => &mut ends,
        };
        if target.contains_key(id) {
            errors.push(format!("duplicate {} for {id}", e.kind));
            continue;
        }
        target.insert(id.to_owned(), e.clone());
        if e.kind == "tool_call_start" {
            order.push(id.to_owned())
        }
    }
    if starts.is_empty() {
        errors.push("no canonical tool lifecycle was recorded".into())
    }
    if starts.keys().ne(results.keys()) || starts.keys().ne(ends.keys()) {
        errors.push("tool lifecycle start/result/end IDs are not one-to-one".into())
    }
    let mut projected = BTreeMap::new();
    for id in order {
        let start = &starts[&id];
        let (result, end) = (results.get(&id), ends.get(&id));
        let args = &start.data["arguments"];
        let name = tool_name(&start.data);
        if name.is_none() || !args.is_object() {
            errors.push(format!(
                "tool_call_start {id} lacks name or object arguments"
            ));
            continue;
        }
        let name = name.unwrap();
        if text(&start.data["tool_name"])
            .zip(text(&start.data["name"]))
            .is_some_and(|(a, b)| a != b)
        {
            errors.push(format!("tool lifecycle {id} has conflicting start names"))
        }
        let mut controls: Vec<_> = CONTROLS
            .iter()
            .copied()
            .filter(|k| args.get(*k).is_some())
            .collect();
        controls.sort();
        if !controls.is_empty() {
            errors.push(format!(
                "tool lifecycle {id} exposes control arguments: {}",
                controls.join(", ")
            ))
        }
        let (Some(result), Some(end)) = (result, end) else {
            continue;
        };
        if start.turn != result.turn || start.turn != end.turn {
            errors.push(format!(
                "tool lifecycle {id} crosses a terminal turn boundary"
            ))
        }
        if !(start.position() < result.position() && result.position() < end.position()) {
            errors.push(format!("tool lifecycle {id} is out of order"))
        }
        let (rs, es) = (status(&result.data["status"]), status(&end.data["status"]));
        if rs.is_none() || es.is_none() || rs != es {
            errors.push(format!(
                "tool lifecycle {id} has conflicting terminal status"
            ))
        }
        for terminal in [result, end] {
            if text(&terminal.data["tool_name"])
                .zip(text(&terminal.data["name"]))
                .is_some_and(|(a, b)| a != b)
            {
                errors.push(format!(
                    "tool lifecycle {id} has conflicting terminal names"
                ))
            }
            match tool_name(&terminal.data) {
                None => errors.push(format!(
                    "tool lifecycle {id} terminal receipt lacks tool name"
                )),
                Some(n) if n != name => {
                    errors.push(format!("tool lifecycle {id} changes tool name"))
                }
                _ => {}
            }
            match terminal.data["success"].as_bool() {
                None => errors.push(format!(
                    "tool lifecycle {id} terminal receipt lacks success"
                )),
                Some(s)
                    if status(&terminal.data["status"]).is_some_and(|s| SUCCESS.contains(&s))
                        != s =>
                {
                    errors.push(format!(
                        "tool lifecycle {id} terminal status conflicts with success"
                    ))
                }
                _ => {}
            }
        }
        if result.data["success"]
            .as_bool()
            .zip(end.data["success"].as_bool())
            .is_some_and(|(a, b)| a != b)
        {
            errors.push(format!(
                "tool lifecycle {id} has conflicting success receipts"
            ))
        }
        let call = json!({"call_id":id,"name":name,"arguments":args,"dispatch_state":"dispatched"});
        push_array(
            replay.get_mut(&start.turn).unwrap(),
            "tool_calls",
            call.clone(),
        );
        push_array(
            replay.get_mut(&result.turn).unwrap(),
            "tool_results",
            json!({"tool_call_id":id,"status":result.data["status"]}),
        );
        projected.insert(id, call);
    }
    if !bound {
        errors.push("producer identity binding is not verified".into())
    }
    let mut c = finish(errors, "canonical start/result/end lifecycle");
    c["call_count"] = json!(starts.len());
    (c, projected, results)
}
fn spaced_json(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| format!("{}: {}", canonical(&json!(k)), spaced_json(&o[k])))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(spaced_json).collect::<Vec<_>>().join(", ")
        ),
        _ => canonical(v),
    }
}
fn hitl_projection(
    turns: &[Turn],
    replay: &mut BTreeMap<u64, Value>,
    starts: &BTreeMap<String, Value>,
    results: &BTreeMap<String, Event>,
    bound: bool,
) -> Value {
    let pauses: Vec<_> = turns
        .iter()
        .flat_map(|t| &t.events)
        .filter(|e| e.kind == "approval_required")
        .collect();
    let resumes: Vec<_> = turns
        .iter()
        .flat_map(|t| &t.events)
        .filter(|e| e.kind == "approval_result")
        .collect();
    let checkpoints: Vec<_> = turns
        .iter()
        .filter(|t| t.checkpoint["phase"] == "approval_pending")
        .collect();
    let resumed: Vec<_> = turns
        .iter()
        .filter(|t| t.checkpoint["phase"] == "tool_call_pending")
        .collect();
    if [
        pauses.len(),
        resumes.len(),
        checkpoints.len(),
        resumed.len(),
    ] != [1, 1, 1, 1]
    {
        return component(
            "unknown",
            "HITL requires one pause, one approval result, one pause checkpoint, and one resume checkpoint",
        );
    }
    let (pause, resume, checkpoint, resumed) = (pauses[0], resumes[0], checkpoints[0], resumed[0]);
    let c = &checkpoint.checkpoint;
    let rc = &resumed.checkpoint;
    let mut errors: Vec<String> = vec![];
    let (cid, aid) = (text(&c["checkpoint_id"]), text(&c["approval_id"]));
    let pending = &c["pending_tool"];
    if cid.is_none()
        || aid.is_none()
        || c["status"] != "blocked"
        || !pending.is_object()
        || !c["checkpoint_receipt"].is_object()
        || c["checkpoint_receipt"]["committed"] != true
        || c["checkpoint_receipt"]["durability"] != "database"
    {
        errors
            .push("approval checkpoint is not a committed database-durable blocked receipt".into())
    }
    let (id, name, hash) = (
        text(&pending["tool_id"]),
        text(&pending["tool_name"]),
        text(&pending["arguments_hash"]),
    );
    if id.is_none() || name.is_none() || hash.is_none_or(|h| h.len() != 64) {
        errors.push("approval checkpoint lacks bound tool identity or arguments hash".into())
    }
    let empty = json!({});
    let args = starts
        .get(id.unwrap_or(""))
        .map(|s| &s["arguments"])
        .unwrap_or(&Value::Null);
    let args = if !args.is_object() {
        errors.push("approved tool has no canonical dispatch arguments".into());
        &empty
    } else {
        if Some(sha256(spaced_json(args).as_bytes()).as_str()) != hash {
            errors.push("approved dispatch arguments do not match the checkpoint hash".into())
        }
        args
    };
    let rid = text(&rc["checkpoint_id"]);
    if rid.is_none()
        || rid == cid
        || rc["status"] != "running"
        || rc["approval_id"] != c["approval_id"]
        || !rc["pending_tool"].is_object()
        || rc["pending_tool"]["tool_id"] != pending["tool_id"]
        || rc["pending_tool"]["tool_name"] != pending["tool_name"]
        || rc["pending_tool"]["arguments_hash"] != pending["arguments_hash"]
        || !rc["checkpoint_receipt"].is_object()
        || rc["checkpoint_receipt"]["committed"] != true
        || rc["checkpoint_receipt"]["durability"] != "database"
    {
        errors.push("resume checkpoint is not database-durable and bound to the approval".into())
    }
    if pause.data["checkpoint_id"] != c["checkpoint_id"]
        || pause.data["approval_id"] != c["approval_id"]
        || pause.data["tool_id"] != pending["tool_id"]
        || pause.data["tool_name"] != pending["tool_name"]
    {
        errors.push("approval pause event does not match its checkpoint".into())
    }
    let se: Vec<_> = turns
        .iter()
        .flat_map(|t| &t.events)
        .filter(|e| e.kind == "tool_call_start" && text(&e.data["tool_call_id"]) == id)
        .collect();
    let ee: Vec<_> = turns
        .iter()
        .flat_map(|t| &t.events)
        .filter(|e| e.kind == "tool_call_end" && text(&e.data["tool_call_id"]) == id)
        .collect();
    let result = results.get(id.unwrap_or(""));
    let ordered = if let (Some(result), [start], [end]) = (result, se.as_slice(), ee.as_slice()) {
        resumed.index == start.turn
            && pause.position() < (resumed.index, 0)
            && (resumed.index, 0) < start.position()
            && start.position() < result.position()
            && result.position() < end.position()
            && end.position() < resume.position()
    } else {
        false
    };
    if resume.data["approval_id"] != c["approval_id"]
        || resume.data["tool_id"] != pending["tool_id"]
        || resume.data["tool_name"] != pending["tool_name"]
        || resume.data["approved"] != true
        || !ordered
    {
        errors.push("approval result/dispatch is missing, mismatched, or out of order".into())
    }
    if starts.len() != usize::from(id.is_some()) || id.is_some_and(|id| !starts.contains_key(id)) {
        errors.push("HITL receipt cannot classify unrelated tool calls".into())
    }
    if CONTROLS.iter().any(|k| args.get(*k).is_some()) {
        errors.push("approved dispatch exposed runtime control arguments".into())
    }
    if !bound {
        errors.push("producer identity binding is not verified".into())
    }
    if !errors.is_empty() {
        return component("unknown", errors.join("; "));
    }
    let (id, cid, aid, name) = (id.unwrap(), cid.unwrap(), aid.unwrap(), name.unwrap());
    let hash = digest(args);
    for value in replay.values_mut() {
        if let Some(calls) = value["tool_calls"].as_array_mut() {
            for call in calls {
                if call["call_id"] == id {
                    call["arguments"] = args.clone();
                    call["arguments_hash"] = json!(hash);
                    call["dispatch_state"] = json!("dispatched");
                    call["approval_required"] = json!(true);
                    call["checkpoint_id"] = json!(cid);
                }
            }
        }
    }
    let pending = json!({"call_id":id,"name":name,"arguments":args,"arguments_hash":hash,"dispatch_state":"pending_approval","approval_required":true,"checkpoint_id":cid});
    let turn = replay.get_mut(&checkpoint.index).unwrap();
    if !turn["tool_calls"].is_array() {
        turn["tool_calls"] = json!([])
    }
    turn["tool_calls"]
        .as_array_mut()
        .unwrap()
        .insert(0, pending);
    replay.get_mut(&pause.turn).unwrap()["hitl"] =
        json!({"state":"paused","checkpoint_id":cid,"dispatch_count":0});
    replay.get_mut(&resume.turn).unwrap()["hitl"] = json!({"state":"resumed","checkpoint_id":cid,"approval_granted":true,"approved_call_ids":[id],"approved_arguments_hashes":{id:hash},"dispatch_count":1});
    let mut result = component(
        "verified",
        "durable checkpoint, approval receipt, and canonical dispatch are hash-bound",
    );
    result["checkpoint_id"] = json!(cid);
    result["approval_id"] = json!(aid);
    result
}
pub fn load_producer_artifacts(path: &str) -> Result<Vec<Value>> {
    Ok(crate::snapshot(path)?.0)
}
pub fn adapt_producer_case(row: &Value) -> Result<(String, Value)> {
    let id = text(&row["case_id"]).ok_or("producer artifact row requires a non-empty case_id")?;
    let p = &row["producer"];
    if !p.is_object() {
        return Err(format!("producer artifact {id:?} requires a producer object").into());
    }
    if p["schema_version"] != PRODUCER_EVIDENCE_SCHEMA_VERSION {
        return Err(format!(
            "producer artifact {id:?} requires schema {PRODUCER_EVIDENCE_SCHEMA_VERSION:?}"
        )
        .into());
    }
    let trace = object(&p["trace"]);
    let (turns, mut errors) = normalize(p);
    let trace_status = text(&trace["status"]);
    let envelope = turns
        .iter()
        .rev()
        .find(|t| obj(&t.envelope))
        .map(|t| &t.envelope)
        .unwrap_or(&Value::Null);
    let envelope_status = text(&envelope["status"]);
    if trace_status
        .zip(envelope_status)
        .is_some_and(|(a, b)| a != b)
    {
        errors.push("trace status conflicts with the final terminal envelope".into())
    }
    errors.extend(binding(&trace, &turns));
    let bound = errors.is_empty();
    let mut replay: BTreeMap<u64, Value> = turns
        .iter()
        .map(|t| (t.index, json!({"turn_index":t.index})))
        .collect();
    for t in &turns {
        let tenants: BTreeSet<_> = [&t.envelope, &t.snapshot, &t.checkpoint]
            .iter()
            .filter_map(|v| identity(v).get("tenant_id").cloned())
            .collect();
        if tenants.len() == 1 {
            replay.get_mut(&t.index).unwrap()["tenant_id"] = json!(tenants.first().unwrap());
        }
    }
    let (tool, starts, results) = tool_projection(&turns, &mut replay, bound);
    let budget = budget_projection(&turns, &mut replay, bound);
    let hitl = hitl_projection(&turns, &mut replay, &starts, &results, bound);
    let tool_observed = turns.iter().flat_map(|t| &t.events).any(|e| {
        ["tool_call_start", "tool_call_result", "tool_call_end"].contains(&e.kind.as_str())
    });
    let budget_observed = turns.iter().any(|t| {
        t.snapshot["bootstrap"]["run_budget"].is_object()
            || t.envelope["usage"]["run_budget"].is_object()
            || t.events.iter().any(|e| e.kind == "run_budget_exceeded")
    });
    let hitl_observed = turns.iter().any(|t| {
        text(&t.checkpoint["phase"]).is_some_and(|p| {
            [
                "approval_pending",
                "tool_call_pending",
                "tool_call_completed",
            ]
            .contains(&p)
        }) || t
            .events
            .iter()
            .any(|e| ["approval_required", "approval_result"].contains(&e.kind.as_str()))
    });
    let mut integrity = errors.clone();
    for (observed, label, c) in [
        (tool_observed, "tool_pairing", &tool),
        (budget_observed, "budget", &budget),
        (hitl_observed, "hitl", &hitl),
    ] {
        if observed && c["status"] != "verified" {
            integrity.push(format!(
                "observed {label} evidence is {}",
                text(&c["status"]).unwrap_or("unknown")
            ))
        }
    }
    let verified = integrity.is_empty();
    let span_values: Vec<Value> = if let Some(a) = trace["span_kinds"].as_array() {
        a.clone()
    } else {
        trace["spans"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|v| text(&v["span_kind"]).is_some())
                    .map(|v| v["span_kind"].clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut seen = BTreeSet::new();
    let spans: Vec<_> = span_values
        .into_iter()
        .filter(|v| v.is_string() && seen.insert(v.as_str().unwrap().to_owned()))
        .collect();
    let output = if let Some(s) = trace["output_preview"].as_str() {
        s.to_owned()
    } else if trace["output_preview"].is_null()
        || trace["output_preview"] == false
        || trace["output_preview"] == 0
    {
        String::new()
    } else {
        canonical(&trace["output_preview"])
    };
    let mut result = json!({"status":if verified{trace_status.or(envelope_status).unwrap_or("unknown")}else{"unknown"},"output_preview":output,"span_kinds":spans,"turns":replay.into_values().collect::<Vec<_>>(),"source_adapter":{"name":"canonical_assistant_producer","schema_version":ADAPTER_SCHEMA_VERSION},"adapter_evidence":{"source":"canonical_assistant_producer","schema_version":ADAPTER_SCHEMA_VERSION,"evidence_tier":"recorded_runtime_artifact","integrity":component(if verified{"verified"}else{"not_verified"},if verified{"all observed canonical receipts are internally consistent".into()}else{integrity.join("; ")}),"components":{"binding":finish(errors,"run/tenant/session receipts agree"),"plan":component("unknown","producer has no stable plan identity plus per-turn completion receipt"),"tool_pairing":tool,"budget":budget,"hitl":hitl,"compaction":component("unknown","lineage does not prove literal retained/dropped required facts"),"security":component("unknown","producer has no prompt-policy or cross-tenant access decision receipt")}}});
    if let Some(reason) = text(&envelope["exit_reason"]) {
        result["exit_reason"] = json!(reason)
    }
    for k in [
        "cache_hit",
        "cached_tokens",
        "cost_cents",
        "input_tokens",
        "output_tokens",
        "total_latency_ms",
        "total_tokens",
        "trace_id",
    ] {
        if let Some(v) = trace.get(k) {
            result[k] = v.clone()
        }
    }
    Ok((id.into(), result))
}
pub fn adapt_producer_artifacts(rows: &[Value]) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for row in rows {
        let (id, replay) = adapt_producer_case(row)?;
        if out.insert(id.clone(), replay).is_some() {
            return Err(format!("duplicate producer artifact case_id {id:?}").into());
        }
    }
    Ok(out)
}
pub fn summarize_adapter_evidence(observations: &BTreeMap<String, Value>) -> Value {
    let mut components = json!({});
    for c in COMPONENTS {
        components[*c] = json!({"verified":0,"unknown":0,"not_run":0})
    }
    let mut integrity = json!({"verified":0,"not_verified":0});
    for v in observations.values() {
        let evidence = &v["adapter_evidence"];
        let status = if evidence["integrity"]["status"] == "verified" {
            "verified"
        } else {
            "not_verified"
        };
        integrity[status] = json!(integrity[status].as_u64().unwrap() + 1);
        for c in COMPONENTS {
            let status = text(&evidence["components"][c]["status"])
                .filter(|s| ["verified", "unknown", "not_run"].contains(s))
                .unwrap_or("unknown");
            components[*c][status] = json!(components[*c][status].as_u64().unwrap() + 1)
        }
    }
    json!({"source_adapter":"canonical_assistant_producer","schema_version":ADAPTER_SCHEMA_VERSION,"observation_count":observations.len(),"status":if !observations.is_empty()&&integrity["not_verified"]==0{"verified"}else{"not_verified"},"integrity":integrity,"components":components})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_evidence_is_never_verified() {
        let(_,r)=adapt_producer_case(&json!({"case_id":"x","producer":{"schema_version":PRODUCER_EVIDENCE_SCHEMA_VERSION,"turns":[]}})).unwrap();
        assert_eq!(r["status"], "unknown");
        assert_eq!(r["adapter_evidence"]["integrity"]["status"], "not_verified");
    }
    #[test]
    fn duplicate_artifacts_refused() {
        let r = json!({"case_id":"x","producer":{"schema_version":PRODUCER_EVIDENCE_SCHEMA_VERSION,"turns":[]}});
        assert!(adapt_producer_artifacts(&[r.clone(), r]).is_err());
    }
    #[test]
    fn runtime_hash_keeps_python_spaces() {
        let v = json!({"b":[1,2],"a":"é"});
        assert_eq!(spaced_json(&v), "{\"a\": \"é\", \"b\": [1, 2]}");
        assert_ne!(sha256(spaced_json(&v).as_bytes()), digest(&v));
    }
    #[test]
    fn empty_summary_not_verified() {
        assert_eq!(
            summarize_adapter_evidence(&BTreeMap::new())["status"],
            "not_verified"
        );
    }
}
#[cfg(test)]
mod source_parity {
    use super::*;
    #[test]
    fn frozen_original_producer_cases() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("producer_goldens.json")).unwrap();
        assert!(cases.len() >= 17);
        for case in cases {
            let (_, actual) = adapt_producer_case(&case["input"]).unwrap();
            assert_eq!(
                actual, case["expected"],
                "source parity case {}",
                case["name"]
            );
        }
    }
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
