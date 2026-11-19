use super::*;
pub(super) fn identity(v: &Value) -> String {
    let profile = text(v, "profile_id");
    if !profile.is_empty() {
        profile.into()
    } else {
        let kind = text(v, "agent_type");
        if kind.is_empty() {
            String::new()
        } else {
            format!("builtin:{kind}")
        }
    }
}
fn tool(v: &Value) -> String {
    let names: BTreeSet<_> = ["name", "tool_name"]
        .iter()
        .filter_map(|k| v[k].as_str().filter(|s| !s.is_empty()))
        .collect();
    if names.len() == 1 {
        names.into_iter().next().unwrap().into()
    } else {
        String::new()
    }
}
fn call_id(v: &Value) -> &str {
    ["tool_call_id", "call_id", "tool_id"]
        .iter()
        .find_map(|k| v[k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("")
}
fn records<'a>(v: &'a Value, key: &str, failures: &mut Vec<String>) -> &'a [Value] {
    if !v[key]
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_object))
    {
        failures.push(format!("{key} must be a list of objects"));
        &[]
    } else {
        arr(&v[key])
    }
}
fn ordinal(v: &Value) -> Option<i64> {
    v["ordinal"].as_i64().filter(|n| *n >= 0)
}
fn write_receipt(v: &Value, id: &str, name: &str, decision: &str, binding: &str) -> Result<()> {
    strict_keys(
        v,
        &[
            "tool_call_id",
            "tool_name",
            decision,
            binding,
            "receipt_sha256",
        ],
        &[],
        "write receipt",
    )?;
    require(
        v["tool_call_id"] == id && v["tool_name"] == name,
        "write receipt bound to another call",
    )?;
    require(
        v[decision] == true,
        "write receipt does not affirm decision",
    )?;
    hash(&v[binding], binding)?;
    require(
        hash(&v["receipt_sha256"], "write receipt digest")?
            == digest(&unsigned(v, "receipt_sha256")),
        "write receipt self-digest mismatch",
    )
}
pub(super) fn validate(
    s: &Value,
    o: &Value,
    definitions: &BTreeMap<String, String>,
) -> (Vec<String>, Value) {
    let mut failures = vec![];
    let attempt = if arr(&o["attempt_ids"]).len() == 1 {
        match identifier(&o["attempt_ids"][0], "attempt_id") {
            Ok(v) => v,
            Err(e) => {
                failures.push(e.to_string());
                String::new()
            }
        }
    } else {
        failures.push("exactly one parent attempt_id is required".into());
        String::new()
    };
    let counts = &o["event_counts"];
    if !counts.is_object() {
        failures.push("event_counts must be an object".into())
    }
    if counts["run_finished"] != 1 {
        failures.push("event_counts must contain exactly one run_finished".into())
    }
    for event in ["run_error", "run_cancelled", "run_blocked"] {
        if counts[event].as_i64().is_some_and(|v| v != 0) {
            failures.push(format!("terminal failure event observed: {event}"));
        }
    }
    let terminals = if o["terminal_events"]
        .as_array()
        .is_some_and(|a| a.len() == 1)
    {
        arr(&o["terminal_events"])
    } else {
        failures.push("exactly one terminal event is required".into());
        &[]
    };
    for terminal in terminals {
        if !terminal.is_object() {
            failures.push("terminal event must be an object".into());
            continue;
        }
        if terminal["event_type"] != "run_finished" {
            failures.push("terminal event must be run_finished".into())
        }
        if terminal["attempt_id"] != attempt {
            failures.push("terminal event attempt_id is stale or missing".into())
        }
        let envelope = &terminal["terminal_envelope"];
        if !envelope.is_object() {
            failures.push("terminal_envelope must be an object".into())
        } else {
            if envelope["attempt_id"] != attempt {
                failures.push("terminal_envelope attempt_id is stale".into())
            }
            if envelope["run_id"] != terminal["run_id"] {
                failures.push("terminal_envelope run_id mismatch".into())
            }
            if envelope["status"] != "succeeded" {
                failures.push("terminal_envelope status is not succeeded".into())
            }
            if text(envelope, "tenant_id").is_empty() {
                failures.push("terminal_envelope tenant_id missing".into())
            }
        }
    }
    let starts = records(o, "subagent_starts", &mut failures);
    let finishes = records(o, "subagent_finishes", &mut failures);
    let start_ids: Vec<_> = starts.iter().map(|v| text(v, "agent_id")).collect();
    let finish_ids: Vec<_> = finishes.iter().map(|v| text(v, "agent_id")).collect();
    if start_ids.iter().chain(&finish_ids).any(|s| s.is_empty()) {
        failures.push("every subagent lifecycle event needs agent_id".into())
    }
    if start_ids.iter().collect::<BTreeSet<_>>().len() != start_ids.len()
        || finish_ids.iter().collect::<BTreeSet<_>>().len() != finish_ids.len()
    {
        failures.push("duplicate subagent lifecycle agent_id".into())
    }
    if start_ids.iter().collect::<BTreeSet<_>>() != finish_ids.iter().collect::<BTreeSet<_>>() {
        failures.push("subagent starts and finishes do not pair exactly".into())
    }
    if starts
        .iter()
        .chain(finishes)
        .any(|v| v["attempt_id"] != attempt)
    {
        failures.push("subagent lifecycle contains a stale attempt_id".into())
    }
    for v in finishes {
        let status = text(v, "status").to_lowercase();
        if status != "completed" {
            failures.push(format!(
                "subagent {} did not complete: {status}",
                text(v, "agent_id")
            ))
        }
        if ["failed", "cancelled", "blocked", "timeout"].contains(&status.as_str())
            || truth(&v["error"])
        {
            failures.push("subagent has failure evidence".into())
        }
    }
    let required: Vec<String> = arr(&s["required_agent_ids"])
        .iter()
        .map(|v| {
            let id = v.as_str().unwrap_or("");
            if id.contains(':') {
                id.into()
            } else {
                format!("builtin:{id}")
            }
        })
        .collect();
    let mut sorted_required = required.clone();
    sorted_required.sort();
    for (list, label) in [(starts, "starts"), (finishes, "finishes")] {
        let mut observed: Vec<_> = list.iter().map(identity).collect();
        observed.sort();
        if observed != sorted_required {
            failures.push(format!(
                "subagent {label} do not exactly match required agent identities"
            ));
        }
    }
    let profiles: BTreeSet<_> = required
        .iter()
        .filter(|p| !p.starts_with("builtin:"))
        .cloned()
        .collect();
    let observed_profiles: BTreeSet<_> = finishes
        .iter()
        .filter_map(|v| v["profile_id"].as_str().filter(|s| !s.is_empty()))
        .collect();
    for v in starts.iter().chain(finishes) {
        let profile = text(v, "profile_id");
        if profiles.contains(profile) {
            if hash(&v["definition_sha256"], "definition_sha256")
                .ok()
                .as_ref()
                != definitions.get(profile)
            {
                failures.push(format!(
                    "{profile} definition receipt does not match local plugin"
                ))
            }
            if text(v, "source_plugin") != profile.split(':').next().unwrap_or("") {
                failures.push(format!("{profile} source_plugin mismatch"));
            }
        }
    }
    let overlaps = records(o, "parallel_overlaps", &mut failures);
    if s["require_parallel"] == true && !overlaps.iter().any(|v| v["observed"] == true) {
        failures.push("required child execution has no observed interval overlap".into())
    }
    let tool_starts = records(o, "tool_starts", &mut failures);
    let tool_results = records(o, "tool_results", &mut failures);
    let texts = records(o, "text_events", &mut failures);
    let sequence = records(o, "event_sequence", &mut failures);
    let mut expected = vec![];
    for (kind, rows) in [
        ("subagent_started", starts),
        ("subagent_finished", finishes),
    ] {
        for v in rows {
            expected.push(json!({"ordinal":v["ordinal"],"event_type":kind}));
        }
    }
    for v in texts
        .iter()
        .chain(tool_starts)
        .chain(tool_results)
        .chain(terminals)
    {
        expected.push(json!({"ordinal":v["ordinal"],"event_type":v["event_type"]}));
    }
    expected.sort_by_key(|v| ordinal(v).unwrap_or(-1));
    let ordinals: Vec<_> = sequence.iter().map(ordinal).collect();
    if ordinals.iter().any(Option::is_none)
        || ordinals.iter().collect::<BTreeSet<_>>().len() != ordinals.len()
    {
        failures.push("event_sequence ordinals must be unique non-negative integers".into())
    }
    if sequence != expected {
        failures.push("event_sequence does not exactly match lifecycle receipts".into())
    }
    for v in texts {
        let result = (|| -> Result<()> {
            strict_keys(
                v,
                &["ordinal", "event_type", "content_sha256", "content_chars"],
                &[],
                "text event",
            )?;
            require(
                ["text_delta", "text_message_content"].contains(&text(v, "event_type"))
                    && ordinal(v).is_some(),
                "text event type or ordinal invalid",
            )?;
            hash(&v["content_sha256"], "text event digest")?;
            integer(&v["content_chars"], 0, 100000, "text event content_chars")?;
            Ok(())
        })();
        if let Err(e) = result {
            failures.push(e.to_string())
        }
    }
    let start_calls: Vec<_> = tool_starts.iter().map(call_id).collect();
    let result_calls: Vec<_> = tool_results.iter().map(call_id).collect();
    if start_calls
        .iter()
        .chain(&result_calls)
        .any(|id| id.is_empty())
    {
        failures.push("every tool lifecycle event needs a call id".into())
    }
    if start_calls.iter().collect::<BTreeSet<_>>().len() != start_calls.len() {
        failures.push("duplicate tool start call id".into())
    }
    if start_calls.iter().collect::<BTreeSet<_>>() != result_calls.iter().collect::<BTreeSet<_>>() {
        failures.push("tool starts and results do not pair exactly".into())
    }
    if tool_starts
        .iter()
        .chain(tool_results)
        .any(|v| v["attempt_id"] != attempt)
    {
        failures.push("tool lifecycle contains a stale attempt_id".into())
    }
    for start in tool_starts {
        let id = call_id(start);
        let results: Vec<_> = tool_results.iter().filter(|v| call_id(v) == id).collect();
        if results.is_empty() {
            continue;
        }
        let name = tool(start);
        let names: BTreeSet<_> = results.iter().map(|v| tool(v)).collect();
        if name.is_empty() || names.contains("") || names.len() != 1 {
            failures.push(format!(
                "tool call {id} has a missing or ambiguous exact name"
            ));
            continue;
        }
        if !names.contains(&name) {
            failures.push(format!("tool call {id} start/result names mismatch"))
        }
        if name != "spawn_subagent" {
            failures.push(format!("parent tool {name} is not allowed by scenario"))
        }
        if ordinal(start).is_none()
            || results
                .iter()
                .any(|v| ordinal(v).is_none() || ordinal(v) <= ordinal(start))
        {
            failures.push(format!("tool call {id} lifecycle ordering invalid"))
        }
    }
    if !required.is_empty()
        && tool_starts.iter().map(tool).collect::<Vec<_>>() != ["spawn_subagent"]
    {
        failures.push("delegating scenario requires exactly one spawn_subagent batch call".into())
    }
    if required.is_empty() && !tool_starts.is_empty() {
        failures.push("non-delegating scenario must not invoke a parent tool".into())
    }
    let mut effects = vec![];
    for v in tool_results {
        let name = tool(v);
        let status = text(v, "status").to_lowercase();
        if v["success"] == false
            || [
                "failed",
                "error",
                "cancelled",
                "blocked",
                "timeout",
                "side_effect_unknown",
            ]
            .contains(&status.as_str())
        {
            failures.push(format!("tool result failed: {name}"))
        }
        let effect = text(v, "side_effect_state");
        if !["none", "read_only", "write_known", "write_unknown"].contains(&effect) {
            failures.push("tool result lacks explicit typed side_effect_state".into());
            continue;
        }
        effects.push(effect);
        if effect == "write_unknown" {
            failures.push("unknown side-effect state is present".into())
        }
        if name == "spawn_subagent" && !["none", "read_only"].contains(&effect) {
            failures.push("spawn_subagent must have none or read_only side_effect_state".into())
        }
        if effect == "write_known" {
            for (field, decision, binding) in [
                ("approval_receipt", "approved", "scope_sha256"),
                ("readback_receipt", "verified", "observed_state_sha256"),
            ] {
                if let Err(e) = write_receipt(&v[field], call_id(v), &name, decision, binding) {
                    failures.push(e.to_string())
                }
            }
        }
    }
    for (start, finish) in starts.iter().zip(finishes) {
        if start["agent_id"] == finish["agent_id"]
            && (ordinal(start).is_none()
                || ordinal(finish).is_none()
                || ordinal(start) >= ordinal(finish))
        {
            failures.push("subagent lifecycle ordering invalid".into())
        }
    }
    let text_ordinals: Vec<_> = texts
        .iter()
        .filter(|v| v["content_chars"].as_i64().unwrap_or(0) > 0)
        .filter_map(ordinal)
        .collect();
    let terminal_ordinals: Vec<_> = terminals.iter().filter_map(ordinal).collect();
    if text_ordinals.is_empty()
        || terminal_ordinals.len() != 1
        || text_ordinals.iter().max() >= terminal_ordinals.first()
    {
        failures.push("final candidate text must precede the single terminal event".into())
    }
    if !required.is_empty() && !text_ordinals.is_empty() {
        let finish_ordinals: Vec<_> = finishes.iter().filter_map(ordinal).collect();
        let spawn_ordinals: Vec<_> = tool_results
            .iter()
            .filter(|v| {
                tool(v) == "spawn_subagent"
                    && ["tool_call_result", "tool_call_completed", "tool_result"]
                        .contains(&text(v, "event_type"))
                    && v["success"] == true
            })
            .filter_map(ordinal)
            .collect();
        if finish_ordinals.len() != required.len()
            || spawn_ordinals.len() != 1
            || finish_ordinals.iter().max() >= spawn_ordinals.first()
            || spawn_ordinals.first() >= text_ordinals.iter().max()
        {
            failures.push("delegation completion must precede parent synthesis text".into())
        }
    }
    (
        failures,
        json!({"scenario_id":s["scenario_id"],"attempt_id":attempt,"terminal_count":terminals.len(),"terminal_type":terminals.first().map(|v|&v["event_type"]),"completed_subagents":finishes.len(),"required_agent_ids":required,"observed_profile_ids":observed_profiles,"parallel_overlap_observed":overlaps.iter().any(|v|v["observed"]==true),"allowed_parent_tools":["spawn_subagent"],"tool_result_count":tool_results.len(),"tool_side_effect_states":effects}),
    )
}
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.),
    }
}
