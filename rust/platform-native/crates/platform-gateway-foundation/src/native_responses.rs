//! Source-owned success envelopes, serialized plugin DTOs and SQL JSON projections.
//! SQL-owned/dynamic row values are kept open; no response filtering or reparsing.
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn open_row() -> Value {
    json!({"type":"object","additionalProperties":true,"x-native-dynamic-row":true})
}
fn array(items: Value) -> Value {
    json!({"type":"array","items":items})
}
fn nullable_string() -> Value {
    json!({"type":["string","null"]})
}
fn object(properties: serde_json::Map<String, Value>) -> Value {
    let required: Vec<_> = properties.keys().cloned().collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
macro_rules! envelope {
    ($name:ident { $($field:ident: $ty:ty => $schema:expr),* $(,)? }) => {
        pub mod $name {
            use super::*;
            pub fn value($($field:$ty),*) -> Value { json!({$(stringify!($field):$field),*}) }
            pub fn schema() -> Value {
                let mut properties=serde_json::Map::new();
                $(properties.insert(stringify!($field).into(),$schema);)*
                object(properties)
            }
        }
    };
}
envelope!(tasks { tasks: Vec<Value> => array(open_row()) });
envelope!(task { task: Value => open_row() });
envelope!(execution { execution: Value => open_row() });
envelope!(executions { executions: Vec<Value> => array(open_row()) });
envelope!(task_action { task_id: String => json!({"type":"string"}), status: &'static str => json!({"type":"string","enum":["active","paused","cancelled"]}), cancellation_requested: bool => json!({"type":"boolean"}) });
envelope!(audit { items: Vec<Value> => array(json!({"x-native-dynamic-row":true})), has_more: bool => json!({"type":"boolean"}), limit: usize => json!({"type":"integer"}), offset: i64 => json!({"type":"integer"}), max_offset: i64 => json!({"type":"integer"}) });
envelope!(notification { notification: Value => notification_projection::schema() });
envelope!(acknowledgements { notifications: Vec<Value> => array(ack_projection::schema()) });
envelope!(plugin_update { plugin_id: String => json!({"type":"string"}), tenant_id: &str => json!({"type":"string"}), lifecycle: PluginLifecycle => PluginLifecycle::schema() });

macro_rules! response {
    ($name:ident { $($field:ident: $ty:ty => $schema:expr),* $(,)? }) => {
        #[derive(Debug, Serialize)]
        pub struct $name { $(pub $field:$ty),* }
        impl $name {
            pub fn schema() -> Value {
                let mut properties=serde_json::Map::new();
                $(properties.insert(stringify!($field).into(),$schema);)*
                object(properties)
            }
        }
    };
}
response!(PluginRegistry {
    schema_version: &'static str => json!({"type":"string"}),
    catalog_revision: u64 => json!({"type":"integer","minimum":0}),
    catalog_revision_token: String => json!({"type":"string"}),
    tenant_id: String => json!({"type":"string"}),
    can_manage: bool => json!({"type":"boolean"}),
    package_installation_supported: bool => json!({"type":"boolean"}),
    plugins: Vec<Plugin> => array(Plugin::schema()),
});
response!(Plugin { id: String => json!({"type":"string"}), versions: Vec<PluginVersion> => array(PluginVersion::schema()), lifecycle: PluginLifecycle => PluginLifecycle::schema() });
response!(PluginVersion {
    version: String => json!({"type":"string"}), availability: &'static str => json!({"type":"string"}),
    required_permissions: BTreeSet<String> => array(json!({"type":"string"})),
    declared_permissions: BTreeSet<String> => array(json!({"type":"string"})),
    capabilities: Vec<PluginCapability> => array(PluginCapability::schema()),
});
response!(PluginCapability {
    id: String => json!({"type":"string"}), name: String => json!({"type":"string"}),
    version: String => json!({"type":"string"}), schema_hash: String => json!({"type":"string"}),
    category: String => json!({"type":"string"}), risk_level: String => json!({"type":"string"}),
    read_only: bool => json!({"type":"boolean"}),
    required_permissions: Vec<String> => array(json!({"type":"string"})),
    declared_permissions: Vec<String> => array(json!({"type":"string"})),
    available: bool => json!({"type":"boolean"}),
});
response!(PluginLifecycle {
    managed: bool => json!({"type":"boolean"}), enabled: bool => json!({"type":"boolean"}),
    selected_version: Option<String> => nullable_string(), revision: String => json!({"type":"string"}),
    selected_catalog_revision: Option<String> => nullable_string(),
    selection_status: &'static str => json!({"type":"string"}),
    updated_at: Option<DateTime<Utc>> => json!({"type":["string","null"],"format":"date-time"}),
});
impl Default for PluginLifecycle {
    fn default() -> Self {
        Self {
            managed: false,
            enabled: true,
            selected_version: None,
            revision: "0".into(),
            selected_catalog_revision: None,
            selection_status: "deployment_default",
            updated_at: None,
        }
    }
}

// Each declaration owns the actual SQL JSON key, its expression and its schema.
// The query still returns Value directly, with identical filters/binds/null behavior.
macro_rules! sql_object {
    ($name:ident { $($field:literal => ($sql:expr, $schema:expr)),* $(,)? }) => {
        pub mod $name {
            use super::*;
            pub fn sql() -> String {
                let fields = vec![$(format!("'{}', {}",$field,$sql)),*];
                format!("jsonb_build_object({})", fields.join(", "))
            }
            pub fn schema() -> Value {
                let mut properties=serde_json::Map::new();
                $(properties.insert($field.into(),$schema);)*
                object(properties)
            }
        }
    };
}
sql_object!(notification_task_projection {
    "task_id" => ("t.task_id",json!({"type":"string"})),
    "title" => ("t.title",json!({"type":"string"})),
    "status" => ("t.status",json!({"type":"string"})),
});
sql_object!(notification_projection {
    "notification_id" => ("e.execution_id",json!({"type":"string"})),
    "occurred_at" => ("COALESCE(e.finished_at, e.updated_at)",json!({"type":"string","format":"date-time"})),
    "read_at" => ("e.notification_read_at",json!({"type":["string","null"],"format":"date-time"})),
    "task" => (notification_task_projection::sql(),notification_task_projection::schema()),
    "execution" => ("to_jsonb(e) - 'notification_read_at' - 'tenant_id' - 'user_id'",open_row()),
});
sql_object!(ack_projection {
    "notification_id" => ("execution_id",json!({"type":"string"})),
    "read_at" => ("notification_read_at",json!({"type":["string","null"],"format":"date-time"})),
});
sql_object!(summary_projection {
    "unread_count" => ("count(*) FILTER (WHERE e.notification_read_at IS NULL)",json!({"type":"integer"})),
    "total_count" => ("count(*)",json!({"type":"integer"})),
});
pub const TERMINAL: &str = "e.status IN ('succeeded','failed','cancelled')";
pub const JOIN: &str = "gateway.scheduled_task_executions e JOIN gateway.scheduled_tasks t ON t.task_id=e.task_id AND t.tenant_id=e.tenant_id AND t.user_id=e.user_id";
sql_object!(list_projection {
    "unread_count" => (format!("(SELECT count(*) FROM gateway.scheduled_task_executions e WHERE e.tenant_id=$1 AND e.user_id=$2 AND {TERMINAL} AND e.notification_read_at IS NULL)"),json!({"type":"integer"})),
    "notifications" => (format!(r#"COALESCE((SELECT jsonb_agg(p.document ORDER BY p.occurred_at DESC, p.execution_id DESC)
        FROM (SELECT e.execution_id, COALESCE(e.finished_at,e.updated_at) AS occurred_at,
            {} AS document FROM {JOIN}
            WHERE e.tenant_id=$1 AND e.user_id=$2 AND {TERMINAL}
                AND (NOT $3::boolean OR e.notification_read_at IS NULL)
                AND ($4::timestamptz IS NULL OR
                    (COALESCE(e.finished_at,e.updated_at),e.execution_id) < ($4::timestamptz,$5::uuid))
            ORDER BY COALESCE(e.finished_at,e.updated_at) DESC,e.execution_id DESC LIMIT $6
        ) p), '[]'::jsonb)"#,notification_projection::sql()),array(notification_projection::schema())),
});
pub fn list_sql() -> String {
    format!("SELECT {}", list_projection::sql())
}
pub fn summary_sql() -> String {
    format!(
        "SELECT {} FROM gateway.scheduled_task_executions e WHERE e.tenant_id=$1 AND e.user_id=$2 AND {TERMINAL}",
        summary_projection::sql()
    )
}
pub fn list_response_schema() -> Value {
    let mut schema = list_projection::schema();
    schema["properties"]["next_cursor"] = json!({"anyOf":[{"type":"null"},{"type":"object","properties":{"before_at":{"type":"string","format":"date-time"},"before_id":{"type":"string"}},"required":["before_at","before_id"],"additionalProperties":false}]});
    schema["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("next_cursor"));
    schema
}

/// Stable keyset cursor: timestamps alone cannot order equal-time receipts.
pub fn schedule_page(mut rows: Vec<Value>, limit: usize, field: &str, id: &str) -> Value {
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next = if has_more {
        rows.last()
            .map(|row| json!({"before_at":row["created_at"],"before_id":row[id]}))
    } else {
        None
    };
    json!({field:rows,"next_cursor":next})
}
pub fn schedule_page_schema(mut schema: Value) -> Value {
    schema["properties"]["next_cursor"] =
        list_response_schema()["properties"]["next_cursor"].clone();
    schema["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("next_cursor"));
    schema
}
pub fn catalog_review_schema(preview: bool, read: bool) -> Value {
    let mut properties = serde_json::Map::from_iter([
        ("review_id".into(), json!({"type":"string"})),
        ("provider_id".into(), json!({"type":"string"})),
        ("state".into(), json!({"type":"string"})),
    ]);
    if preview {
        for key in [
            "changes",
            "retained_models",
            "affected_references",
            "skipped_models",
        ] {
            properties.insert(key.into(), array(open_row()));
        }
        properties.insert("discovery_warnings".into(), array(json!({"type":"string"})));
        for key in ["reference_coverage", "notice"] {
            properties.insert(key.into(), json!({"type":"string"}));
        }
        properties.insert("expires_in_seconds".into(), json!({"type":"integer"}));
        properties.insert("provider_revision".into(), open_row());
        properties.insert(
            "automatic_removals".into(),
            json!({"type":"boolean","const":false}),
        );
    } else {
        properties.insert("result".into(),json!({"type":["object","null"],"additionalProperties":true,"x-native-dynamic-row":true}));
        if read {
            properties.insert("current".into(), open_row());
        }
    }
    object(properties)
}
pub fn success_schema(id: &str) -> Value {
    match id {
        "create_service_registration" => {
            json!({"type":"object","additionalProperties":false,"required":["service_id","status"],"properties":{"service_id":{"type":"string"},"status":{"const":"registered"}}})
        }
        "preview_provider_model_catalog" => catalog_review_schema(true, false),
        "apply_provider_model_catalog" => catalog_review_schema(false, false),
        "read_provider_model_catalog_review" => catalog_review_schema(false, true),
        "query_config_audit_log" => audit::schema(),
        "list_scheduled_tasks" => schedule_page_schema(tasks::schema()),
        "create_scheduled_task" | "get_scheduled_task" => task::schema(),
        "get_scheduled_task_execution" => execution::schema(),
        "list_scheduled_task_executions" => schedule_page_schema(executions::schema()),
        "pause_scheduled_task" | "resume_scheduled_task" | "cancel_scheduled_task" => {
            task_action::schema()
        }
        "list_assistant_plugins" => PluginRegistry::schema(),
        "update_assistant_plugin_lifecycle" => plugin_update::schema(),
        "list_scheduled_task_notifications" => list_response_schema(),
        "summarize_scheduled_task_notifications" => summary_projection::schema(),
        "get_scheduled_task_notification" => notification::schema(),
        "mark_scheduled_task_notification_read"
        | "mark_scheduled_task_notification_unread"
        | "mark_scheduled_task_notifications_read" => acknowledgements::schema(),
        _ => panic!("no source-owned response declaration for operation"),
    }
}
// These sets describe code-proven handler branches, not all possible host errors.
// Keep default explicit for extractors/middleware/custom identity/storage adapters.
pub fn responses(id: &str) -> Value {
    let codes: &[u16] = match id {
        "create_service_registration" => &[400, 401, 403, 409, 422, 501],
        "list_assistant_plugins" => &[401, 503],
        "update_assistant_plugin_lifecycle" => &[401, 403, 409, 422, 503],
        "query_config_audit_log" => &[401, 403, 422, 500, 503],
        "create_scheduled_task" => &[400, 401, 403, 409, 422, 429, 503],
        "list_scheduled_tasks" => &[400, 401, 403, 409, 422, 503],
        "summarize_scheduled_task_notifications" => &[400, 401, 403, 409, 503],
        "list_scheduled_task_notifications" => &[400, 401, 403, 409, 422, 503],
        _ => &[400, 401, 403, 404, 409, 422, 503],
    };
    let mut result = json!({"200":{"description":"Native success response","content":{"application/json":{"schema":success_schema(id)}}},
        "default":{"description":"Additional extractor, middleware or collaborator response; schema not declared","x-native-undeclared":true}});
    for code in codes {
        // Statuses are observed handler branches, not exhaustive host coverage.
        // Do not impose a detail envelope: host/extractor/collaborator JSON can differ.
        result[code.to_string()] = json!({"description":"Observed native handler status; error body remains unconstrained",
            "content":{"application/json":{"schema":{}}},"x-native-handler-status-observation":true});
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../../platform-openapi/tests/fixtures/legacy_plugin_types.rs");
    fn normalize_sql(value: &str) -> String {
        let mut quote = false;
        value
            .chars()
            .filter(|c| {
                if *c == '\'' {
                    quote = !quote;
                }
                quote || !c.is_whitespace()
            })
            .collect()
    }
    #[test]
    fn sql_projection_is_exactly_equivalent_to_preimage() {
        for (actual, expected) in [
            (
                list_sql(),
                include_str!("../../platform-openapi/tests/fixtures/notification-list.sql"),
            ),
            (
                summary_sql(),
                include_str!("../../platform-openapi/tests/fixtures/notification-summary.sql"),
            ),
            (
                notification_projection::sql(),
                include_str!("../../platform-openapi/tests/fixtures/notification-projection.sql"),
            ),
            (
                ack_projection::sql(),
                include_str!("../../platform-openapi/tests/fixtures/notification-ack.sql"),
            ),
        ] {
            assert_eq!(normalize_sql(&actual), normalize_sql(expected));
        }
    }
    #[test]
    fn dynamic_envelopes_preserve_values_nulls_and_wire_key_order() {
        let row = json!({"unknown":null,"extra":[1,true],"large":9007199254740993_u64});
        let rows = vec![row.clone()];
        for (actual, expected) in [
            (tasks::value(rows.clone()), json!({"tasks":rows})),
            (task::value(row.clone()), json!({"task":row})),
            (executions::value(rows.clone()), json!({"executions":rows})),
            (
                task_action::value("id".into(), "cancelled", true),
                json!({"task_id":"id","status":"cancelled","cancellation_requested":true}),
            ),
            (
                audit::value(rows.clone(), true, 25, 0, 10000),
                json!({"items":rows,"has_more":true,"limit":25,"offset":0,"max_offset":10000}),
            ),
            (
                acknowledgements::value(rows.clone()),
                json!({"notifications":rows}),
            ),
            (
                notification::value(row.clone()),
                json!({"notification":row}),
            ),
        ] {
            assert_eq!(
                serde_json::to_vec(&actual).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
        }
    }
    macro_rules! registry_fixture {
        ($m:ident, $lifecycle:expr) => {
            $m::PluginRegistry {
                schema_version: "v1",
                catalog_revision: 9007199254740993,
                catalog_revision_token: "9007199254740993".into(),
                tenant_id: "tenant".into(),
                can_manage: false,
                package_installation_supported: false,
                plugins: vec![$m::Plugin {
                    id: "p".into(),
                    lifecycle: $lifecycle,
                    versions: vec![$m::PluginVersion {
                        version: "v".into(),
                        availability: "available",
                        required_permissions: BTreeSet::from(["z".into(), "a".into()]),
                        declared_permissions: BTreeSet::new(),
                        capabilities: vec![$m::PluginCapability {
                            id: "c".into(),
                            name: "n".into(),
                            version: "1".into(),
                            schema_hash: "h".into(),
                            category: "x".into(),
                            risk_level: "read".into(),
                            read_only: true,
                            required_permissions: vec![],
                            declared_permissions: vec!["a".into()],
                            available: true,
                        }],
                    }],
                }],
            }
        };
    }
    #[test]
    fn moved_plugin_dtos_serialize_byte_identically_with_explicit_nulls() {
        use super as current;
        let actual = registry_fixture!(current, PluginLifecycle::default());
        let expected = registry_fixture!(legacy, legacy::PluginLifecycle::default());
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let value = serde_json::to_value(actual).unwrap();
        for key in [
            "selected_version",
            "selected_catalog_revision",
            "updated_at",
        ] {
            assert!(
                value["plugins"][0]["lifecycle"]
                    .as_object()
                    .unwrap()
                    .contains_key(key)
            );
            assert!(value["plugins"][0]["lifecycle"][key].is_null());
        }
    }
    #[test]
    fn moved_plugin_dtos_preserve_populated_optional_values_and_timestamps() {
        use super as current;
        let time: DateTime<Utc> = "2030-01-02T03:04:05.123456Z".parse().unwrap();
        let actual = registry_fixture!(
            current,
            PluginLifecycle {
                managed: true,
                enabled: false,
                selected_version: Some("1".into()),
                revision: "9007199254740993".into(),
                selected_catalog_revision: Some("7".into()),
                selection_status: "current",
                updated_at: Some(time)
            }
        );
        let expected = registry_fixture!(
            legacy,
            legacy::PluginLifecycle {
                managed: true,
                enabled: false,
                selected_version: Some("1".into()),
                revision: "9007199254740993".into(),
                selected_catalog_revision: Some("7".into()),
                selection_status: "current",
                updated_at: Some(time)
            }
        );
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
    }
    #[test]
    fn plugin_update_wrapper_preserves_existing_serialization() {
        let expected =
            json!({"plugin_id":"p","tenant_id":"t","lifecycle":legacy::PluginLifecycle::default()});
        let actual = plugin_update::value("p".into(), "t", PluginLifecycle::default());
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
    }
    #[test]
    fn success_schemas_keep_dynamic_rows_open_and_nullable_fields_present() {
        assert_eq!(
            task::schema()["properties"]["task"]["additionalProperties"],
            true
        );
        assert_eq!(
            notification_projection::schema()["properties"]["execution"]["additionalProperties"],
            true
        );
        assert_eq!(
            notification_projection::schema()["properties"]["read_at"]["type"],
            json!(["string", "null"])
        );
        let lifecycle = PluginLifecycle::schema();
        assert_eq!(lifecycle["required"].as_array().unwrap().len(), 7);
        assert_eq!(
            lifecycle["properties"]["selected_version"]["type"],
            json!(["string", "null"])
        );
        let summary = summary_projection::schema();
        assert!(
            summary["properties"]["total_count"]
                .get("maximum")
                .is_none()
        );
    }
    #[test]
    fn every_owned_operation_has_success_but_never_complete_error_claim() {
        for op in crate::native_contract::OPERATIONS {
            let response = responses(op.id);
            assert_eq!(
                response["200"]["content"]["application/json"]["schema"]["type"],
                "object"
            );
            assert_eq!(response["default"]["x-native-undeclared"], true);
            for (status, branch) in response.as_object().unwrap() {
                if status != "200" && status != "default" {
                    assert_eq!(branch["content"]["application/json"]["schema"], json!({}));
                }
            }
        }
    }
}
