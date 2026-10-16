//! Artifact-backed Gemini history. Persist pointers; inflate only for provider replay.
//! The caller supplies an owner-scoped downloader. This module never resolves a
//! bare artifact ID through an unscoped repository and never logs signatures.
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::future::join_all;
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future};
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.),
    }
}
pub fn append_image_turns(
    history: &mut Vec<Value>,
    user_text: &str,
    result_image: Option<&Value>,
    result_text: Option<&str>,
    artifact_id: Option<&str>,
) {
    let Some(image) = result_image else { return };
    history.push(json!({"role":"user","text":user_text}));
    let mut turn = json!({"role":"model","mime_type":image.get("mime_type").cloned().unwrap_or(json!("image/jpeg"))});
    if let Some(id) = artifact_id.filter(|v| !v.is_empty()) {
        turn["artifact_id"] = json!(id);
    }
    if let Some(text) = result_text.filter(|v| !v.is_empty()) {
        turn["text"] = json!(text);
    }
    if let Some(signature) = image.get("thought_signature").filter(|v| truthy(v)) {
        turn["thought_signature"] = signature.clone();
    }
    history.push(turn);
}
pub fn build_gemini_contents_from_history(history: &[Value], new_user_text: &str) -> Vec<Value> {
    let mut contents = Vec::new();
    for turn in history {
        let mut parts = Vec::new();
        if let Some(text) = turn.get("text").filter(|v| truthy(v)) {
            parts.push(json!({"text":text}));
        }
        let mime = turn
            .get("mime_type")
            .cloned()
            .unwrap_or(json!("image/jpeg"));
        let image = if let Some(data) = turn.get("image_base64").filter(|v| truthy(v)) {
            Some(json!({"inlineData":{"mimeType":mime,"data":data}}))
        } else {
            turn.get("file_uri")
                .filter(|v| truthy(v))
                .map(|uri| json!({"fileData":{"fileUri":uri,"mimeType":mime}}))
        };
        if let Some(mut image) = image {
            if let Some(signature) = turn.get("thought_signature").filter(|v| truthy(v)) {
                image["thoughtSignature"] = signature.clone();
            }
            parts.push(image);
        }
        if !parts.is_empty() {
            contents.push(
                json!({"role":turn.get("role").cloned().unwrap_or(json!("user")),"parts":parts}),
            );
        }
    }
    contents.push(json!({"role":"user","parts":[{"text":new_user_text}]}));
    contents
}
/// Errors/missing/empty downloads degrade that anchor only, as source gather
/// return_exceptions does. All downloads begin concurrently and input is unchanged.
pub async fn inflate_history_with_bytes<F, Fut, E>(history: &[Value], download: F) -> Vec<Value>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Option<Vec<u8>>, E>>,
{
    let ids: Vec<String> = history
        .iter()
        .filter(|turn| turn["role"] == "model" && !truthy(&turn["image_base64"]))
        .filter_map(|turn| {
            turn["artifact_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
        .collect();
    let fetched = join_all(ids.iter().cloned().map(download)).await;
    let mut bytes = BTreeMap::new();
    for (id, result) in ids.into_iter().zip(fetched) {
        if let Ok(Some(value)) = result {
            if !value.is_empty() {
                bytes.insert(id, value);
            }
        }
    }
    history
        .iter()
        .map(|turn| {
            let mut out = turn.clone();
            if turn["role"] == "model" && !truthy(&turn["image_base64"]) {
                if let Some(data) = turn["artifact_id"].as_str().and_then(|id| bytes.get(id)) {
                    out["image_base64"] = json!(STANDARD.encode(data));
                }
            }
            out
        })
        .collect()
}
