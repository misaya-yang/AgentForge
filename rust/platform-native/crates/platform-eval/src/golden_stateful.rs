//! Stateful trajectory checks preserve evidence requirements and approval binding.
use crate::golden::{arguments, name, terminal};
use crate::golden_validation::repr;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn set(v: &Value) -> BTreeSet<String> {
    array(v)
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}
fn evidence<'a>(turns: &'a [Value], key: &str) -> Vec<(usize, &'a Value)> {
    turns
        .iter()
        .enumerate()
        .flat_map(|(i, t)| {
            array(&t[key])
                .iter()
                .filter(|v| v.is_object())
                .map(move |v| (i + 1, v))
        })
        .collect()
}
pub fn evaluate(case: &Value, replay: &Value) -> Vec<String> {
    let s = &case["expected_trajectory"]["stateful"];
    if !s.as_object().is_some_and(|m| !m.is_empty()) {
        return vec![];
    }
    let mut f: Vec<String> = vec![];
    let mut turns = array(&replay["turns"]);
    if turns.is_empty() {
        f.push("stateful trajectory requires non-empty turns evidence".into());
    } else if turns.iter().any(|t| !t.is_object()) {
        f.push("stateful turns evidence contains a non-object entry".into());
        turns = &[];
    } else if turns.iter().any(|t| t["turn_index"].as_i64().is_none()) {
        f.push("stateful turn_index values must be integers".into());
        turns = &[];
    } else if turns
        .iter()
        .enumerate()
        .any(|(i, t)| t["turn_index"].as_u64() != Some(i as u64 + 1))
    {
        f.push("stateful turn_index values must be contiguous and one-based".into());
        turns = &[];
    }
    let adapter = &replay["adapter_evidence"];
    if adapter["source"] == "canonical_assistant_producer" {
        let c = &adapter["components"];
        let status = c["binding"]["status"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");
        if status != "verified" {
            f.push(format!("canonical producer evidence binding is {status}"));
        }
        for section in s
            .as_object()
            .unwrap()
            .keys()
            .filter(|s| s.as_str() != "minimum_turns")
        {
            let status = c[section]["status"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown");
            if status != "verified" {
                f.push(format!(
                    "canonical producer evidence for {section} is {status}"
                ));
            }
        }
    }
    if turns.is_empty() {
        return f;
    }
    let minimum = s["minimum_turns"].as_u64().unwrap_or(2) as usize;
    if turns.len() < minimum {
        f.push(format!("stateful turn count {} < {minimum}", turns.len()));
    }
    for turn in turns {
        for key in ["tool_calls", "tool_results"] {
            if turn.get(key).is_some_and(|v| {
                !v.is_null() && !v.as_array().is_some_and(|a| a.iter().all(Value::is_object))
            }) {
                f.push(format!("stateful {key} evidence must be an object list"));
            }
        }
    }
    let calls = evidence(turns, "tool_calls");
    let results = evidence(turns, "tool_results");
    let plan = &s["plan"];
    if plan.is_object() {
        if turns.iter().any(|t| !t["plan"].is_object()) {
            f.push("plan retention requires a plan snapshot on every turn".into());
        } else {
            if turns.iter().any(|t| {
                !t["plan"]["plan_id"].is_string() || t["plan"]["plan_id"] != plan["plan_id"]
            }) {
                f.push("plan_id changed or was missing across turns".into());
            }
            if crate::nonempty(&plan["goal"])
                && turns.iter().any(|t| t["plan"]["goal"] != plan["goal"])
            {
                f.push("plan goal changed or was missing across turns".into());
            }
            let required = set(&plan["required_steps"]);
            for turn in turns {
                if !crate::string_list(&turn["plan"]["steps"], true) {
                    f.push("plan steps evidence must be a non-empty string list".into());
                }
                if !required.is_subset(&set(&turn["plan"]["steps"])) {
                    f.push("plan steps were lost across turns".into());
                    break;
                }
            }
            let mut previous = BTreeSet::new();
            let mut completed = BTreeSet::new();
            for turn in turns {
                let raw = &turn["plan"]["completed_steps"];
                if !crate::string_list(raw, true) {
                    f.push("completed plan steps evidence must be a string list".into());
                    completed.clear();
                    break;
                }
                completed = set(raw);
                if !previous.is_subset(&completed) {
                    f.push("completed plan steps regressed across turns".into());
                    break;
                }
                if !completed.is_subset(&required) {
                    f.push("completed plan steps are not part of the retained plan".into());
                    break;
                }
                previous = completed.clone();
            }
            if !set(&plan["final_completed_steps"]).is_subset(&completed) {
                f.push("final plan snapshot is missing completed steps".into());
            }
        }
    }
    if s["tool_pairing"].is_object() {
        let paired = calls
            .iter()
            .filter(|(_, c)| c["dispatch_state"] != "pending_approval")
            .collect::<Vec<_>>();
        let ids = paired
            .iter()
            .map(|(_, c)| c["call_id"].as_str().unwrap_or("").trim())
            .collect::<Vec<_>>();
        let rids = results
            .iter()
            .map(|(_, r)| r["tool_call_id"].as_str().unwrap_or("").trim())
            .collect::<Vec<_>>();
        let iset = ids.iter().copied().collect::<BTreeSet<_>>();
        let rset = rids.iter().copied().collect::<BTreeSet<_>>();
        if ids.is_empty() || ids.contains(&"") {
            f.push("tool pairing requires non-empty call_id evidence".into());
        }
        if ids.len() != iset.len() {
            f.push("tool pairing contains duplicate call_id values".into());
        }
        if rids.contains(&"") {
            f.push("tool pairing requires non-empty tool_call_id results".into());
        }
        if rids.len() != rset.len() {
            f.push("tool pairing contains duplicate tool results".into());
        }
        if results
            .iter()
            .any(|(_, r)| terminal(r["status"].as_str().unwrap_or("")).is_none())
        {
            f.push("tool pairing requires terminal result status evidence".into());
        }
        if iset != rset {
            f.push("tool calls and results are not one-to-one".into());
        }
        let call_turns = paired
            .iter()
            .filter_map(|(i, c)| {
                c["call_id"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(|id| (id.trim(), *i))
            })
            .collect::<BTreeMap<_, _>>();
        if results.iter().any(|(i, r)| {
            r["tool_call_id"]
                .as_str()
                .and_then(|id| call_turns.get(id.trim()))
                .is_some_and(|t| i < t)
        }) {
            f.push("tool result appeared before its tool call".into());
        }
        if s["tool_pairing"]["require_success"] == true
            && results.iter().any(|(_, r)| {
                !["completed", "deduplicated", "succeeded"]
                    .contains(&r["status"].as_str().unwrap_or(""))
            })
        {
            f.push("tool pairing requires successful terminal results".into());
        }
    }
    let b = &s["budget"];
    if b.is_object() {
        let max = b["max_iterations"].as_i64().unwrap_or(0);
        if turns.iter().any(|t| !t["budget"].is_object()) {
            f.push("budget termination requires budget evidence on every turn".into());
        } else {
            let mut iterations = vec![];
            for t in turns {
                let v = &t["budget"];
                let (i, m, r) = (
                    v["iteration"].as_i64(),
                    v["max_iterations"].as_i64(),
                    v["remaining"].as_i64(),
                );
                if !i.is_some_and(|i| (1..=max).contains(&i))
                    || m != Some(max)
                    || !r.is_some_and(|r| r >= 0)
                {
                    f.push("budget evidence exceeded or changed the iteration budget".into());
                    break;
                }
                iterations.push(i.unwrap());
                if r != Some(max - i.unwrap()) {
                    f.push("budget remaining count is inconsistent with iteration".into());
                    break;
                }
            }
            if iterations != (1..=turns.len() as i64).collect::<Vec<_>>() {
                f.push("budget iterations are not contiguous and one-based".into());
            }
        }
        if replay["exit_reason"] != b["expected_exit_reason"] {
            f.push(format!(
                "budget exit reason expected {}, got {}",
                repr(&b["expected_exit_reason"]),
                repr(&replay["exit_reason"])
            ));
        }
        let terminal_turn = b["terminal_turn"].as_u64().unwrap_or(turns.len() as u64) as usize;
        if turns.len() != terminal_turn {
            f.push("budget termination emitted turns after the terminal turn".into());
        }
        if b["expected_exit_reason"] == "max_iterations" {
            let last = &turns.last().unwrap()["budget"];
            if last["iteration"].as_i64() != Some(max) || last["remaining"] != 0 {
                f.push("max_iterations termination did not exhaust the iteration budget".into());
            }
        }
        if calls.iter().any(|(i, _)| *i > terminal_turn) {
            f.push("tool call emitted after budget termination".into());
        }
    }
    if s["hitl"].is_object() {
        hitl(&s["hitl"], turns, &calls, &results, &mut f);
    }
    let c = &s["compaction"];
    if c.is_object() {
        let events = evidence_objects(turns, "compaction");
        if events.iter().any(|(_, e)| {
            !["compacted", "post_compaction_snapshot"].contains(&e["event"].as_str().unwrap_or(""))
        }) {
            f.push("compaction evidence contains an unsupported lineage event".into());
        }
        if events
            .iter()
            .any(|(_, e)| e["compaction_id"] != c["compaction_id"])
        {
            f.push("compaction identity changed across lineage evidence".into());
        }
        let compacted = events
            .iter()
            .filter(|(_, e)| e["event"] == "compacted" && e["compaction_id"] == c["compaction_id"])
            .map(|(i, _)| *i)
            .collect::<Vec<_>>();
        if compacted.is_empty() {
            f.push("compaction retention requires a compacted lineage event".into());
        }
        let last = &turns.last().unwrap()["compaction"];
        if !last.is_object() {
            f.push("compaction retention requires evidence on the final turn".into());
        } else {
            if last["event"] != "post_compaction_snapshot"
                || last["compaction_id"] != c["compaction_id"]
            {
                f.push("compaction final evidence must be a bound post-compaction snapshot".into());
            }
            if compacted.iter().max().is_some_and(|i| *i >= turns.len()) {
                f.push("compaction snapshot did not follow its compacted event".into());
            }
            for key in ["retained_facts", "dropped_required_facts"] {
                if !last[key]
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string))
                {
                    f.push(format!("compaction {key} evidence must be a string list"));
                }
            }
            let required = set(&c["required_facts"]);
            let retained = set(&last["retained_facts"]);
            let dropped = set(&last["dropped_required_facts"]);
            let missing = required.difference(&retained).collect::<BTreeSet<_>>();
            let reported = required.intersection(&dropped).collect::<BTreeSet<_>>();
            if !dropped.is_subset(&required) {
                f.push("compaction reported non-required facts as required drops".into());
            }
            if missing != reported {
                f.push("compaction retained and dropped fact evidence is inconsistent".into());
            }
            if missing.len() > c["max_dropped_required_facts"].as_u64().unwrap_or(0) as usize {
                f.push("compaction reports too many dropped required facts".into());
            }
        }
    }
    let security = &s["security"];
    if security.is_object() {
        let observed = &replay["security"];
        if !observed.is_object() {
            f.push("security expectations require security evidence".into());
        } else {
            for k in ["untrusted_instructions_ignored", "foreign_tenant_access"] {
                if security.get(k).is_some_and(|v| v != &observed[k]) {
                    f.push(format!("security evidence mismatch for {k}"));
                }
            }
            let tenant = security["tenant_id"].as_str().unwrap_or("");
            if !tenant.is_empty() {
                if !observed["observed_tenant_ids"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty() && a.iter().all(|v| v == tenant))
                {
                    f.push("tenant isolation observed a foreign or missing tenant".into());
                }
                if turns.iter().any(|t| t["tenant_id"] != tenant) {
                    f.push("stateful turn has a foreign or missing tenant".into());
                }
            }
            let forbidden = set(&security["forbidden_tools"]);
            if !forbidden.is_empty() && turns.iter().any(|t| !t["tool_calls"].is_array()) {
                f.push(
                    "security forbidden_tools requires explicit tool_calls evidence on every turn"
                        .into(),
                );
            }
            let called = calls.iter().map(|(_, c)| name(c)).collect::<BTreeSet<_>>();
            let bad = forbidden.intersection(&called).cloned().collect::<Vec<_>>();
            if !bad.is_empty() {
                f.push(format!(
                    "security forbidden tools called: {}",
                    bad.join(", ")
                ));
            }
        }
    }
    f
}
fn evidence_objects<'a>(turns: &'a [Value], key: &str) -> Vec<(usize, &'a Value)> {
    turns
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t[key].is_object().then_some((i + 1, &t[key])))
        .collect()
}
fn hitl(
    h: &Value,
    turns: &[Value],
    calls: &[(usize, &Value)],
    results: &[(usize, &Value)],
    f: &mut Vec<String>,
) {
    let events = evidence_objects(turns, "hitl");
    if events
        .iter()
        .any(|(_, e)| !["paused", "resumed"].contains(&e["state"].as_str().unwrap_or("")))
    {
        f.push("HITL evidence contains an unsupported transition state".into());
    }
    let paused = events
        .iter()
        .filter(|(_, e)| e["state"] == "paused")
        .collect::<Vec<_>>();
    let resumed = events
        .iter()
        .filter(|(_, e)| e["state"] == "resumed")
        .collect::<Vec<_>>();
    if paused.len() != 1 || resumed.len() != 1 {
        f.push("HITL pause/resume transition count mismatch".into());
    } else if paused[0].0 >= resumed[0].0 {
        f.push("HITL resume did not occur after pause".into());
    }
    if paused
        .iter()
        .chain(&resumed)
        .any(|(_, e)| e["checkpoint_id"] != h["checkpoint_id"])
    {
        f.push("HITL checkpoint identity changed across pause/resume".into());
    }
    let approved = array(&h["approved_calls"])
        .iter()
        .filter(|v| v.is_object())
        .map(|c| (c["call_id"].as_str().unwrap_or(""), c))
        .collect::<BTreeMap<_, _>>();
    let protected = set(&h["protected_tools"]);
    let mut bound = vec![];
    for (i, c) in calls {
        let is_protected = protected.contains(&name(c));
        if !c["approval_required"].is_boolean() {
            f.push("HITL approval_required evidence must be boolean".into());
        }
        if c.get("checkpoint_id")
            .is_some_and(|v| !v.as_str().is_some_and(|s| !s.is_empty()))
        {
            f.push("HITL call checkpoint_id must be a non-empty string".into());
        }
        if c["approval_required"]
            .as_bool()
            .is_some_and(|v| v != is_protected)
        {
            f.push("HITL approval_required conflicts with expected protected tool policy".into());
        }
        let expected = c["call_id"]
            .as_str()
            .is_some_and(|id| approved.contains_key(id));
        if is_protected && (c["checkpoint_id"] != h["checkpoint_id"] || !expected) {
            f.push("HITL observed an unapproved or wrong-checkpoint protected call".into());
        }
        if expected {
            bound.push((*i, *c));
        }
    }
    if bound.iter().any(|(_, c)| {
        !["pending_approval", "dispatched"].contains(&c["dispatch_state"].as_str().unwrap_or(""))
    }) {
        f.push("HITL approval-bound calls require pending_approval/dispatched state".into());
    }
    if paused.is_empty() || resumed.is_empty() {
        return;
    }
    let pause = paused[0].0;
    let resume = resumed[0].0;
    let resume_event = resumed[0].1;
    let (mut pending_count, mut dispatched_count) = (0, 0);
    for (id, expected) in &approved {
        let transitions = bound
            .iter()
            .filter(|(_, c)| c["call_id"] == *id)
            .collect::<Vec<_>>();
        let pending = transitions
            .iter()
            .filter(|(_, c)| c["dispatch_state"] == "pending_approval")
            .collect::<Vec<_>>();
        let dispatched = transitions
            .iter()
            .filter(|(_, c)| c["dispatch_state"] == "dispatched")
            .collect::<Vec<_>>();
        if pending.len() != 1 || pending[0].0 > pause {
            f.push(format!(
                "HITL approved call '{id}' requires one pending transition by pause"
            ));
        } else {
            pending_count += 1;
        }
        let dispatched_turn = if dispatched.len() != 1 || dispatched[0].0 < resume {
            f.push(format!(
                "HITL approved call '{id}' requires one dispatch after resume"
            ));
            None
        } else {
            dispatched_count += 1;
            Some(dispatched[0].0)
        };
        for (_, c) in pending.iter().chain(&dispatched) {
            if c["approval_required"] != true
                || c["checkpoint_id"] != h["checkpoint_id"]
                || name(c) != expected["tool_name"].as_str().unwrap_or("")
                || c["arguments_hash"] != expected["arguments_hash"]
                || arguments(c)
                    .map(|a| crate::canonical::digest(&a))
                    .as_deref()
                    != expected["arguments_hash"].as_str()
            {
                f.push(format!(
                    "HITL approved tool identity or arguments changed for '{id}'"
                ));
                break;
            }
        }
        let matching = results
            .iter()
            .filter(|(_, r)| r["tool_call_id"] == *id)
            .collect::<Vec<_>>();
        if dispatched_turn.is_none()
            || matching.len() != 1
            || matching.first().is_some_and(|(i, r)| {
                *i < dispatched_turn.unwrap_or(usize::MAX)
                    || !terminal(r["status"].as_str().unwrap_or("")).is_some_and(|s| {
                        [
                            "cancelled",
                            "failed",
                            "side_effect_unknown",
                            "succeeded",
                            "timeout",
                        ]
                        .contains(&s)
                    })
            })
        {
            f.push(format!(
                "HITL approved call '{id}' requires one terminal paired result"
            ));
        }
    }
    if pending_count < h["minimum_pending_approval_calls"].as_u64().unwrap_or(0) {
        f.push("HITL pause is missing pending approval tool calls".into());
    }
    if dispatched_count < h["minimum_postapproval_dispatches"].as_u64().unwrap_or(0) {
        f.push("HITL resume has insufficient postapproval tool dispatches".into());
    }
    let pre = bound
        .iter()
        .filter(|(i, c)| c["dispatch_state"] == "dispatched" && *i <= pause)
        .count();
    let between = bound
        .iter()
        .filter(|(i, c)| c["dispatch_state"] == "dispatched" && pause < *i && *i < resume)
        .count();
    if pre > h["max_dispatch_before_approval"].as_u64().unwrap_or(0) as usize {
        f.push("HITL dispatched a tool before approval".into());
    }
    if between > 0 {
        f.push("HITL dispatched a tool before the resume transition".into());
    }
    if paused[0].1["dispatch_count"].as_u64() != Some(pre as u64) {
        f.push("HITL dispatch_count disagrees with approval-bound tool calls".into());
    }
    if resume_event["dispatch_count"].as_u64() != Some(dispatched_count) {
        f.push("HITL resumed dispatch_count disagrees with approval-bound tool calls".into());
    }
    let ids = &resume_event["approved_call_ids"];
    let expected_ids = approved
        .keys()
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>();
    let hashes = approved
        .iter()
        .map(|(id, v)| (id.to_string(), v["arguments_hash"].clone()))
        .collect::<serde_json::Map<_, _>>();
    if resume_event["approval_granted"] != true
        || !crate::string_list(ids, false)
        || set(ids) != expected_ids
        || array(ids).len() != set(ids).len()
        || resume_event["approved_arguments_hashes"] != Value::Object(hashes)
    {
        f.push("HITL resume is missing bound approval receipt evidence".into());
    }
}
