use super::*;
use std::collections::BTreeMap;
pub(super) fn parse(surface: Surface, body: Bytes) -> LlmResult<Invocation> {
    let raw = raw_json::RawObject::parse(&body)?;
    let payload = raw.value;
    if payload["model"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s.len() > 512)
    {
        return Err(LlmError::new(400, "llm_model_required"));
    }
    let stream = payload
        .get("stream")
        .map(|v| v.as_bool().ok_or(LlmError::new(400, "llm_stream_invalid")))
        .transpose()?
        .unwrap_or(false);
    match surface {
        Surface::Embeddings if stream || payload.get("input").is_none_or(Value::is_null) => {
            return Err(LlmError::new(400, "llm_embedding_input_required"));
        }
        Surface::Chat | Surface::Messages if !payload["messages"].is_array() => {
            return Err(LlmError::new(400, "llm_messages_required"));
        }
        _ => {}
    }
    Ok(Invocation {
        client_request_id: None,
        external_audit: false,
        surface,
        body,
        payload,
        stream,
        request_id: format!("llm_{}", uuid::Uuid::new_v4().simple()),
    })
}
pub(super) fn allowed_model(actor: &Actor, model: &str, provider: &str) -> bool {
    actor.allowed_models.is_empty()
        || crate::service_access_policy::service_scope_matches(
            &json!(actor.allowed_models),
            &[json!(model), json!(format!("{provider}/{model}"))],
        )
}
pub(super) fn model_access(actor: &Actor, model: &Value) -> bool {
    let admin = actor.tier == "admin" || actor.roles.iter().any(|r| r == "admin");
    match model["access_level"].as_str() {
        Some("public") => true,
        Some("premium") => admin || ["premium", "enterprise"].contains(&actor.tier.as_str()),
        Some("admin") => admin,
        _ => false,
    }
}
pub(super) fn select_model(actor: &Actor, requested: &str, rows: Vec<Value>) -> LlmResult<Value> {
    let matches = rows
        .into_iter()
        .filter(|row| {
            row["tenant_id"] == actor.tenant && {
                let model = row["model_id"].as_str().unwrap_or("");
                let provider = row["provider_id"].as_str().unwrap_or("");
                requested == model || requested == format!("{provider}/{model}")
            }
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(LlmError::new(404, "llm_model_not_found_or_ambiguous"));
    }
    let row = matches.into_iter().next().unwrap();
    if row["is_enabled"] != true
        || !model_access(actor, &row)
        || !allowed_model(
            actor,
            row["model_id"].as_str().unwrap_or(""),
            row["provider_id"].as_str().unwrap_or(""),
        )
    {
        return Err(LlmError::new(403, "llm_model_forbidden"));
    }
    Ok(row)
}
pub(super) fn profile(model: &Value) -> Value {
    crate::capabilities::effective(
        model["provider_id"].as_str().unwrap_or(""),
        model["model_id"].as_str().unwrap_or(""),
        &model["catalog_capabilities"],
        &model["capability_overrides"],
        model["supports_tools"] == true,
        model["supports_vision"] == true,
    )
}
pub(super) fn bind(
    invocation: &Invocation,
    model: &Value,
    provider: &Value,
) -> LlmResult<(Bytes, u64, Value)> {
    let profile = profile(model);
    let surface = invocation.surface;
    let protocol = provider["api_type"].as_str().unwrap_or("");
    let same = match surface {
        Surface::Messages => protocol == "anthropic",
        _ => matches!(protocol, "openai" | "openai-compatible"),
    };
    if !same
        || !profile["wire_protocols"]["supported"]
            .as_array()
            .is_some_and(|v| v.contains(&json!(surface.wire())))
    {
        return Err(LlmError::new(409, "llm_protocol_or_capability_unsupported"));
    }
    let kind = model["model_type"].as_str().unwrap_or("chat");
    if (surface == Surface::Embeddings) != matches!(kind, "embedding" | "multimodal_embedding") {
        return Err(LlmError::new(409, "llm_model_kind_unsupported"));
    }
    let p = &invocation.payload;
    if p.get("tools")
        .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
        && profile["tools"]["function_calling"] != true
        || p["parallel_tool_calls"] == true && profile["tools"]["parallel_calls"] != true
    {
        return Err(LlmError::new(409, "llm_tool_capability_unsupported"));
    }
    fn supported_content(value: &Value, profile: &Value) -> bool {
        match value {
            Value::Array(values) => values.iter().all(|v| supported_content(v, profile)),
            Value::Object(values) => {
                let modality = match value["type"].as_str() {
                    Some("image" | "image_url" | "input_image") => Some("image"),
                    Some("input_audio" | "audio") => Some("audio"),
                    Some("video" | "video_url") => Some("video"),
                    _ => None,
                };
                modality.is_none_or(|m| {
                    profile["modalities"]["input"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(m)))
                }) && values.values().all(|v| supported_content(v, profile))
            }
            _ => true,
        }
    }
    if !supported_content(p, &profile) {
        return Err(LlmError::new(409, "llm_modality_unsupported"));
    }
    if invocation.stream && profile["streaming"]["text_deltas"] != true {
        return Err(LlmError::new(409, "llm_stream_capability_unsupported"));
    }
    if let Some(tools) = p["tools"].as_array() {
        for tool in tools {
            if (tool["strict"] == true || tool["function"]["strict"] == true)
                && profile["tools"]["strict_schema"] != true
                || tool["type"]
                    .as_str()
                    .is_some_and(|kind| kind.starts_with("web_search"))
                    && (profile["native_search"]["enabled"] != true
                        || profile["tools"]["web_search_wire"] != "native")
            {
                return Err(LlmError::new(409, "llm_tool_capability_unsupported"));
            }
        }
    }
    let mut changes = BTreeMap::new();
    changes.insert("model".into(), model["model_id"].clone());
    let output = if surface == Surface::Embeddings {
        0
    } else {
        let ceiling = model["max_output_tokens"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(LlmError::new(503, "llm_model_output_ceiling_missing"))?;
        let fields = if surface == Surface::Chat {
            vec!["max_tokens", "max_completion_tokens"]
        } else {
            vec!["max_tokens"]
        };
        let mut cap = None;
        for field in fields {
            if let Some(value) = p.get(field) {
                let value = value
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= ceiling)
                    .ok_or(LlmError::new(400, "llm_output_budget_invalid"))?;
                if cap.is_some_and(|n| n != value) {
                    return Err(LlmError::new(400, "llm_output_budget_ambiguous"));
                }
                cap = Some(value);
            }
        }
        let cap = cap.unwrap_or(ceiling);
        if cap == ceiling
            && p.get("max_tokens").is_none()
            && (surface == Surface::Messages || p.get("max_completion_tokens").is_none())
        {
            changes.insert("max_tokens".into(), json!(cap));
        }
        let n = p
            .get("n")
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n > 0 && *n <= 128)
                    .ok_or(LlmError::new(400, "llm_choice_count_invalid"))
            })
            .transpose()?
            .unwrap_or(1);
        if surface == Surface::Messages && n != 1 || p.get("best_of").is_some() {
            return Err(LlmError::new(409, "llm_candidate_semantics_unsupported"));
        }
        cap.checked_mul(n)
            .filter(|n| *n <= 1_000_000)
            .ok_or(LlmError::new(400, "llm_output_budget_invalid"))?
    };
    if invocation.stream && surface == Surface::Chat {
        let mut options = p
            .get("stream_options")
            .map(|v| {
                v.as_object()
                    .cloned()
                    .ok_or(LlmError::new(400, "llm_stream_options_invalid"))
            })
            .transpose()?
            .unwrap_or_default();
        options.insert("include_usage".into(), json!(true));
        changes.insert("stream_options".into(), Value::Object(options));
    }
    let body = raw_json::RawObject::parse(&invocation.body)?.patch(&invocation.body, &changes)?;
    Ok((body, output, profile))
}
pub(super) fn credentials(headers: &HeaderMap, key_header: &HeaderName) -> LlmResult<HeaderMap> {
    use axum::http::header::AUTHORIZATION;
    if key_header.as_str() != "x-api-key" && headers.contains_key("x-api-key") {
        if headers.get_all("x-api-key").iter().count() != 1
            || headers.contains_key(key_header)
            || headers.contains_key(AUTHORIZATION)
        {
            return Err(LlmError::new(401, "llm_credentials_ambiguous"));
        }
        let mut normalized = headers.clone();
        let value = normalized
            .remove("x-api-key")
            .ok_or(LlmError::new(401, "llm_credentials_invalid"))?;
        normalized.insert(key_header.clone(), value);
        return credentials(&normalized, key_header);
    }
    if headers.get_all(AUTHORIZATION).iter().count() > 1
        || headers.get_all(key_header).iter().count() > 1
        || headers.contains_key(AUTHORIZATION) && headers.contains_key(key_header)
    {
        return Err(LlmError::new(401, "llm_credentials_ambiguous"));
    }
    let mut selected = headers.clone();
    if let Some(value) = headers.get(AUTHORIZATION) {
        let value = value
            .to_str()
            .map_err(|_| LlmError::new(401, "llm_credentials_invalid"))?;
        let mut words = value.split_whitespace();
        if !words
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            return Err(LlmError::new(401, "llm_credentials_invalid"));
        }
        let token = words
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(LlmError::new(401, "llm_credentials_invalid"))?;
        if words.next().is_some() {
            return Err(LlmError::new(401, "llm_credentials_invalid"));
        }
        // Compact JWTs remain JWTs, including malformed signatures. Never fall
        // back to an accompanying API key after a failed JWT verification.
        if token.split('.').count() != 3 {
            selected.remove(AUTHORIZATION);
            selected.insert(
                key_header.clone(),
                token
                    .parse()
                    .map_err(|_| LlmError::new(401, "llm_credentials_invalid"))?,
            );
        }
    }
    Ok(selected)
}
