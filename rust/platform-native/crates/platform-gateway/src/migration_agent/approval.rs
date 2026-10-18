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
        "local_node_action" => {
            let device = args["device_id"].as_str()?;
            let op = args["operation"].as_str()?;
            let params = &args["arguments"];
            if device.is_empty() || op.is_empty() || !params.is_object() || secret(params) {
                return None;
            }
            let serialized = spec::canonical(params);
            if serialized.chars().count() > 12000 {
                return None;
            }
            Some((
                device.into(),
                vec![
                    json!({"name":"operation","value":op}),
                    json!({"name":"device","value":device}),
                    json!({"name":"arguments","value":serialized}),
                ],
            ))
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
pub async fn owner_approval_preview(
    pool: &PgPool,
    scope: &Scope,
    thread: &str,
    id: &str,
    summary: &Value,
) -> Value {
    let unavailable = json!({"can_approve":false,"reason":"Action details could not be verified. Reject this request or check the affected system.","authorization_scope":"this action only"});
    let row=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('run_id',a.run_id,'tool_name',a.tool_name,'arguments',a.arguments,'status',a.status) FROM assistant_tool_approvals a JOIN assistant_runs r ON r.run_id=a.run_id WHERE a.approval_id=$1::uuid AND a.tenant_id=$2 AND a.user_id=$3 AND a.session_id=$4 AND r.tenant_id=$2 AND r.user_id=$3 AND r.session_id=$4 AND r.harness_thread_id=$5::uuid").bind(id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(thread).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(pool, &scope.tenant_id)).await;
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
    let Some((target, parameters)) = action_details(text(&row, "tool_name"), &args) else {
        return unavailable;
    };
    json!({"can_approve":true,"tool_name":row["tool_name"],"effect":"write","target":target,"parameters":parameters,"authorization_scope":"this action only","expires_in_seconds":remaining as i64,"arguments_hash":digest})
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
