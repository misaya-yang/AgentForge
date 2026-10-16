//! Model capability source assertions at actual validation/catalog boundaries.
use ai_platform_gateway_foundation::capabilities;
use serde_json::{Value, json};
use std::collections::BTreeSet;
#[test]
fn capability_validation_rejects_malformed_values_and_native_search_without_permission() {
    let mut profile = capabilities::safe(true, false);
    profile["tools"]["web_search_wire"] = json!("native");
    assert!(capabilities::validate(profile).is_err());
    for path in [
        "/reasoning/default_option",
        "/reasoning/visibility",
        "/reasoning/replay_policy",
        "/reasoning/options/0/canonical_effort",
        "/reasoning/options/0/label",
    ] {
        let mut profile = capabilities::safe(true, false);
        *profile.pointer_mut(path).unwrap() = json!({"bad":"value"});
        assert!(capabilities::validate(profile).is_err(), "{path}");
    }
    let mut profile = capabilities::safe(true, false);
    profile["reasoning"] = json!({"adapter_id":"reasoning/deepseek-thinking-effort-v1","default_option":"bad","options":[{"id":"bad","label":"Bad","aliases":[],"canonical_effort":"low","settings":{"enabled":true,"effort":"low"}}],"visibility":"summary","replay_policy":"preserve_during_tool_turn"});
    assert!(capabilities::validate(profile).is_err());
}
#[test]
fn reasoning_option_ids_cannot_shadow_aliases_or_reserved_auto() {
    for reverse in [false, true] {
        let mut p = capabilities::safe(true, false);
        let low =
            json!({"id":"low","label":"Low","aliases":[],"canonical_effort":"low","settings":{}});
        let high = json!({"id":"high","label":"High","aliases":["low"],"canonical_effort":"high","settings":{}});
        p["reasoning"]["default_option"] = json!("high");
        p["reasoning"]["options"] = if reverse {
            json!([high, low])
        } else {
            json!([low, high])
        };
        assert!(capabilities::validate(p).is_err());
    }
    for field in ["id", "aliases"] {
        let mut p = capabilities::safe(true, false);
        p["reasoning"]["options"][0][field] = if field == "id" {
            json!("auto")
        } else {
            json!(["auto"])
        };
        assert!(capabilities::validate(p).is_err());
    }
}
#[test]
fn bundled_catalog_and_adapter_keys_are_unique_valid_and_search_is_explicit() {
    let adapters: Value =
        serde_json::from_str(include_str!("../data/capability-adapters.json")).unwrap();
    let adapters = adapters.as_array().unwrap();
    let ids = adapters
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), adapters.len());
    assert_eq!(
        adapters
            .iter()
            .map(|v| v["kind"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["reasoning", "prompt_cache", "native_search"])
    );
    let catalog: Value =
        serde_json::from_str(include_str!("../data/model-capability-catalog.json")).unwrap();
    let entries = catalog["models"].as_array().unwrap();
    assert!(!entries.is_empty());
    let mut pairs = BTreeSet::new();
    for entry in entries {
        let p = &entry["capabilities"];
        assert!(p.is_object());
        capabilities::validate(p.clone()).unwrap();
        let options = p["reasoning"]["options"].as_array().unwrap();
        let mut keys = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for option in options {
            let id = option["id"].as_str().unwrap();
            assert!(keys.insert(id));
            ids.insert(id);
            for alias in option["aliases"].as_array().unwrap() {
                assert!(keys.insert(alias.as_str().unwrap()));
            }
        }
        assert!(!keys.contains("auto"));
        assert!(ids.contains(p["reasoning"]["default_option"].as_str().unwrap()));
        let mut models = BTreeSet::from([entry["model_id"].as_str().unwrap().to_lowercase()]);
        if let Some(aliases) = entry["model_ids"].as_array() {
            models.extend(aliases.iter().map(|v| v.as_str().unwrap().to_lowercase()));
        }
        for provider in entry["provider_keys"].as_array().unwrap() {
            for model in &models {
                assert!(pairs.insert((provider.as_str().unwrap().to_lowercase(), model.clone())));
            }
        }
    }
    let p = capabilities::builtin("dashscope-intl", "qwen3.7-plus").unwrap();
    assert_eq!(
        p["native_search"],
        json!({"adapter_id":"search/dashscope-native-v1","enabled":true,"config":{}})
    );
    assert_eq!(p["tools"]["web_search_wire"], "native");
    assert_eq!(p["wire_protocols"]["preferred"], "responses_v1");
}
