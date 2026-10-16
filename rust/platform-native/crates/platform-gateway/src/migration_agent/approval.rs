//! Owner-only bounded projection of the exact action awaiting runtime approval.
use super::{Scope, spec, text};
use serde_json::{Value, json};
use sqlx::PgPool;
fn secret(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.iter().any(|(k, v)| {
            ["token", "secret", "password", "credential", "api_key"]
                .iter()
                .any(|s| k.to_lowercase().contains(s))
                || secret(v)
        }),
        Value::Array(a) => a.iter().any(secret),
        _ => false,
    }
}
pub(crate) fn action_details(name: &str, args: &Value) -> Option<(String, Vec<Value>)> {
    let o = args.as_object()?;
    match name {
        "execute_python_code" => {
            let code = args["code"].as_str()?;
            let empty = Vec::new();
            let inputs = args
                .get("inputs")
                .map(Value::as_array)
                .unwrap_or(Some(&empty))?;
            if code.is_empty() || code.chars().count() > 12000 || inputs.len() > 20 {
                return None;
            }
            let mut files = Vec::new();
            for i in inputs {
                files.push(format!(
                    "{} ({} bytes)",
                    i["filename"].as_str()?,
                    i["size_bytes"].as_i64()?
                ));
            }
            Some((
                "Isolated Python execution".into(),
                vec![
                    json!({"name":"code","value":code}),
                    json!({"name":"input files","value":if files.is_empty(){"none".into()}else{files.join(", ")}}),
                ],
            ))
        }
        "confluence_write" => {
            if !matches!(
                text(args, "action"),
                "create_page"
                    | "update_page"
                    | "find_replace"
                    | "move_page"
                    | "comment"
                    | "delete_page"
            ) {
                return None;
            }
            let target = args["page_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| args["space_key"].as_str())?;
            if target.trim().is_empty() {
                return None;
            }
            let allowed = [
                "action",
                "page_id",
                "space_key",
                "title",
                "content",
                "parent_id",
                "target_parent_id",
                "find",
                "replace",
                "raw_html",
                "body",
            ];
            if o.keys().any(|k| !allowed.contains(&k.as_str())) {
                return None;
            }
            let mut fields = Vec::new();
            for k in [
                "action",
                "page_id",
                "space_key",
                "title",
                "parent_id",
                "target_parent_id",
                "find",
                "replace",
                "raw_html",
                "content",
                "body",
            ] {
                if let Some(v) = o.get(k) {
                    let value = match v {
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
                        _ => return None,
                    };
                    if value.chars().count() > 12000 {
                        return None;
                    }
                    fields.push(json!({"name":k,"value":value}));
                }
            }
            Some((target.into(), fields))
        }
        "context_compact"
        | "generate_image"
        | "generate_quiz"
        | "mcp_docgen__generate_document"
        | "todo_write"
        | "update_user_memory" => {
            if secret(args) || spec::canonical(args).chars().count() > 50000 {
                return None;
            }
            let target = ["title", "topic", "key", "goal", "prompt"]
                .iter()
                .find_map(|k| args[*k].as_str().filter(|s| !s.trim().is_empty()))
                .unwrap_or("This assistant conversation");
            Some((target.into(),o.iter().map(|(k,v)|json!({"name":k,"value":v.as_str().map(str::to_owned).unwrap_or_else(||serde_json::to_string(v).expect("JSON"))})).collect()))
        }
        _ => None,
    }
}

fn local_node_snapshot_binding(snapshot: &Value) -> Option<(String, String)> {
    if snapshot["schema_version"] != "agent-runtime-snapshot/v1"
        || snapshot["agent_spec"]["channel"] != "builtin"
    {
        return None;
    }
    let capabilities = snapshot["agent_spec"]["capabilities"].as_array()?;
    let mut matches = capabilities.iter().filter(|capability| {
        capability["type"] == "connector"
            && capability["id"] == "local_node_action"
            && capability["name"] == "local_node_action"
    });
    let capability = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let config = &capability["config"];
    if config["provider"] != "local_node"
        || config["tool_name"] != "local_node_action"
        || config["principal_type"] != "user_delegated"
    {
        return None;
    }
    let device = config["connection_id"].as_str()?;
    let device_suffix = device.strip_prefix("dev_")?;
    if device_suffix.len() != 24
        || !device_suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let grant = config["grant_id"].as_str()?;
    if uuid::Uuid::parse_str(grant).ok()?.to_string() != grant {
        return None;
    }
    let digest = config["permission_snapshot_digest"].as_str()?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || config["device_capability_revision"]
            .as_u64()
            .unwrap_or_default()
            == 0
    {
        return None;
    }
    Some((device.to_owned(), grant.to_owned()))
}

fn safe_local_path(path: &str, allow_root: bool) -> bool {
    if path.is_empty()
        || path.chars().count() > 1024
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path.contains('\0')
        || path.chars().any(char::is_control)
        || path.as_bytes().get(1) == Some(&b':')
    {
        return false;
    }
    if path == "." {
        return allow_root;
    }
    path.split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn optional_bounded_integer(args: &Value, key: &str, max: u64) -> bool {
    match args.get(key) {
        None => true,
        Some(Value::Number(value)) => value
            .as_u64()
            .is_some_and(|value| (1..=max).contains(&value)),
        _ => false,
    }
}

fn local_node_read_action_details(
    args: &Value,
    binding: &(String, String),
) -> Option<(String, Vec<Value>)> {
    let object = args.as_object()?;
    if object
        .keys()
        .any(|key| !["operation", "arguments", "device_id"].contains(&key.as_str()))
        || secret(args)
    {
        return None;
    }
    let device = &binding.0;
    if args
        .get("device_id")
        .is_some_and(|value| value.as_str() != Some(device.as_str()))
    {
        return None;
    }
    let operation = args["operation"].as_str()?;
    let arguments = args["arguments"].as_object()?;
    let grant = arguments.get("grant_id")?.as_str()?;
    let path = arguments.get("path")?.as_str()?;
    if grant != binding.1 || spec::canonical(args).chars().count() > 12000 {
        return None;
    }

    let (allowed, path_allows_root) = match operation {
        "file.list" => (&["grant_id", "path", "limit"][..], true),
        "file.read" => (&["grant_id", "path", "max_bytes"][..], false),
        "file.search" => (&["grant_id", "path", "query", "limit"][..], true),
        _ => return None,
    };
    if arguments.keys().any(|key| !allowed.contains(&key.as_str()))
        || !safe_local_path(path, path_allows_root)
    {
        return None;
    }
    match operation {
        "file.list" if !optional_bounded_integer(&args["arguments"], "limit", 500) => {
            return None;
        }
        "file.read"
            if path == "."
                || !optional_bounded_integer(&args["arguments"], "max_bytes", 8 * 1024 * 1024) =>
        {
            return None;
        }
        "file.search" => {
            let query = arguments.get("query")?.as_str()?;
            if query.is_empty()
                || query.chars().count() > 500
                || query.contains('\0')
                || !optional_bounded_integer(&args["arguments"], "limit", 200)
            {
                return None;
            }
        }
        _ => {}
    }

    let mut parameters = vec![
        json!({"name":"operation","value":operation}),
        json!({"name":"device_id","value":device}),
        json!({"name":"grant_id","value":grant}),
        json!({"name":"path","value":path}),
    ];
    for key in ["query", "limit", "max_bytes"] {
        if let Some(value) = arguments.get(key) {
            parameters.push(json!({"name":key,"value":value}));
        }
    }
    Some((format!("{device}:{path}"), parameters))
}

pub async fn owner_approval_preview(
    pool: &PgPool,
    scope: &Scope,
    thread: &str,
    id: &str,
    summary: &Value,
) -> Value {
    let unavailable = json!({"can_approve":false,"reason":"Action details could not be verified. Reject this request or check the affected system.","authorization_scope":"this action only"});
    let row=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('run_id',a.run_id,'tool_name',a.tool_name,'arguments',a.arguments,'status',a.status) FROM assistant_tool_approvals a JOIN assistant_runs r ON r.run_id=a.run_id WHERE a.approval_id=$1::uuid AND a.tenant_id=$2 AND a.user_id=$3 AND a.session_id=$4 AND r.tenant_id=$2 AND r.user_id=$3 AND r.session_id=$4 AND r.harness_thread_id=$5::uuid").bind(id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(thread).fetch_optional(pool).await;
    let Ok(Some(row)) = row else {
        return unavailable;
    };
    let args = if let Some(s) = row["arguments"].as_str() {
        serde_json::from_str(s).unwrap_or(Value::Null)
    } else {
        row["arguments"].clone()
    };
    if !args.is_object() {
        return unavailable;
    }
    let digest = spec::hash(&args);
    let remaining = summary["expires_in_seconds"].as_f64().unwrap_or(0.0);
    if row["run_id"] != summary["run_id"]
        || row["tool_name"] != summary["tool_name"]
        || row["status"] != "pending"
        || summary["status"] != "pending"
        || summary["arguments_hash"] != digest
        || remaining <= 0.0
    {
        return unavailable;
    }
    let (target, parameters, effect) = if text(&row, "tool_name") == "local_node_action" {
        let Some(run_id) = row["run_id"].as_str() else {
            return unavailable;
        };
        let snapshot = sqlx::query_scalar::<_, Value>(
            "SELECT s.snapshot FROM assistant_runtime_snapshots s \
             JOIN assistant_runs r ON r.run_id=s.run_id AND r.tenant_id=s.tenant_id \
               AND r.user_id=s.user_id AND r.session_id=s.session_id \
             WHERE s.run_id=$1::uuid AND s.runtime_thread_id=$2::uuid \
               AND s.tenant_id=$3 AND s.user_id=$4 AND s.session_id=$5 \
               AND r.engine='agent_runtime'",
        )
        .bind(run_id)
        .bind(thread)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .fetch_optional(pool)
        .await;
        let Ok(Some(snapshot)) = snapshot else {
            return unavailable;
        };
        let Some(binding) = local_node_snapshot_binding(&snapshot) else {
            return unavailable;
        };
        let Some((target, parameters)) = local_node_read_action_details(&args, &binding) else {
            return unavailable;
        };
        (target, parameters, "read")
    } else {
        let Some((target, parameters)) = action_details(text(&row, "tool_name"), &args) else {
            return unavailable;
        };
        (target, parameters, "write")
    };
    json!({"can_approve":true,"tool_name":row["tool_name"],"effect":effect,"target":target,"parameters":parameters,"authorization_scope":"this action only","expires_in_seconds":remaining as i64,"arguments_hash":digest})
}

// This selects a read-only operation, never authorization. Runtime revalidates
// the durable required/result receipts and exact owner, thread, call and reason.
pub(super) fn requires_receipt_readback(summary: &Value) -> bool {
    matches!(
        summary["status"].as_str(),
        Some("approved" | "rejected" | "consumed" | "cancelled" | "expired")
    )
}

#[cfg(test)]
mod receipt_readback_tests {
    use super::*;

    #[test]
    fn only_known_nonpending_states_select_readback() {
        for status in ["approved", "rejected", "consumed", "cancelled", "expired"] {
            assert!(requires_receipt_readback(&json!({"status":status})));
        }
        for summary in [
            json!({}),
            json!({"status":null}),
            json!({"status":"pending"}),
            json!({"status":"unknown"}),
            json!({"status":true}),
        ] {
            assert!(!requires_receipt_readback(&summary));
        }
    }
}

#[cfg(test)]
mod local_node_read_approval_tests {
    use super::*;

    const DEVICE: &str = "dev_aaaaaaaaaaaaaaaaaaaaaaaa";
    const GRANT: &str = "cfbfa9d4-18b3-4470-b3f2-f13660a993dc";

    fn snapshot() -> Value {
        json!({
            "schema_version":"agent-runtime-snapshot/v1",
            "agent_spec":{
                "channel":"builtin",
                "capabilities":[{
                    "type":"connector",
                    "id":"local_node_action",
                    "name":"local_node_action",
                    "config":{
                        "provider":"local_node",
                        "tool_name":"local_node_action",
                        "principal_type":"user_delegated",
                        "connection_id":DEVICE,
                        "grant_id":GRANT,
                        "permission_snapshot_digest":"a".repeat(64),
                        "device_capability_revision":2
                    }
                }]
            }
        })
    }

    fn binding() -> (String, String) {
        local_node_snapshot_binding(&snapshot()).expect("exact built-in connector binding")
    }

    #[test]
    fn only_bound_list_read_and_search_operations_get_read_previews() {
        let binding = binding();
        for (operation, arguments) in [
            ("file.list", json!({"grant_id":GRANT,"path":".","limit":20})),
            (
                "file.read",
                json!({"grant_id":GRANT,"path":"marker.txt","max_bytes":256}),
            ),
            (
                "file.search",
                json!({"grant_id":GRANT,"path":".","query":"synthetic marker","limit":10}),
            ),
        ] {
            let args = json!({"operation":operation,"arguments":arguments,"device_id":DEVICE});
            let (target, parameters) = local_node_read_action_details(&args, &binding)
                .expect("allowlisted read operation is reviewable");
            assert!(target.starts_with(DEVICE));
            assert!(
                parameters
                    .iter()
                    .any(|value| value["name"] == "grant_id" && value["value"] == GRANT)
            );
            assert!(
                parameters
                    .iter()
                    .any(|value| value["name"] == "operation" && value["value"] == operation)
            );
        }
    }

    #[test]
    fn local_node_preview_rejects_writes_aliases_mismatched_binding_and_unsafe_paths() {
        let binding = binding();
        for operation in ["file.write", "file.watch", "list_directory", "unknown"] {
            assert!(
                local_node_read_action_details(
                    &json!({"operation":operation,"arguments":{"grant_id":GRANT,"path":"."}}),
                    &binding,
                )
                .is_none()
            );
        }
        assert!(local_node_read_action_details(
            &json!({"operation":"file.list","arguments":{"grant_id":"d7bcf05f-45c8-4bd8-960e-81cd6972ec3f","path":"."}}),
            &binding,
        )
        .is_none());
        assert!(local_node_read_action_details(
            &json!({"operation":"file.list","device_id":"dev_bbbbbbbbbbbbbbbbbbbbbbbb","arguments":{"grant_id":GRANT,"path":"."}}),
            &binding,
        )
        .is_none());
        assert!(local_node_read_action_details(
            &json!({"operation":"file.read","arguments":{"grant_id":GRANT,"path":"../outside"}}),
            &binding,
        )
        .is_none());
        assert!(local_node_read_action_details(
            &json!({"operation":"file.list","arguments":{"grant_id":GRANT,"path":".","content_base64":"c2VjcmV0"}}),
            &binding,
        )
        .is_none());
        assert!(local_node_read_action_details(
            &json!({"operation":"file.search","arguments":{"grant_id":GRANT,"path":".","query":"x","limit":201}}),
            &binding,
        )
        .is_none());
    }

    #[test]
    fn local_node_snapshot_binding_rejects_untrusted_or_ambiguous_connectors() {
        let mut invalid = snapshot();
        invalid["agent_spec"]["channel"] = json!("preview");
        assert!(local_node_snapshot_binding(&invalid).is_none());

        let mut invalid = snapshot();
        invalid["agent_spec"]["capabilities"][0]["config"]["permission_snapshot_digest"] =
            json!("A".repeat(64));
        assert!(local_node_snapshot_binding(&invalid).is_none());

        let mut invalid = snapshot();
        let duplicate = invalid["agent_spec"]["capabilities"][0].clone();
        invalid["agent_spec"]["capabilities"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(local_node_snapshot_binding(&invalid).is_none());
    }
}
