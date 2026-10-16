//! Pure Python gateway-to-runtime contract projections (no kernel implementation).
use super::{AgentError, Scope, spec, text};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn invalid() -> AgentError {
    AgentError::code(400, "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID")
}
pub fn runtime_model_config(base: &str, model: &str, search: bool) -> Value {
    json!({"model_provider":"ai-platform-gateway","model":model,"web_search":if search{"live"}else{"disabled"},"features":{"standalone_web_search":false,"multi_agent_v2":{"enabled":true,"max_concurrent_threads_per_session":6}},"model_providers":{"ai-platform-gateway":{"name":"AI Platform Gateway Model Plane","base_url":base,"env_key":"AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN","wire_api":"responses","requires_openai_auth":false,"supports_websockets":false,"request_max_retries":0,"stream_max_retries":0}}})
}
#[path = "runtime_tools.rs"]
mod runtime_tools;
pub use runtime_tools::{dynamic_tools, tool_policy};
pub fn fingerprint(readonly: &Value) -> String {
    let tools: Vec<_> = ["tools", "mcp", "deferred"]
        .iter()
        .flat_map(|k| readonly[*k].as_array().into_iter().flatten().cloned())
        .collect();
    // Old flat catalogs must retain their existing persisted fingerprint.
    // Explicit namespaces were previously dropped at thread creation, so those
    // sessions need a new immutable catalog rather than silently reusing it.
    if tools.iter().any(|tool| {
        tool["metadata"].get("namespace").is_some_and(|namespace| {
            !namespace.is_null() && namespace != "" && namespace != "functions"
        })
    }) {
        spec::hash(&json!({"dynamic_tool_wire":"codex-namespaces/v1","tools":tools}))
    } else {
        spec::hash(&json!(tools))
    }
}
pub fn readonly_payload(raw: &Value, scope: &Scope, revision: i64) -> Result<Value, AgentError> {
    let empty = json!({});
    let raw = if raw.is_null() { &empty } else { raw };
    let object = raw.as_object().ok_or_else(invalid)?;
    let allowed = [
        "knowledge",
        "attachments",
        "web_search",
        "tools",
        "mcp",
        "deferred",
        "memory_context",
        "capability_allowlist",
        "platform_config",
        "attachment_tools",
        "responses_tool_names",
        "responses_tool_choice",
        "responses_parallel_tool_calls",
    ];
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid());
    }
    let mut items = Vec::new();
    for (key, list, kind, source, payload_key, prefix) in [
        (
            "knowledge",
            "dataset_ids",
            "knowledge",
            "knowledge",
            "dataset_id",
            "knowledge",
        ),
        (
            "attachments",
            "refs",
            "attachment",
            "attachments",
            "content_ref",
            "attachment",
        ),
    ] {
        if let Some(v) = raw.get(key).filter(|v| !v.is_null()) {
            if !v.is_object() {
                return Err(invalid());
            }
            if let Some(a) = v.get(list).filter(|v| !v.is_null()) {
                for id in a.as_array().ok_or_else(invalid)? {
                    let id = id.as_str().ok_or_else(invalid)?;
                    let mut payload = json!({});
                    payload[payload_key] = id.into();
                    items.push(json!({"item_id":format!("{prefix}:{id}"),"kind":kind,"source":source,"payload":payload,"tenant_id":scope.tenant_id,"capability_revision":revision}));
                }
            }
        }
    }
    if let Some(web) = raw.get("web_search").filter(|v| !v.is_null()) {
        if !web.is_object() {
            return Err(invalid());
        }
        if web["enabled"] == true {
            let max = web["max_results"].as_i64().unwrap_or(5);
            items.push(json!({"item_id":"context:web-search","kind":"context","source":"web-search","payload":{"max_results":max},"tenant_id":scope.tenant_id,"capability_revision":revision}));
        }
    }
    if let Some(memory) = raw.get("memory_context").filter(|v| !v.is_null()) {
        if !memory.is_object() {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_CONTEXT_INVALID",
            ));
        }
        items.push(json!({"item_id":"context:long-term-memory","kind":"context","source":"long-term-memory","payload":memory,"tenant_id":scope.tenant_id,"capability_revision":revision}));
    }
    let mut out = json!({"schema_version":"agent-readonly-capability/v1","tenant_id":scope.tenant_id,"capability_revision":revision,"items":items});
    for key in ["tools", "mcp", "deferred", "attachment_tools"] {
        let v = raw
            .get(key)
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!([]));
        for d in v.as_array().ok_or_else(invalid)? {
            if !d.is_object()
                || d["tenant_id"] != scope.tenant_id
                || d["capability_revision"].as_i64() != Some(revision)
                || (key == "deferred" && d["read_only"] == true)
                || (key != "deferred" && d["read_only"] != true)
                || (key == "attachment_tools" && d["name"] != "read_attachment")
            {
                return Err(invalid());
            }
        }
        out[key] = v;
    }
    let names = &raw["responses_tool_names"];
    if !names.is_null() {
        let a = names.as_array().ok_or_else(invalid)?;
        let names: BTreeSet<_> = a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if a.len() > 128 || names.len() != a.len() {
            return Err(invalid());
        }
    }
    out["responses_tool_names"] = names.clone();
    let choice = raw
        .get("responses_tool_choice")
        .cloned()
        .unwrap_or(json!("auto"));
    if !matches!(choice.as_str(), Some("auto" | "none" | "required"))
        && !(choice["type"] == "function" && choice["name"].is_string())
    {
        return Err(invalid());
    }
    out["responses_tool_choice"] = choice;
    let parallel = raw
        .get("responses_parallel_tool_calls")
        .cloned()
        .unwrap_or(json!(true));
    if !parallel.is_boolean() {
        return Err(invalid());
    }
    out["responses_parallel_tool_calls"] = parallel;
    if let Some(a) = raw.get("capability_allowlist").filter(|v| !v.is_null()) {
        if a.as_array()
            .is_none_or(|a| a.iter().any(|v| !v.is_object()))
        {
            return Err(invalid());
        }
        out["capability_allowlist"] = a.clone();
    }
    if let Some(config) = raw.get("platform_config").filter(|v| !v.is_null()) {
        if !config.is_object() {
            return Err(AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_PLATFORM_CONFIG_INVALID",
            ));
        }
        out["platform_config"] = config.clone();
    }
    for item in &items {
        if item["kind"] == "attachment" {
            let r = text(&item["payload"], "content_ref");
            if r.len() != 20
                || !r.starts_with("art_")
                || !r[4..]
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(AgentError::code(
                    422,
                    "AI_PLATFORM_AGENT_RUNTIME_ATTACHMENT_INVALID",
                ));
            }
        }
    }
    out["attachment_tools"] = json!([]);
    Ok(out)
}
pub fn turn_prompt_readonly(v: &Value) -> Value {
    let mut out = json!({"schema_version":v["schema_version"],"tenant_id":v["tenant_id"],"capability_revision":v["capability_revision"],"items":v["items"],"tools":[],"mcp":[],"deferred":[],"attachment_tools":[]});
    if let Some(c) = v.get("platform_config") {
        out["platform_config"] = c.clone();
    }
    out
}
pub fn reasoning(profile: &Value, requested: Option<&str>) -> Result<Value, AgentError> {
    let r = &profile["reasoning"];
    let requested = requested.unwrap_or("auto").trim().to_lowercase();
    let requested = if requested.is_empty() {
        "auto".to_owned()
    } else {
        requested
    };
    let opts = r["options"].as_array().ok_or_else(|| {
        AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_PROFILE_INVALID")
    })?;
    let default = opts
        .iter()
        .find(|o| o["id"] == r["default_option"])
        .ok_or_else(|| {
            AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_PROFILE_INVALID")
        })?;
    let selected = if requested == "auto" {
        Some(default)
    } else {
        opts.iter().find(|o| {
            o["id"] == requested
                || o["aliases"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == &requested))
        })
    };
    let fallback = selected.is_none();
    let o = selected.unwrap_or(default);
    Ok(
        json!({"requested_option":requested,"effective_option":o["id"],"adapter_id":r["adapter_id"],"canonical_effort":o["canonical_effort"],"settings":o["settings"],"fallback_reason":if fallback{json!("unsupported_reasoning_option")}else{Value::Null}}),
    )
}
fn platform_error() -> AgentError {
    AgentError::code(409, "AI_PLATFORM_AGENT_RUNTIME_PLATFORM_CONFIG_INVALID")
}
fn safe_hash(v: &Value, required: bool) -> Result<Value, AgentError> {
    if v.is_null() && !required {
        return Ok(Value::Null);
    }
    let s = v.as_str().ok_or_else(platform_error)?;
    let s = s.strip_prefix("sha256:").unwrap_or(s);
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(platform_error());
    }
    Ok(json!(format!("sha256:{s}")))
}
fn nonempty(v: &Value, max: usize) -> Result<String, AgentError> {
    let s = v
        .as_str()
        .filter(|s| {
            !s.trim().is_empty()
                && s.chars().count() <= max
                && s.chars().all(|c| !c.is_control() || "\r\n\t".contains(c))
        })
        .ok_or_else(platform_error)?;
    Ok(s.trim().into())
}
fn first<'a>(values: &[&'a Value]) -> Value {
    values
        .iter()
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .map(|v| (*v).clone())
        .unwrap_or(Value::Null)
}
fn metadata(v: &Value) -> Result<Value, AgentError> {
    if v.is_null() {
        return Ok(json!({}));
    }
    let o = v
        .as_object()
        .filter(|o| o.len() <= 64)
        .ok_or_else(platform_error)?;
    if o.iter().any(|(k, v)| {
        k.is_empty()
            || k.len() > 128
            || !k.as_bytes()[0].is_ascii_alphabetic()
            || !k
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
            || [
                "token",
                "secret",
                "password",
                "apikey",
                "api_key",
                "api-key",
                "credential",
                "private_key",
                "private-key",
                "authorization",
            ]
            .iter()
            .any(|x| k.to_lowercase().contains(x))
            || v.is_object()
            || v.is_array()
    }) {
        return Err(platform_error());
    }
    Ok(v.clone())
}
pub fn platform_config(
    snapshot: &Value,
    scope: &Scope,
    readonly: &Value,
) -> Result<Value, AgentError> {
    let agent_spec = &snapshot["agent_spec"];
    let instructions = agent_spec["developerInstructions"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(platform_error)?;
    let caps = snapshot["capabilities"]
        .as_array()
        .ok_or_else(platform_error)?;
    let (mut skills, mut grants, mut plugins) = (Vec::new(), Vec::new(), Vec::new());
    let mut seen = BTreeSet::new();
    for b in caps {
        let config = super::object(&b["config"]);
        if !spec::unsafe_paths(&config, "config").is_empty() {
            return Err(platform_error());
        }
        let kind = first(&[&b["type"], &b["capability_type"]]);
        let id = nonempty(&first(&[&b["id"], &b["resource_id"]]), 255)?;
        if !seen.insert((id.clone(), kind.clone().to_string())) {
            return Err(platform_error());
        }
        if kind == "skill" {
            let version = nonempty(
                &first(&[&b["version"], &b["resource_version"], &config["version"]]),
                255,
            )?;
            let hash = safe_hash(
                &first(&[
                    &b["content_hash"],
                    &b["content_sha256"],
                    &config["content_hash"],
                    &config["content_sha256"],
                    &b["schema_hash"],
                ]),
                true,
            )?;
            let raw = config
                .get("permissions")
                .or_else(|| config.get("required_permissions"))
                .cloned()
                .unwrap_or(json!([]));
            let a = raw
                .as_array()
                .filter(|a| a.len() <= 32)
                .ok_or_else(platform_error)?;
            let permissions: BTreeSet<String> = a
                .iter()
                .map(|v| nonempty(v, 128))
                .collect::<Result<_, _>>()?;
            let mut manifest = json!({});
            for k in ["name", "title", "description", "entrypoint"] {
                if let Some(v) = config["manifest"].get(k) {
                    manifest[k] = v.clone();
                }
            }
            skills.push(json!({"id":id,"version":version,"content_hash":hash,"schema_hash":safe_hash(&b["schema_hash"],false)?,"permissions":permissions,"manifest":metadata(&manifest)?}));
        }
        let mut grant = json!({"capability_id":id,"capability_type":kind});
        for (k, limit) in [
            ("grant_id", 255),
            ("connection_id", 255),
            ("principal_type", 64),
            ("provider", 128),
        ] {
            let value = first(&[&b[k], &config[k]]);
            if !value.is_null() {
                grant[k] = json!(nonempty(&value, limit)?);
            }
        }
        if ["grant_id", "connection_id", "principal_type"]
            .iter()
            .any(|k| grant.get(*k).is_some())
        {
            grants.push(grant);
        }
        let plugin = &config["plugin"];
        let pid = first(&[&b["plugin_id"], &config["plugin_id"], &plugin["id"]]);
        if !pid.is_null() || plugin.as_object().is_some_and(|o| !o.is_empty()) {
            plugins.push(json!({"id":nonempty(&pid,255)?,"version":nonempty(&first(&[&b["plugin_version"],&config["plugin_version"],&plugin["version"]]),255)?,"capability_id":id,"metadata":metadata(&first(&[&config["plugin_metadata"],&plugin["metadata"]]))?}));
        }
    }
    skills.sort_by_key(|v| (text(v, "id").to_owned(), text(v, "version").to_owned()));
    grants.sort_by_key(|v| {
        (
            text(v, "capability_type").to_owned(),
            text(v, "capability_id").to_owned(),
        )
    });
    plugins.sort_by_key(|v| (text(v, "id").to_owned(), text(v, "version").to_owned()));
    let refs: BTreeSet<_> = readonly["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["kind"] == "attachment")
        .map(|v| text(&v["payload"], "content_ref").to_owned())
        .collect();
    let mut out = json!({"schema_version":"agent-runtime-platform-config/v1","tenant_id":scope.tenant_id,"user_id":scope.user_id,"session_id":scope.session_id,"agent_spec":agent_spec,"instructions":{"prompt_hash":format!("sha256:{}",spec::hash(&json!(instructions))),"dynamic_data_after_instructions":true},"tenant_grants":grants,"skills":skills,"plugins":plugins,"attachments":refs.iter().map(|r|json!({"ref":r,"descriptor":"read_attachment","version":"v1"})).collect::<Vec<_>>()});
    out["config_hash"] = json!(format!("sha256:{}", spec::hash(&out)));
    Ok(out)
}
pub fn snapshot_allowlist(snapshot: &Value) -> Result<Option<Vec<Value>>, AgentError> {
    let invalid = || AgentError::code(409, "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID");
    let caps = snapshot["capabilities"].as_array().ok_or_else(invalid)?;
    if caps.is_empty() && snapshot["agent_spec"]["channel"] == "builtin" {
        return Ok(None);
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in caps {
        let kind = text(raw, "type");
        let id = text(raw, "id");
        let config = &raw["config"];
        let name = config["tool_name"]
            .as_str()
            .or_else(|| config["name"].as_str())
            .unwrap_or(id);
        let version = text(raw, "version");
        let hash = text(raw, "schema_hash");
        if kind.is_empty()
            || id.is_empty()
            || name.is_empty()
            || (hash.is_empty() && !matches!(kind, "platform" | "connector"))
        {
            return Err(invalid());
        }
        if !seen.insert((kind, id, version, hash)) {
            continue;
        }
        let mut entry = json!({"type":kind,"id":id,"name":name,"version":if version.is_empty(){Value::Null}else{json!(version)},"schema_hash":if hash.is_empty(){Value::Null}else{json!(hash)}});
        let channel = text(&snapshot["agent_spec"], "channel");
        if kind == "mcp" {
            let o = config.as_object().ok_or_else(invalid)?;
            if o.keys()
                .any(|k| !["connection_id", "principal_type", "risk"].contains(&k.as_str()))
            {
                return Err(invalid());
            }
            let connection = super::uuid(text(config, "connection_id")).map_err(|_| invalid())?;
            let principal = text(config, "principal_type");
            let risk = config["risk"].as_str().unwrap_or(text(raw, "risk"));
            if !matches!(principal, "service_account" | "user_delegated")
                || !matches!(risk, "low" | "medium" | "high" | "critical")
                || !matches!(
                    channel,
                    "preview" | "hosted_private" | "hosted_public" | "embed" | "api"
                )
                || hash.len() != 71
                || !hash.starts_with("sha256:")
            {
                return Err(invalid());
            }
            entry["connector_binding"] = json!({"binding_type":"grant","provider":"mcp","tool_name":name,"principal_type":principal,"grant_id":null,"connection_id":connection,"schema_hash":hash,"risk_level":risk,"channel":channel});
        } else if kind == "connector" {
            let o = config.as_object().ok_or_else(invalid)?;
            let provider = text(config, "provider");
            let local_node = provider == "local_node";
            if o.keys().any(|k| {
                !["provider", "tool_name", "principal_type", "grant_id"].contains(&k.as_str())
                    && !(local_node
                        && [
                            "connection_id",
                            "permission_snapshot_digest",
                            "device_capability_revision",
                        ]
                        .contains(&k.as_str()))
            }) {
                return Err(invalid());
            }
            let grant = &config["grant_id"];
            let principal = &config["principal_type"];
            if provider.is_empty()
                || provider.len() > 128
                || config["tool_name"].as_str().is_some_and(|s| s != name)
                || !matches!(
                    channel,
                    "preview"
                        | "hosted"
                        | "hosted_private"
                        | "hosted_public"
                        | "embed"
                        | "api"
                        | "builtin"
                )
                || grant.is_null() != principal.is_null()
            {
                return Err(invalid());
            }
            if !grant.is_null() {
                super::uuid(grant.as_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
                if !matches!(
                    principal.as_str(),
                    Some("service_account" | "user_delegated")
                ) {
                    return Err(invalid());
                }
            }
            if local_node {
                let connection = text(config, "connection_id");
                let digest = text(config, "permission_snapshot_digest");
                let device_revision = config["device_capability_revision"]
                    .as_u64()
                    .filter(|revision| *revision > 0);
                if id != "local_node_action"
                    || name != "local_node_action"
                    || channel != "builtin"
                    || principal.as_str() != Some("user_delegated")
                    || grant.is_null()
                    || connection.strip_prefix("dev_").is_none_or(|suffix| {
                        suffix.len() != 24
                            || !suffix
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                    || digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || device_revision.is_none()
                {
                    return Err(invalid());
                }
                entry["connector_binding"] = json!({
                    "binding_type":"grant",
                    "provider":provider,
                    "tool_name":name,
                    "principal_type":principal,
                    "grant_id":grant,
                    "connection_id":connection,
                    "permission_snapshot_digest":digest,
                    "device_capability_revision":device_revision,
                    "channel":channel
                });
            } else {
                entry["connector_binding"] = json!({"binding_type":if grant.is_null(){"catalog"}else{"grant"},"provider":provider,"tool_name":name,"principal_type":principal,"grant_id":grant,"channel":channel});
            }
        }
        result.push(entry);
    }
    let kb = &snapshot["knowledge"];
    if kb["datasets"].as_array().is_some_and(|a| !a.is_empty())
        && matches!(
            kb["retrieval"]["mode"].as_str().unwrap_or("auto"),
            "auto" | "tool"
        )
        && !result.iter().any(|v| v["id"] == "search_knowledge_base")
    {
        result.push(json!({"type":"platform","id":"search_knowledge_base","name":"search_knowledge_base","version":null,"schema_hash":null}));
    }
    Ok(Some(result))
}

#[cfg(test)]
mod local_node_binding_tests {
    use super::*;

    fn snapshot() -> Value {
        json!({
            "agent_spec":{"channel":"builtin"},
            "capabilities":[{
                "type":"connector",
                "id":"local_node_action",
                "name":"local_node_action",
                "version":null,
                "schema_hash":null,
                "config":{
                    "provider":"local_node",
                    "tool_name":"local_node_action",
                    "principal_type":"user_delegated",
                    "grant_id":"cfbfa9d4-18b3-4470-b3f2-f13660a993dc",
                    "connection_id":"dev_aaaaaaaaaaaaaaaaaaaaaaaa",
                    "permission_snapshot_digest":"a".repeat(64),
                    "device_capability_revision":2
                }
            }]
        })
    }

    #[test]
    fn local_node_allowlist_preserves_the_server_resolved_device_snapshot() {
        let allowlist = snapshot_allowlist(&snapshot()).unwrap().unwrap();
        assert_eq!(allowlist.len(), 1);
        assert_eq!(allowlist[0]["type"], "connector");
        assert_eq!(
            allowlist[0]["connector_binding"]["permission_snapshot_digest"],
            "a".repeat(64)
        );
        assert_eq!(
            allowlist[0]["connector_binding"]["device_capability_revision"],
            2
        );
    }

    #[test]
    fn local_node_allowlist_rejects_incomplete_or_non_builtin_bindings() {
        let mut stale = snapshot();
        stale["capabilities"][0]["config"]
            .as_object_mut()
            .unwrap()
            .remove("permission_snapshot_digest");
        assert!(snapshot_allowlist(&stale).is_err());

        let mut non_builtin = snapshot();
        non_builtin["agent_spec"]["channel"] = json!("preview");
        assert!(snapshot_allowlist(&non_builtin).is_err());
    }
}
