//! Retained model transcript/stream scope assertions from source projection suites.
use ai_platform_gateway::migration_agent::{
    model::validate_transcript,
    model_projection::{ChatProjector, NativeValidator},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn history(output: &str) -> Value {
    json!([{"type":"function_call","call_id":"call-unknown","name":"execute_bash","arguments":"{\"command\":\"pwd\"}"},{"type":"function_call_output","call_id":"call-unknown","output":output}])
}
#[test]
fn unadvertised_call_allows_only_exact_runtime_rejection_receipt() {
    for enabled in [false, true] {
        let allowed = if enabled {
            BTreeSet::from(["read".into()])
        } else {
            BTreeSet::new()
        };
        validate_transcript(
            &history("unsupported call: execute_bash"),
            &allowed,
            &BTreeSet::new(),
        )
        .unwrap();
        let error = validate_transcript(&history("command completed"), &allowed, &BTreeSet::new())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("RUNTIME_TOOL_TRANSCRIPT_INVALID")
        );
    }
}
#[test]
fn named_output_must_match_original_call_namespace() {
    let history = json!([{"type":"function_call","name":"read","namespace":"docs","call_id":"call","arguments":"{}"},{"type":"function_call_output","name":"read","namespace":"mail","call_id":"call","output":"content"}]);
    assert!(
        validate_transcript(
            &history,
            &BTreeSet::new(),
            &BTreeSet::from([("docs".into(), "read".into())])
        )
        .unwrap_err()
        .to_string()
        .contains("RUNTIME_TOOL_TRANSCRIPT_INVALID")
    );
}
#[test]
fn native_stream_rejects_wrong_namespace_unadvertised_tools_and_search() {
    let alias = "ns_fixture_read";
    for item in [
        json!({"type":"function_call","name":"exec_command"}),
        json!({"type":"function_call","name":"read","namespace":"mail"}),
        json!({"type":"function_call","name":alias,"namespace":"mail"}),
        json!({"type":"web_search_call"}),
    ] {
        let mut validator = NativeValidator::new(
            BTreeMap::from([(alias.into(), ("docs".into(), "read".into()))]),
            BTreeSet::from([(Some("docs".into()), "read".into())]),
            false,
            false,
        );
        validator
            .consume(&json!({"type":"response.created","sequence_number":0}).to_string())
            .unwrap();
        let error = validator
            .consume(
                &json!({"type":"response.output_item.added","sequence_number":1,"item":item})
                    .to_string(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH")
        );
    }
}
fn delta(calls: Value) -> Value {
    json!({"choices":[{"delta":{"tool_calls":calls}}]})
}
fn items(frames: Vec<axum::body::Bytes>) -> Vec<Value> {
    frames
        .into_iter()
        .filter_map(|frame| {
            let frame = std::str::from_utf8(&frame).unwrap();
            let value: Value =
                serde_json::from_str(frame.split("data: ").nth(1).unwrap().trim()).unwrap();
            value["type"]
                .as_str()
                .filter(|kind| kind.starts_with("response.output_item."))
                .map(|_| value["item"].clone())
        })
        .collect()
}
#[test]
fn split_tool_name_waits_for_provider_call_identity_and_rejects_duplicate_id() {
    let mut projector = ChatProjector::new(
        "model",
        1,
        BTreeMap::new(),
        BTreeSet::from([(None, "get_data".into())]),
    );
    assert!(
        projector
            .consume(&delta(json!([{"index":0,"function":{"name":"get"}}])))
            .unwrap()
            .is_empty()
    );
    assert!(
        projector
            .consume(&delta(
                json!([{"index":0,"function":{"name":"_data","arguments":"{"}}])
            ))
            .unwrap()
            .is_empty()
    );
    let mut frames = projector
        .consume(&delta(
            json!([{"index":0,"id":"actual_call","function":{"arguments":"}"}}]),
        ))
        .unwrap();
    frames.extend(projector.complete().unwrap().0);
    let items = items(frames);
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item["call_id"] == "actual_call" && item["name"] == "get_data")
    );
    assert!(
        projector
            .consume(&delta(
                json!([{"index":1,"id":"actual_call","function":{"name":"get_data"}}])
            ))
            .unwrap_err()
            .to_string()
            .contains("RUNTIME_PROVIDER_STREAM_INVALID")
    );
}
#[test]
fn tool_argument_limit_counts_utf8_bytes() {
    let mut projector = ChatProjector::new(
        "model",
        1,
        BTreeMap::new(),
        BTreeSet::from([(None, "lookup".into())]),
    );
    let error=projector.consume(&delta(json!([{"index":0,"id":"call-1","function":{"name":"lookup","arguments":"字".repeat(1024*1024/3+1)}}]))).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT")
    );
}

