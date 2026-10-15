//! Structured capability form from ModelCapabilityEditor/modelCapabilities.ts.
use super::common::{lines, select_text, string_list};
use eframe::egui;
use serde_json::{Value, json};
pub fn safe_profile(tools: bool, vision: bool) -> Value {
    json!({"schema_version":1,"reasoning":{"adapter_id":"reasoning/none-v1","default_option":"off","options":[{"id":"off","label":"Off","aliases":[],"canonical_effort":"none","settings":{}}],"visibility":"none","replay_policy":"discard_after_turn"},"prompt_cache":{"adapter_id":"cache/none-v1","config":{}},"native_search":{"adapter_id":"search/none-v1","enabled":false,"config":{}},"tools":{"function_calling":tools,"parallel_calls":false,"strict_schema":false},"modalities":{"input":if vision{vec!["text","image"]}else{vec!["text"]},"output":["text"]},"streaming":{"text_deltas":true,"reasoning_deltas":false,"tool_call_deltas":tools}})
}
pub fn usable(p: &Value) -> bool {
    p["schema_version"] == 1
        && p["reasoning"].is_object()
        && p["reasoning"]["options"].is_array()
        && [
            "prompt_cache",
            "native_search",
            "tools",
            "modalities",
            "streaming",
        ]
        .iter()
        .all(|key| p[*key].is_object())
}
fn valid_id(id: &str) -> bool {
    id != "auto"
        && (1..=32).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
pub fn validate_options(p: &Value) -> bool {
    let Some(options) = p["reasoning"]["options"].as_array() else {
        return false;
    };
    let declared: std::collections::HashSet<&str> =
        options.iter().filter_map(|o| o["id"].as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut aliases = std::collections::HashSet::new();
    for option in options {
        let Some(id) = option["id"].as_str() else {
            return false;
        };
        let label = option["label"].as_str().unwrap_or("").trim();
        if !valid_id(id)
            || !seen.insert(id)
            || label.is_empty()
            || label.encode_utf16().count() > 64
        {
            return false;
        }
        if let Some(values) = option["aliases"].as_array() {
            for alias in values {
                let Some(alias) = alias.as_str() else {
                    return false;
                };
                if !valid_id(alias) || declared.contains(alias) || !aliases.insert(alias) {
                    return false;
                }
            }
        } else {
            return false;
        }
    }
    true
}
fn text(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str) {
    let mut input = value[key].as_str().unwrap_or("").to_owned();
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.text_edit_singleline(&mut input).changed() {
            value[key] = json!(input);
        }
    });
}
fn boolean(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str) {
    let mut input = value[key].as_bool().unwrap_or(false);
    if ui.checkbox(&mut input, label).changed() {
        value[key] = json!(input);
    }
}
fn choice(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str, choices: &[&str]) {
    let mut input = value[key].as_str().unwrap_or(choices[0]).to_owned();
    ui.label(label);
    select_text(ui, key, &mut input, choices);
    value[key] = json!(input);
}
fn strings(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str) {
    let mut input = string_list(&value[key]);
    ui.label(label);
    if ui.text_edit_multiline(&mut input).changed() {
        value[key] = json!(lines(&input));
    }
}
fn object(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str) {
    let id = ui.make_persistent_id(("capability-object", key, label));
    let stored = ui
        .ctx()
        .data_mut(|data| data.get_temp::<(String, Value)>(id));
    let (mut input, baseline) = stored
        .filter(|(_, baseline)| baseline == &value[key])
        .unwrap_or_else(|| {
            (
                serde_json::to_string_pretty(&value[key]).unwrap_or_default(),
                value[key].clone(),
            )
        });
    ui.label(label);
    ui.add(
        egui::TextEdit::multiline(&mut input)
            .code_editor()
            .desired_rows(3),
    );
    let parsed = serde_json::from_str::<Value>(&input)
        .ok()
        .filter(Value::is_object);
    let changed = parsed.as_ref() != Some(&value[key]);
    if changed {
        ui.label("Unapplied settings draft");
    }
    let mut accepted = baseline;
    if ui
        .add_enabled(
            changed && parsed.is_some(),
            egui::Button::new("Apply settings object to profile"),
        )
        .clicked()
    {
        if let Some(parsed) = parsed {
            value[key] = parsed.clone();
            accepted = parsed;
        }
    }
    ui.ctx()
        .data_mut(|data| data.insert_temp(id, (input, accepted)));
}
fn adapter(ui: &mut egui::Ui, value: &mut Value, kind: &str, adapters: &Value) {
    text(ui, value, "adapter_id", "Adapter ID");
    if let Some(rows) = adapters.as_array() {
        for row in rows.iter().filter(|r| r["kind"] == kind) {
            if ui
                .button(row["label"].as_str().unwrap_or("Adapter"))
                .clicked()
            {
                value["adapter_id"] = row["id"].clone();
            }
        }
    }
}
pub fn ui(ui: &mut egui::Ui, profile: &mut Value, adapters: &Value) -> bool {
    if !usable(profile) {
        ui.label("Profile is incomplete. Load a complete effective profile or initialize the safe defaults.");
        return false;
    }
    let before = profile.clone();
    egui::CollapsingHeader::new("Reasoning options").show(ui,|ui|{adapter(ui,&mut profile["reasoning"],"reasoning",adapters);text(ui,&mut profile["reasoning"],"default_option","Default option ID");choice(ui,&mut profile["reasoning"],"visibility","Reasoning visibility",&["none","summary","stream"]);choice(ui,&mut profile["reasoning"],"replay_policy","Replay policy",&["discard_after_turn","preserve_during_tool_turn","preserve_session"]);if let Some(options)=profile["reasoning"]["options"].as_array_mut(){let mut remove=None;for(index,option)in options.iter_mut().enumerate(){ui.push_id(index,|ui|{egui::Frame::group(ui.style()).show(ui,|ui|{text(ui,option,"id","Option ID");text(ui,option,"label","Label");strings(ui,option,"aliases","Aliases, one per line");choice(ui,option,"canonical_effort","Canonical effort",&["none","minimal","low","medium","high","xhigh","adaptive","max","ultra"]);object(ui,option,"settings","Adapter settings");if ui.button("Remove option").clicked(){remove=Some(index);}});});}if let Some(index)=remove{options.remove(index);}if ui.button("Add reasoning option").clicked(){options.push(json!({"id":"","label":"","aliases":[],"canonical_effort":"medium","settings":{}}));}}});
    egui::CollapsingHeader::new("Prompt cache and native search").show(ui, |ui| {
        ui.push_id("cache", |ui| {
            adapter(ui, &mut profile["prompt_cache"], "prompt_cache", adapters);
            object(
                ui,
                &mut profile["prompt_cache"],
                "config",
                "Cache configuration",
            );
        });
        ui.push_id("search", |ui| {
            adapter(ui, &mut profile["native_search"], "native_search", adapters);
            boolean(
                ui,
                &mut profile["native_search"],
                "enabled",
                "Native search enabled",
            );
            object(
                ui,
                &mut profile["native_search"],
                "config",
                "Search configuration",
            );
        });
    });
    egui::CollapsingHeader::new("Tools, modalities and streaming").show(ui, |ui| {
        for (key, label) in [
            ("function_calling", "Function calling"),
            ("parallel_calls", "Parallel calls"),
            ("strict_schema", "Strict schema"),
        ] {
            boolean(ui, &mut profile["tools"], key, label);
        }
        strings(ui, &mut profile["modalities"], "input", "Input modalities");
        strings(
            ui,
            &mut profile["modalities"],
            "output",
            "Output modalities",
        );
        for (key, label) in [
            ("text_deltas", "Text deltas"),
            ("reasoning_deltas", "Reasoning deltas"),
            ("tool_call_deltas", "Tool-call deltas"),
        ] {
            boolean(ui, &mut profile["streaming"], key, label);
        }
    });
    *profile != before
}
