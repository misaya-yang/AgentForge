//! Snapshot-compatible capability projection; no provider requests.
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, sync::LazyLock};
static ADAPTERS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/capability-adapters.json"))
        .expect("bundled adapter data")
});
static CATALOG: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../data/model-capability-catalog.json"))
        .expect("bundled catalog data")
});
#[derive(Debug, PartialEq)]
pub struct InvalidProfile;
type Result<T> = std::result::Result<T, InvalidProfile>;
fn check(ok: bool) -> Result<()> {
    if ok { Ok(()) } else { Err(InvalidProfile) }
}
fn object(value: &Value) -> Result<&Map<String, Value>> {
    value.as_object().ok_or(InvalidProfile)
}
fn one_of(value: &Value, options: &[&str]) -> bool {
    value.as_str().is_some_and(|s| options.contains(&s))
}
fn option_id(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 32
            && s.as_bytes()[0].is_ascii_lowercase()
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
    })
}
fn adapter(id: &Value, kind: &str) -> Result<&'static Value> {
    ADAPTERS
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == *id && a["kind"] == kind)
        .ok_or(InvalidProfile)
}
fn settings(spec: &Value, value: &Value) -> Result<()> {
    let settings = object(value)?;
    let schema = &spec["settings_schema"];
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    check(settings.keys().all(|key| properties.contains_key(key)))?;
    if let Some(required) = schema["required"].as_array() {
        check(
            required
                .iter()
                .all(|key| key.as_str().is_some_and(|key| settings.contains_key(key))),
        )?;
    }
    for (key, value) in settings {
        let rule = &properties[key];
        check(match rule["type"].as_str() {
            Some("boolean") => value.is_boolean(),
            Some("string") => value.is_string(),
            Some("integer") => value.is_i64() || value.is_u64(),
            _ => true,
        })?;
        if let Some(values) = rule["enum"].as_array() {
            check(values.contains(value))?;
        }
        if let Some(number) = value.as_i64() {
            if let Some(min) = rule["minimum"].as_i64() {
                check(number >= min)?;
            }
            if let Some(max) = rule["maximum"].as_i64() {
                check(number <= max)?;
            }
        } else if value.is_u64() && (rule.get("minimum").is_some() || rule.get("maximum").is_some())
        {
            return Err(InvalidProfile);
        }
    }
    Ok(())
}
pub fn safe(tools: bool, vision: bool) -> Value {
    json!({"schema_version":1,"wire_protocols":{"preferred":"chat_completions","supported":["chat_completions"]},"reasoning":{"adapter_id":"reasoning/none-v1","default_option":"off","options":[{"id":"off","label":"Off","aliases":[],"canonical_effort":"none","settings":{}}],"visibility":"none","replay_policy":"discard_after_turn"},"prompt_cache":{"adapter_id":"cache/none-v1","config":{}},"native_search":{"adapter_id":"search/none-v1","enabled":false,"config":{}},"tools":{"function_calling":tools,"parallel_calls":false,"strict_schema":false,"namespace_wire":"native","web_search_wire":"disabled"},"modalities":{"input":if vision{vec!["text","image"]}else{vec!["text"]},"output":["text"]},"streaming":{"text_deltas":true,"reasoning_deltas":false,"tool_call_deltas":tools}})
}
fn merge(base: &mut Value, patch: &Value) {
    if let (Some(base), Some(patch)) = (base.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if value.is_object() && base.get(key).is_some_and(Value::is_object) {
                merge(base.get_mut(key).unwrap(), value);
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}
pub fn validate(mut value: Value) -> Result<Value> {
    check(value["schema_version"] == 1)?;
    object(&value)?;
    if value.get("wire_protocols").is_none() {
        value["wire_protocols"] = safe(true, false)["wire_protocols"].clone();
    }
    let wires = &value["wire_protocols"];
    object(wires)?;
    let supported = wires["supported"].as_array().ok_or(InvalidProfile)?;
    check(
        !supported.is_empty()
            && supported
                .iter()
                .all(|v| one_of(v, &["chat_completions", "responses_v1", "anthropic_messages", "embeddings"]))
            && supported.contains(&wires["preferred"]),
    )?;
    check(
        supported
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>()
            .len()
            == supported.len(),
    )?;
    let reasoning = &value["reasoning"];
    object(reasoning)?;
    let spec = adapter(&reasoning["adapter_id"], "reasoning")?;
    let options = reasoning["options"].as_array().ok_or(InvalidProfile)?;
    check((1..=16).contains(&options.len()))?;
    let declared: BTreeSet<&str> = options.iter().filter_map(|o| o["id"].as_str()).collect();
    let mut ids = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    let empty = json!({});
    for option in options {
        object(option)?;
        check(option_id(&option["id"]) && option["id"] != "auto")?;
        check(ids.insert(option["id"].as_str().unwrap()))?;
        check(
            option["label"]
                .as_str()
                .is_some_and(|s| (1..=64).contains(&s.trim().chars().count())),
        )?;
        if !option["canonical_effort"].is_null() {
            check(one_of(
                &option["canonical_effort"],
                &[
                    "none", "minimal", "low", "medium", "high", "xhigh", "adaptive", "max", "ultra",
                ],
            ))?;
        }
        if let Some(raw) = option.get("aliases") {
            let values = raw.as_array().ok_or(InvalidProfile)?;
            check(values.len() <= 16)?;
            for alias in values {
                check(option_id(alias) && alias != "auto")?;
                let alias = alias.as_str().unwrap();
                check(!declared.contains(alias) && aliases.insert(alias))?;
            }
        }
        settings(spec, option.get("settings").unwrap_or(&empty))?;
    }
    check(
        reasoning["default_option"]
            .as_str()
            .is_some_and(|s| ids.contains(s)),
    )?;
    check(one_of(
        &reasoning["visibility"],
        &["none", "summary", "stream"],
    ))?;
    check(one_of(
        &reasoning["replay_policy"],
        &[
            "discard_after_turn",
            "preserve_during_tool_turn",
            "preserve_session",
        ],
    ))?;
    for kind in ["prompt_cache", "native_search"] {
        let section = &value[kind];
        object(section)?;
        settings(
            adapter(&section["adapter_id"], kind)?,
            section.get("config").unwrap_or(&empty),
        )?;
    }
    check(value["native_search"]["enabled"].is_boolean())?;
    let tools = value["tools"].as_object_mut().ok_or(InvalidProfile)?;
    tools.entry("namespace_wire").or_insert(json!("native"));
    tools.entry("web_search_wire").or_insert(json!("disabled"));
    check(
        ["function_calling", "parallel_calls", "strict_schema"]
            .iter()
            .all(|key| tools.get(*key).is_some_and(Value::is_boolean)),
    )?;
    check(
        one_of(&tools["namespace_wire"], &["native", "flatten"])
            && one_of(&tools["web_search_wire"], &["native", "disabled"]),
    )?;
    check(
        value["tools"]["web_search_wire"] != "native" || value["native_search"]["enabled"] == true,
    )?;
    object(&value["modalities"])?;
    for key in ["input", "output"] {
        check(
            value["modalities"][key]
                .as_array()
                .is_some_and(|items| !items.is_empty() && items.iter().all(Value::is_string)),
        )?;
    }
    object(&value["streaming"])?;
    check(
        ["text_deltas", "reasoning_deltas", "tool_call_deltas"]
            .iter()
            .all(|key| value["streaming"][*key].is_boolean()),
    )?;
    Ok(value)
}
fn profile_object(value: &Value) -> Value {
    let value = if let Some(raw) = value.as_str() {
        serde_json::from_str(raw).unwrap_or(Value::Null)
    } else {
        value.clone()
    };
    if value.is_object() { value } else { json!({}) }
}
pub fn builtin(provider: &str, model: &str) -> Option<Value> {
    let provider = provider.trim().to_lowercase();
    let model = model.trim().to_lowercase();
    CATALOG["models"]
        .as_array()?
        .iter()
        .find(|entry| {
            let model_match = entry["model_id"]
                .as_str()
                .is_some_and(|id| id.to_lowercase() == model)
                || entry["model_ids"].as_array().is_some_and(|ids| {
                    ids.iter()
                        .any(|id| id.as_str().is_some_and(|id| id.to_lowercase() == model))
                });
            model_match
                && entry["provider_keys"].as_array().is_some_and(|ids| {
                    ids.iter()
                        .any(|id| id.as_str().is_some_and(|id| id.to_lowercase() == provider))
                })
        })
        .and_then(|entry| validate(entry["capabilities"].clone()).ok())
}
pub fn effective(
    provider: &str,
    model: &str,
    catalog: &Value,
    overrides: &Value,
    tools: bool,
    vision: bool,
) -> Value {
    let mut catalog = profile_object(catalog);
    if catalog.as_object().unwrap().is_empty() {
        catalog = builtin(provider, model).unwrap_or_else(|| json!({}));
    }
    let overrides = profile_object(overrides);
    let mut value = safe(tools, vision);
    merge(&mut value, &catalog);
    merge(&mut value, &overrides);
    validate(value).unwrap_or_else(|_| safe(tools, vision))
}
