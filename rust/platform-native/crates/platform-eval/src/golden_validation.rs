use crate::{nonempty, string_list};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const ASSERTIONS: [&str; 11] = [
    "output_equals",
    "output_contains",
    "output_not_contains",
    "required_span_kind",
    "no_sensitive_output",
    "latency_ms_lt",
    "total_tokens_lt",
    "cost_cents_lt",
    "tool_called",
    "tool_not_called",
    "failure_mode_absent",
];
pub fn case_errors(case: &Value) -> Vec<String> {
    let mut e = vec![];
    for key in [
        "case_id",
        "input",
        "expected_output",
        "expected_trajectory",
        "assertions",
        "metadata",
    ] {
        if case.get(key).is_none() {
            e.push(format!("missing {key}"));
        }
    }
    if !case["case_id"].as_str().is_some_and(|s| !s.is_empty()) {
        e.push("case_id must be a non-empty string".into());
    }
    for key in [
        "input",
        "expected_output",
        "expected_trajectory",
        "metadata",
    ] {
        if case.get(key).is_some_and(|v| !v.is_object()) {
            e.push(format!("{key} must be an object"));
        }
    }
    let trajectory = &case["expected_trajectory"];
    if trajectory.get("runtime").is_some_and(|v| !v.is_object()) {
        e.push("expected_trajectory.runtime must be an object".into());
    }
    if trajectory
        .get("required_span_kinds")
        .is_some_and(|v| !string_list(v, true))
    {
        e.push("expected_trajectory.required_span_kinds must be a string list".into());
    }
    if let Some(stateful) = trajectory.get("stateful") {
        e.extend(stateful_errors(stateful));
    }
    let output = &case["expected_output"];
    for key in ["reference", "rubric"] {
        if output.get(key).is_some_and(|v| !nonempty(v)) {
            e.push(format!("expected_output.{key} must be a non-empty string"));
        }
    }
    for key in ["contains", "not_contains"] {
        if output
            .get(key)
            .is_some_and(|v| !nonempty(v) && !string_list(v, false))
        {
            e.push(format!(
                "expected_output.{key} must be a non-empty string or string list"
            ));
        }
    }
    if let Some(tools) = trajectory.get("tools") {
        if let Some(tools) = tools.as_array() {
            for (i, t) in tools.iter().enumerate() {
                let p = format!("expected_trajectory.tools[{}]", i + 1);
                if !t.is_object() {
                    e.push(format!("{p} must be an object"));
                    continue;
                }
                unknown(
                    t,
                    &[
                        "name",
                        "required",
                        "forbidden",
                        "arguments_subset",
                        "order",
                        "max_calls",
                        "status",
                    ],
                    &p,
                    &mut e,
                );
                if !nonempty(&t["name"]) {
                    e.push(format!("{p}.name must be a non-empty string"));
                }
                for key in ["required", "forbidden"] {
                    if t.get(key).is_some_and(|v| !v.is_boolean()) {
                        e.push(format!("{p}.{key} must be boolean"));
                    }
                }
                if t["required"] == true && t["forbidden"] == true {
                    e.push(format!("{p} cannot be both required and forbidden"));
                }
                if t.get("arguments_subset").is_some_and(|v| !v.is_object()) {
                    e.push(format!("{p}.arguments_subset must be an object"));
                }
                if t.get("order").is_some_and(|v| !positive(v)) {
                    e.push(format!("{p}.order must be a positive integer"));
                }
                if t.get("max_calls").is_some_and(|v| v.as_u64().is_none()) {
                    e.push(format!("{p}.max_calls must be a non-negative integer"));
                }
                if t.get("status").is_some_and(|v| !nonempty(v)) {
                    e.push(format!("{p}.status must be a non-empty string"));
                }
            }
        } else {
            e.push("expected_trajectory.tools must be a list".into());
        }
    }
    if let Some(assertions) = case.get("assertions") {
        if let Some(a) = assertions.as_array() {
            for (i, a) in a.iter().enumerate() {
                let p = format!("assertions[{}]", i + 1);
                if !a.is_object() {
                    e.push(format!("{p} must be an object"));
                    continue;
                }
                let kind = a["type"].as_str().unwrap_or("");
                if !ASSERTIONS.contains(&kind) {
                    e.push(format!("{p} has unsupported type {}", repr(&a["type"])));
                } else if ["latency_ms_lt", "total_tokens_lt", "cost_cents_lt"].contains(&kind) {
                    if !a["value"].as_f64().is_some_and(|v| v.is_finite() && v > 0.) {
                        e.push(format!("{p}.value must be a positive number"));
                    }
                } else if kind != "no_sensitive_output" && !nonempty(&a["value"]) {
                    e.push(format!("{p}.value must be a non-empty string"));
                }
            }
        } else {
            e.push("assertions must be a list".into());
        }
    }
    if case
        .get("split")
        .is_some_and(|v| !v.as_str().is_some_and(|s| !s.is_empty()))
    {
        e.push("split must be a non-empty string".into());
    }
    let m = &case["metadata"];
    for k in ["critical", "behavior_confirmed"] {
        if m.get(k).is_some_and(|v| !v.is_null() && !v.is_boolean()) {
            e.push(format!("metadata.{k} must be boolean when present"));
        }
    }
    for k in ["owner", "difficulty"] {
        if m.get(k).is_some_and(|v| !v.is_null() && !v.is_string()) {
            e.push(format!("metadata.{k} must be a string when present"));
        }
    }
    if m.get("review_status").is_some_and(|v| {
        !v.is_null()
            && !["pending", "approved", "rejected", "needs_fix"].contains(&v.as_str().unwrap_or(""))
    }) {
        e.push("metadata.review_status is invalid".into());
    }
    if m.get("tags")
        .is_some_and(|v| !v.is_null() && !string_list(v, true))
    {
        e.push("metadata.tags must be a string list when present".into());
    }
    e
}
pub fn repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => v.to_string(),
    }
}
fn positive(v: &Value) -> bool {
    v.as_u64().is_some_and(|n| n > 0)
}
fn unknown(v: &Value, allowed: &[&str], p: &str, e: &mut Vec<String>) {
    if let Some(m) = v.as_object() {
        let mut keys = m
            .keys()
            .filter(|k| !allowed.contains(&k.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        if !keys.is_empty() {
            e.push(format!("{p} has unsupported fields {}", keys.join(", ")));
        }
    }
}
pub fn stateful_errors(v: &Value) -> Vec<String> {
    let p = "expected_trajectory.stateful";
    if !v.as_object().is_some_and(|m| !m.is_empty()) {
        return vec![format!("{p} must be a non-empty object")];
    }
    let mut e = vec![];
    unknown(
        v,
        &[
            "minimum_turns",
            "plan",
            "tool_pairing",
            "budget",
            "hitl",
            "compaction",
            "security",
        ],
        p,
        &mut e,
    );
    if v.get("minimum_turns")
        .is_some_and(|v| !v.is_null() && !v.as_u64().is_some_and(|n| n >= 2))
    {
        e.push(format!("{p}.minimum_turns must be an integer >= 2"));
    }
    for (section, allowed, required) in [
        (
            "plan",
            &["plan_id", "goal", "required_steps", "final_completed_steps"][..],
            &["plan_id", "goal", "required_steps", "final_completed_steps"][..],
        ),
        (
            "tool_pairing",
            &["required", "require_success"][..],
            &["required"][..],
        ),
        (
            "budget",
            &["max_iterations", "expected_exit_reason", "terminal_turn"][..],
            &["max_iterations", "expected_exit_reason", "terminal_turn"][..],
        ),
        (
            "hitl",
            &[
                "approved_calls",
                "checkpoint_id",
                "max_dispatch_before_approval",
                "minimum_pending_approval_calls",
                "minimum_postapproval_dispatches",
                "protected_tools",
                "resume_count",
            ][..],
            &[
                "approved_calls",
                "checkpoint_id",
                "max_dispatch_before_approval",
                "minimum_pending_approval_calls",
                "minimum_postapproval_dispatches",
                "protected_tools",
                "resume_count",
            ][..],
        ),
        (
            "compaction",
            &[
                "compaction_id",
                "required_facts",
                "max_dropped_required_facts",
            ][..],
            &[
                "compaction_id",
                "required_facts",
                "max_dropped_required_facts",
            ][..],
        ),
        (
            "security",
            &[
                "untrusted_instructions_ignored",
                "forbidden_tools",
                "tenant_id",
                "foreign_tenant_access",
            ][..],
            &[][..],
        ),
    ] {
        if let Some(s) = v.get(section) {
            let prefix = format!("{p}.{section}");
            if !s.as_object().is_some_and(|m| !m.is_empty()) {
                e.push(format!("{prefix} must be a non-empty object"));
                continue;
            }
            unknown(s, allowed, &prefix, &mut e);
            let mut missing = required
                .iter()
                .filter(|k| s.get(**k).is_none())
                .copied()
                .collect::<Vec<_>>();
            missing.sort();
            if !missing.is_empty() {
                e.push(format!(
                    "{prefix} is missing required fields {}",
                    missing.join(", ")
                ));
            }
        }
    }
    for k in ["plan_id", "goal"] {
        if v["plan"].get(k).is_some_and(|v| !nonempty(v)) {
            e.push(format!("{p}.plan.{k} must be a non-empty string"));
        }
    }
    for k in ["required_steps", "final_completed_steps"] {
        if v["plan"].get(k).is_some_and(|v| !string_list(v, false)) {
            e.push(format!("{p}.plan.{k} must be a non-empty string list"));
        }
    }
    if v["tool_pairing"].is_object() {
        if v["tool_pairing"]["required"] != true {
            e.push(format!("{p}.tool_pairing.required must be true"));
        }
        if v["tool_pairing"]
            .get("require_success")
            .is_some_and(|v| !v.is_boolean())
        {
            e.push(format!("{p}.tool_pairing.require_success must be boolean"));
        }
    }
    let budget = &v["budget"];
    for k in ["max_iterations", "terminal_turn"] {
        if budget.get(k).is_some_and(|v| !positive(v)) {
            e.push(format!("{p}.budget.{k} must be a positive integer"));
        }
    }
    if budget
        .get("expected_exit_reason")
        .is_some_and(|v| !nonempty(v))
    {
        e.push(format!(
            "{p}.budget.expected_exit_reason must be a non-empty string"
        ));
    }
    if budget["expected_exit_reason"] == "max_iterations"
        && budget["max_iterations"].is_i64()
        && budget["terminal_turn"] != budget["max_iterations"]
    {
        e.push(format!(
            "{p}.budget.terminal_turn must equal max_iterations for max_iterations exit"
        ));
    }
    let hitl = &v["hitl"];
    if hitl.is_object() {
        if !nonempty(&hitl["checkpoint_id"]) {
            e.push(format!("{p}.hitl.checkpoint_id must be a non-empty string"));
        }
        if hitl
            .get("max_dispatch_before_approval")
            .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
        {
            e.push(format!(
                "{p}.hitl.max_dispatch_before_approval must be a non-negative integer"
            ));
        }
        if hitl
            .get("resume_count")
            .is_some_and(|v| !v.is_null() && v.as_u64() != Some(1))
        {
            e.push(format!(
                "{p}.hitl.resume_count must equal 1 in the v1 contract"
            ));
        }
        for k in [
            "minimum_pending_approval_calls",
            "minimum_postapproval_dispatches",
        ] {
            if hitl.get(k).is_some_and(|v| !v.is_null() && !positive(v)) {
                e.push(format!("{p}.hitl.{k} must be a positive integer"));
            }
        }
        let protected = &hitl["protected_tools"];
        let valid = string_list(protected, false)
            && protected
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len()
                == protected.as_array().unwrap().len();
        if !valid {
            e.push(format!(
                "{p}.hitl.protected_tools must be a unique string list"
            ));
        }
        if let Some(calls) = hitl["approved_calls"].as_array().filter(|a| !a.is_empty()) {
            let mut ids = BTreeSet::new();
            for (i, c) in calls.iter().enumerate() {
                let cp = format!("{p}.hitl.approved_calls[{}]", i + 1);
                if !c.is_object() {
                    e.push(format!("{cp} must be an object"));
                    continue;
                }
                unknown(c, &["call_id", "tool_name", "arguments_hash"], &cp, &mut e);
                for k in ["call_id", "tool_name", "arguments_hash"] {
                    if !nonempty(&c[k]) {
                        e.push(format!("{cp}.{k} must be a non-empty string"));
                    }
                }
                if nonempty(&c["call_id"]) && !ids.insert(c["call_id"].as_str().unwrap()) {
                    e.push(format!("{cp}.call_id must be unique"));
                }
                if c["arguments_hash"].as_str().is_some_and(|s| {
                    s.len() != 64
                        || !s
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                }) {
                    e.push(format!("{cp}.arguments_hash must be lowercase SHA-256"));
                }
                if valid
                    && c["tool_name"].is_string()
                    && !protected.as_array().unwrap().contains(&c["tool_name"])
                {
                    e.push(format!("{cp}.tool_name must be listed in protected_tools"));
                }
            }
            for k in [
                "minimum_pending_approval_calls",
                "minimum_postapproval_dispatches",
            ] {
                if hitl[k].as_u64().is_some_and(|n| n > calls.len() as u64) {
                    e.push(format!("{p}.hitl.{k} exceeds approved_calls"));
                }
            }
        } else {
            e.push(format!(
                "{p}.hitl.approved_calls must be a non-empty object list"
            ));
        }
    }
    let c = &v["compaction"];
    if c.is_object() {
        if !nonempty(&c["compaction_id"]) {
            e.push(format!(
                "{p}.compaction.compaction_id must be a non-empty string"
            ));
        }
        if !string_list(&c["required_facts"], false) {
            e.push(format!(
                "{p}.compaction.required_facts must be a string list"
            ));
        }
        if c.get("max_dropped_required_facts")
            .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
        {
            e.push(format!(
                "{p}.compaction.max_dropped_required_facts must be non-negative"
            ));
        }
    }
    let s = &v["security"];
    for k in ["untrusted_instructions_ignored", "foreign_tenant_access"] {
        if s.get(k).is_some_and(|v| !v.is_boolean()) {
            e.push(format!("{p}.security.{k} must be boolean"));
        }
    }
    if s.get("tenant_id")
        .is_some_and(|v| !v.is_null() && !nonempty(v))
    {
        e.push(format!("{p}.security.tenant_id must be a non-empty string"));
    }
    if s.get("forbidden_tools")
        .is_some_and(|v| !v.is_null() && !string_list(v, true))
    {
        e.push(format!(
            "{p}.security.forbidden_tools must be a string list"
        ));
    }
    e
}
pub fn validate_cases(cases: &[Value]) -> Value {
    let mut seen = BTreeSet::new();
    let mut errors = vec![];
    for (i, c) in cases.iter().enumerate() {
        let id = c["case_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("line-{}", i + 1));
        let mut e = case_errors(c);
        if !seen.insert(id.clone()) {
            e.push("duplicate case_id".into());
        }
        if !e.is_empty() {
            errors.push(json!({"case_id":id,"errors":e}));
        }
    }
    crate::validation(cases.len(), errors)
}
pub fn validate_observations(cases: &[Value], obs: &BTreeMap<String, Value>) -> Value {
    let ids = cases
        .iter()
        .filter_map(|c| c["case_id"].as_str().filter(|s| !s.is_empty()))
        .collect::<BTreeSet<_>>();
    let mut errors = vec![];
    for id in ids.iter().filter(|id| !obs.contains_key(**id)) {
        errors.push(json!({"case_id":id,"errors":["missing replay observation"]}));
    }
    for id in obs.keys().filter(|id| !ids.contains(id.as_str())) {
        errors.push(json!({"case_id":id,"errors":["observation has no expectation"]}));
    }
    let mut joined = 0;
    for id in &ids {
        if let Some(v) = obs.get(*id) {
            joined += 1;
            let message = if !v.as_object().is_some_and(|m| !m.is_empty()) {
                Some("replay observation must be a non-empty object")
            } else if !v["status"].as_str().is_some_and(|s| !s.is_empty()) {
                Some("replay observation must have a non-empty status")
            } else {
                None
            };
            if let Some(m) = message {
                errors.push(json!({"case_id":id,"errors":[m]}));
            }
        }
    }
    json!({"valid":errors.is_empty(),"case_count":ids.len(),"observation_count":obs.len(),"joined_count":joined,"errors":errors})
}