fn authorized() -> ai_platform_gateway::migration_agent::model::AuthorizedCall {
    use ai_platform_gateway::migration_agent::{Scope, model::AuthorizedCall};
    AuthorizedCall {
        call_id: "call".into(),
        lease_id: "lease".into(),
        run_id: "run".into(),
        scope: Scope {
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: "s".into(),
        },
        model_id: "model".into(),
        provider_id: "provider".into(),
        provider_revision: "pinned".into(),
        estimated_input_tokens: 1,
        reserved_output_tokens: 128,
        snapshot: json!({"capabilities":{"tools":{"namespace_wire":"flatten","web_search_wire":"native"},"native_search":{"enabled":true},"reasoning":{"adapter_id":"reasoning/none-v1","default_option":"minimal","options":[{"id":"minimal","settings":{}}]}},"reasoning":{"effective_option":"minimal"},"readonly_capabilities":{}}),
    }
}
fn tool(name: &str) -> Value {
    json!({"type":"function","name":name,"description":name,"parameters":{"type":"object","properties":{}}})
}
#[test]
fn chat_flattens_namespace_without_native_search_and_native_none_omits_tools() {
    use ai_platform_gateway::migration_agent::model::provider_body;
    let body = json!({"input":[{"role":"user","content":"hello"}],"tools":[tool("lookup"),{"type":"web_search"},{"type":"namespace","name":"mcp","tools":[tool("write")]}]});
    let mut call = authorized();
    let (chat, aliases, _, _) = provider_body(&body, &call, false).unwrap();
    let tools = chat["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["function"]["name"], "lookup");
    let alias = tools[1]["function"]["name"].as_str().unwrap();
    assert_eq!(aliases[alias], ("mcp".into(), "write".into()));
    assert!(tools.iter().all(|t| t["type"] == "function"));
    call.snapshot["readonly_capabilities"]["responses_tool_choice"] = json!("none");
    let (native, _, _, _) = provider_body(&body, &call, true).unwrap();
    assert!(native.get("tools").is_none());
    assert!(native.get("tool_choice").is_none());
}
#[test]
fn namespace_parallel_history_roundtrip_is_detached() {
    use ai_platform_gateway::migration_agent::model::provider_body;
    let history = json!([{"type":"function_call","name":"read","namespace":"docs","call_id":"docs","arguments":"{}"},{"type":"function_call","name":"read","namespace":"mail","call_id":"mail","arguments":"{}"},{"type":"function_call_output","call_id":"docs","output":"docs"},{"type":"function_call_output","call_id":"mail","output":"mail"}]);
    let body = json!({"input":history,"tools":[{"type":"namespace","name":"docs","tools":[tool("read")]},{"type":"namespace","name":"mail","tools":[tool("read")]}]});
    let (chat, aliases, _, _) = provider_body(&body, &authorized(), false).unwrap();
    let messages = chat["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    let calls = messages[0]["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    for (index, namespace) in ["docs", "mail"].into_iter().enumerate() {
        assert_eq!(
            aliases[calls[index]["function"]["name"].as_str().unwrap()],
            (namespace.into(), "read".into())
        );
        assert_eq!(messages[index + 1]["tool_call_id"], namespace);
    }
    assert_eq!(history[0]["namespace"], "docs");
}
#[test]
fn projected_items_restore_namespace_without_private_byte_counters() {
    let alias = "ns_fixture_read";
    let aliases = BTreeMap::from([(alias.into(), ("docs".into(), "read".into()))]);
    let mut projector = ChatProjector::new(
        "model",
        1,
        aliases,
        BTreeSet::from([(Some("docs".into()), "read".into())]),
    );
    let input =
        delta(json!([{"index":0,"id":"call-1","function":{"name":alias,"arguments":"{}"}}]));
    let mut frames = projector.consume(&input).unwrap();
    frames.extend(projector.complete().unwrap().0);
    let items = items(frames);
    assert!(!items.is_empty());
    assert!(items.iter().all(|item| item["name"] == "read"
        && item["namespace"] == "docs"
        && item.get("_argument_bytes").is_none()));
    let mut denied = ChatProjector::new("model", 1, BTreeMap::new(), BTreeSet::new());
    assert!(
        denied
            .consume(&input)
            .unwrap_err()
            .to_string()
            .contains("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH")
    );
}
#[test]
fn current_tool_subset_keeps_completed_history() {
    use ai_platform_gateway::migration_agent::model::provider_body;
    let history = json!([{"type":"function_call","name":"read","call_id":"prior","arguments":"{}"},{"type":"function_call_output","call_id":"prior","output":"previous read"}]);
    for choice in ["auto", "none"] {
        let mut call = authorized();
        call.snapshot["readonly_capabilities"] =
            json!({"responses_tool_names":["lookup"],"responses_tool_choice":choice});
        let (body, _, _, _) = provider_body(
            &json!({"input":history,"tools":[tool("lookup"),tool("read")]}),
            &call,
            true,
        )
        .unwrap();
        assert_eq!(body["input"], history);
        let names: Vec<_> = body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            if choice == "auto" {
                vec!["lookup"]
            } else {
                vec![]
            }
        );
    }
}
#[test]
fn multiple_text_tool_outputs_replay_on_both_wires_and_image_output_fails_closed() {
    use ai_platform_gateway::migration_agent::model::provider_body;
    let output =
        json!([{"type":"input_text","text":"part one"},{"type":"input_text","text":" + part two"}]);
    let mut history = json!([{"type":"function_call","name":"lookup","call_id":"call","arguments":"{}"},{"type":"function_call_output","call_id":"call","output":output}]);
    let request = json!({"input":history,"tools":[tool("lookup")]});
    let (native, _, _, _) = provider_body(&request, &authorized(), true).unwrap();
    assert_eq!(native["input"][1]["output"], output);
    let (chat, _, _, _) = provider_body(&request, &authorized(), false).unwrap();
    assert_eq!(
        chat["messages"].as_array().unwrap().last().unwrap()["content"],
        "part one + part two"
    );
    history[1]["output"] = json!([{"type":"input_image","image_url":"hidden"}]);
    let error = provider_body(
        &json!({"input":history,"tools":[tool("lookup")]}),
        &authorized(),
        false,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("RUNTIME_TOOL_TRANSCRIPT_INVALID")
    );
}

#[test]
fn unadvertised_history_provider_wire_preserves_rejection_and_exact_disabled_error() {
    use ai_platform_gateway::migration_agent::model::provider_body;
    for enabled in [true, false] {
        let tools = if enabled { vec![tool("read")] } else { vec![] };
        let (body, _, _, _) = provider_body(
            &json!({"input":history("unsupported call: execute_bash"),"tools":tools}),
            &authorized(),
            true,
        )
        .unwrap();
        assert_eq!(body["input"][1]["output"], "unsupported call: execute_bash");
        if !enabled {
            assert!(
                !body["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|tool| tool["type"] == "function")
            );
        }
        let error = provider_body(
            &json!({"input":history("command completed"),"tools":tools}),
            &authorized(),
            true,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(if enabled {
                "TRANSCRIPT"
            } else {
                "TOOLS_NOT_ENABLED"
            }),
            "{error}"
        );
    }
}
