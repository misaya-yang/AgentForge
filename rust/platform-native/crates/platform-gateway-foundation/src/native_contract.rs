//! Bounded native-owned contract. Never reads or embeds the published SDK schema.
//! Request declarations below produce both runtime serde DTOs and OpenAPI fields.
//! Success responses have source-owned bounded schemas; this is NOT a complete Gateway contract.
#[path = "native_api_keys_contract.rs"]
pub mod api_keys;
#[path = "../../platform-auth/src/contract.rs"]
pub mod auth;
#[path = "native_connectors_contract.rs"]
pub mod connectors;
#[path = "native_invocation_task_contract.rs"]
pub mod invocation_tasks;
#[path = "native_responses.rs"]
pub mod responses;
#[path = "native_roles_contract.rs"]
pub mod roles;
#[path = "native_services_contract.rs"]
pub mod services;
#[path = "native_sessions_contract.rs"]
pub mod sessions;
#[path = "native_users_contract.rs"]
pub mod users;
pub const API_KEY_HEADER_DEFAULT: &str = "X-API-Key";
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SERVICE_REGISTRATIONS: &str = "/api/v1/service-registrations";
pub const TASK_BASE: &str = "/api/v1/scheduled-tasks";
pub const NOTIFICATION_BASE: &str = "/api/v1/scheduled-task-notifications";
pub const PLUGINS: &str = "/api/v1/assistant/plugins";
pub const PLUGIN_LIFECYCLE: &str = "/api/v1/assistant/plugins/{plugin_id}/lifecycle";
pub const CONFIG_AUDIT: &str = "/api/v1/admin/tenant-policies/config-audit-log";
pub const CATALOG_PREVIEW: &str = "/api/v1/providers/{provider_id}/models/sync/preview";
pub const CATALOG_APPLY: &str = "/api/v1/providers/{provider_id}/models/sync/apply";
pub const CATALOG_READ: &str = "/api/v1/providers/{provider_id}/models/sync/reviews/{review_id}";
pub const MAX_PAGE: usize = 100;
pub const TITLE_MAX: usize = 120;
pub const MESSAGE_MAX: usize = 200000;
pub const MODEL_MAX_BYTES: usize = 255;
pub const INTERVAL_MIN: i64 = 60;
pub const INTERVAL_MAX: i64 = 31536000;
pub const RETRIES_MAX: i32 = 3;
pub const AUDIT_LIMIT_MAX: i64 = 100;
pub const AUDIT_OFFSET_MAX: i64 = 10000;
pub const AUDIT_FILTER_LIMITS: [(&str, usize); 5] = [
    ("tenant_id", 255),
    ("user_id", 255),
    ("resource_type", 100),
    ("resource_id", 255),
    ("action", 50),
];

// Deliberately small: a declaration owns its Rust deserialization and schema.
// Validation beyond serde stays in the handlers and is described explicitly.
macro_rules! request {
    ($name:ident { $( $(#[$attr:meta])* $field:ident: $ty:ty => ($required:expr, $schema:expr)),* $(,)? }) => {
        #[derive(Clone, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $( $(#[$attr])* pub $field: $ty ),* }
        impl $name {
            pub fn schema() -> Value {
                let mut properties = serde_json::Map::new();
                let mut required: Vec<&str> = Vec::new();
                $(properties.insert(stringify!($field).into(), $schema);
                  if $required { required.push(stringify!($field)); })*
                json!({"type":"object", "additionalProperties":false,
                    "properties":properties, "required":required})
            }
        }
    };
}
request!(CreateTask {
    title: String => (true, json!({"type":"string","maxLength":TITLE_MAX,"x-native-rule":"trimmed value must be nonempty"})),
    message: String => (true, json!({"type":"string","maxLength":MESSAGE_MAX,"x-native-rule":"trimmed value must be nonempty"})),
    model_id: String => (true, json!({"type":"string","x-native-max-utf8-bytes":MODEL_MAX_BYTES,"x-native-rule":"trimmed value must be nonempty"})),
    run_at: DateTime<Utc> => (true, json!({"type":"string","format":"date-time","x-native-rule":"strictly later than validation time"})),
    interval_seconds: Option<i64> => (false, json!({"type":["integer","null"],"minimum":INTERVAL_MIN,"maximum":INTERVAL_MAX})),
    #[serde(default)] max_retries: i32 => (false, json!({"type":"integer","minimum":0,"maximum":RETRIES_MAX,"default":0})),
});
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    #[default]
    All,
    Unread,
}
request!(ListQuery {
    #[serde(default)] filter: Filter => (false, json!({"type":"string","enum":["all","unread"],"default":"all"})),
    limit: Option<usize> => (false, json!({"type":"integer","minimum":1,"maximum":MAX_PAGE,"default":50})),
    before_at: Option<DateTime<Utc>> => (false, json!({"type":"string","format":"date-time"})),
    before_id: Option<String> => (false, json!({"type":"string","x-native-rule":"accepted by uuid::Uuid::parse_str; paired with before_at"})),
});
request!(SchedulePageQuery {
    limit: Option<usize> => (false, json!({"type":"integer","minimum":1,"maximum":200})),
    before_at: Option<DateTime<Utc>> => (false, json!({"type":"string","format":"date-time"})),
    before_id: Option<String> => (false, json!({"type":"string","x-native-rule":"UUID; paired with before_at"})),
});
impl SchedulePageQuery {
    pub fn validate(self, maximum: usize) -> Result<Self, &'static str> {
        if !(1..=maximum).contains(&self.limit.unwrap_or(maximum))
            || self.before_at.is_some() != self.before_id.is_some() {
            return Err("invalid page size or incomplete cursor");
        }
        Ok(self)
    }
}
request!(ReadBatch {
    notification_ids: Vec<String> => (true, json!({"type":"array","minItems":1,"maxItems":MAX_PAGE,
        "items":{"type":"string"},"x-native-rule":"UUID parseable; uniqueness is checked after UUID normalization"})),
});
request!(UpdatePlugin {
    selected_version: String => (true, name_schema()),
    enabled: bool => (true, json!({"type":"boolean"})),
    expected_revision: String => (true, json!({"type":"string","pattern":"^(0|[1-9][0-9]*)$","x-native-rule":"canonical i64 decimal; 0 <= value < i64::MAX"})),
    expected_catalog_revision: String => (true, json!({"type":"string","pattern":"^[1-9][0-9]*$","x-native-rule":"canonical positive u64 decimal; equals deployed catalog revision"})),
    expected_tenant_id: String => (true, json!({"type":"string","x-native-rule":"equals authenticated tenant"})),
});
pub fn name_schema() -> Value {
    json!({"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9][A-Za-z0-9._:-]*$"})
}
pub fn is_name(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

pub struct Operation {
    pub method: &'static str,
    pub path: &'static str,
    pub id: &'static str,
    pub jwt_only: bool,
    pub request: Option<&'static str>,
}
pub const OPERATIONS: &[Operation] = &[
    Operation { method: "post", path: SERVICE_REGISTRATIONS, id: "create_service_registration", jwt_only: false, request: Some("CreateServiceRegistration") },
    Operation {
        method: "get",
        path: CONFIG_AUDIT,
        id: "query_config_audit_log",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "get",
        path: TASK_BASE,
        id: "list_scheduled_tasks",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "get",
        path: "/api/v1/scheduled-tasks/{task_id}",
        id: "get_scheduled_task",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "get",
        path: "/api/v1/scheduled-tasks/{task_id}/executions/{execution_id}",
        id: "get_scheduled_task_execution",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: TASK_BASE,
        id: "create_scheduled_task",
        jwt_only: false,
        request: Some("CreateTask"),
    },
    Operation {
        method: "get",
        path: "/api/v1/scheduled-tasks/{task_id}/executions",
        id: "list_scheduled_task_executions",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: "/api/v1/scheduled-tasks/{task_id}/pause",
        id: "pause_scheduled_task",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: "/api/v1/scheduled-tasks/{task_id}/resume",
        id: "resume_scheduled_task",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: "/api/v1/scheduled-tasks/{task_id}/cancel",
        id: "cancel_scheduled_task",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "get",
        path: PLUGINS,
        id: "list_assistant_plugins",
        jwt_only: true,
        request: None,
    },
    Operation {
        method: "put",
        path: PLUGIN_LIFECYCLE,
        id: "update_assistant_plugin_lifecycle",
        jwt_only: true,
        request: Some("UpdatePlugin"),
    },
    Operation {
        method: "get",
        path: NOTIFICATION_BASE,
        id: "list_scheduled_task_notifications",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "get",
        path: "/api/v1/scheduled-task-notifications/summary",
        id: "summarize_scheduled_task_notifications",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "get",
        path: "/api/v1/scheduled-task-notifications/{notification_id}",
        id: "get_scheduled_task_notification",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: "/api/v1/scheduled-task-notifications/{notification_id}/read",
        id: "mark_scheduled_task_notification_read",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: "/api/v1/scheduled-task-notifications/{notification_id}/unread",
        id: "mark_scheduled_task_notification_unread",
        jwt_only: false,
        request: None,
    },
    Operation {
        method: "post",
        path: "/api/v1/scheduled-task-notifications/read",
        id: "mark_scheduled_task_notifications_read",
        jwt_only: false,
        request: Some("ReadBatch"),
    },
    Operation { method:"post", path:CATALOG_PREVIEW, id:"preview_provider_model_catalog", jwt_only:false, request:None },
    Operation { method:"post", path:CATALOG_APPLY, id:"apply_provider_model_catalog", jwt_only:false, request:Some("CatalogApply") },
    Operation { method:"get", path:CATALOG_READ, id:"read_provider_model_catalog_review", jwt_only:false, request:None },
];
fn parameter(name: &str, location: &str, required: bool, schema: Value) -> Value {
    json!({"name":name,"in":location,"required":required,"schema":schema})
}
request!(CatalogApply {
    review_id: String => (true, json!({"type":"string"})),
    model_ids: Vec<String> => (true, json!({"type":"array","items":{"type":"string"},"minItems":1,"maxItems":500,"uniqueItems":true})),
    replace_existing_metadata: bool => (true, json!({"type":"boolean"})),
});
pub fn document() -> Value {
    let mut paths = serde_json::Map::new();
    for op in OPERATIONS {
        let mut parameters = Vec::new();
        for name in ["task_id", "execution_id", "notification_id", "plugin_id", "provider_id", "review_id"] {
            if op.path.contains(&format!("{{{name}}}")) {
                let schema = if matches!(name,"provider_id"|"review_id") { json!({"type":"string"}) } else if name == "plugin_id" {
                    name_schema()
                } else {
                    json!({"type":"string","x-native-rule":"accepted by uuid::Uuid::parse_str"})
                };
                parameters.push(parameter(name, "path", true, schema));
            }
        }
        if op.path == CONFIG_AUDIT {
            for (name, max) in AUDIT_FILTER_LIMITS {
                parameters.push(parameter(
                    name,
                    "query",
                    false,
                    json!({"type":"string","maxLength":max}),
                ));
            }
            parameters.push(parameter(
                "limit",
                "query",
                false,
                json!({"type":"integer","minimum":1,"maximum":AUDIT_LIMIT_MAX,"default":25}),
            ));
            parameters.push(parameter(
                "offset",
                "query",
                false,
                json!({"type":"integer","minimum":0,"maximum":AUDIT_OFFSET_MAX,"default":0}),
            ));
        }
        if op.path == NOTIFICATION_BASE {
            for (name, schema) in ListQuery::schema()["properties"].as_object().unwrap() {
                parameters.push(parameter(name, "query", false, schema.clone()));
            }
        }
        if matches!(op.id, "list_scheduled_tasks" | "list_scheduled_task_executions") {
            for (name, schema) in SchedulePageQuery::schema()["properties"].as_object().unwrap() {
                let mut schema = schema.clone();
                if name == "limit" {
                    let maximum = if op.id == "list_scheduled_tasks" { 200 } else { 100 };
                    schema["maximum"] = json!(maximum);
                    schema["default"] = json!(maximum);
                }
                parameters.push(parameter(name, "query", false, schema));
            }
        }
        let security = if op.jwt_only {
            json!([{"NativeBearerAuth":[]}])
        } else {
            json!([{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}])
        };
        let mut operation = json!({"operationId":op.id,"parameters":parameters,"security":security,
            "x-native-response-schema":"bounded-source-owned",
            "responses":responses::responses(op.id)});
        if let Some(request) = op.request {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        if op.path == NOTIFICATION_BASE {
            operation["x-native-query-rule"] =
                json!("unknown fields rejected; before_at and before_id must occur together");
        }
        paths.entry(op.path).or_insert_with(|| json!({}))[op.method] = operation;
    }
    let mut document = json!({"openapi":"3.1.0","info":{"title":"Native Gateway bounded source contract","version":"0.1.0"},
        "x-native-contract":{"scope":"native-only","complete":false,"responses_declared":false,"success_responses_declared":true,
        "security_note":"NativeBoundApiKey header is deployment-configurable; x-api-key is the default. Scope, tenant, forgery and admin admission remain authoritative runtime checks."},
        "paths":paths,"components":{"schemas":{"CreateServiceRegistration":service_registration_schema(),"CatalogApply":CatalogApply::schema(),"CreateTask":CreateTask::schema(),"ReadBatch":ReadBatch::schema(),"UpdatePlugin":UpdatePlugin::schema()},
        "securitySchemes":{"NativeBearerAuth":{"type":"http","scheme":"bearer","bearerFormat":"JWT","description":"JWT bearer credential. Per-operation declarations specify session/account/tenant checks; logout only verifies the token before revocation. Verification failure never falls back to an accompanying API key."},"NativeBoundApiKey":{"type":"apiKey","in":"header","name":API_KEY_HEADER_DEFAULT,"description":"Database-bound API key authority. The admission header is deployment-configurable; static shared keys are not accepted by these operations."}}}});
    auth::extend_document(&mut document);
    connectors::extend_document(&mut document);
    api_keys::extend_document(&mut document);
    invocation_tasks::extend_document(&mut document);
    roles::extend_document(&mut document);
    services::extend_document(&mut document);
    users::extend_document(&mut document);
    sessions::extend_document(&mut document);
    document
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            filter: Filter::default(),
            limit: None,
            before_at: None,
            before_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_defaults_and_rejections_match_native_serde() {
        let task =
            json!({"title":"t","message":"m","model_id":"x","run_at":"2030-01-01T00:00:00Z"});
        assert_eq!(
            serde_json::from_value::<CreateTask>(task.clone())
                .unwrap()
                .max_retries,
            0
        );
        for name in ["title", "message", "model_id", "run_at"] {
            let mut missing = task.clone();
            missing.as_object_mut().unwrap().remove(name);
            assert!(serde_json::from_value::<CreateTask>(missing).is_err());
        }
        let mut task_null = task.clone();
        task_null["interval_seconds"] = Value::Null;
        assert!(
            serde_json::from_value::<CreateTask>(task_null)
                .unwrap()
                .interval_seconds
                .is_none()
        );
        let mut task_null = task.clone();
        task_null["max_retries"] = Value::Null;
        assert!(serde_json::from_value::<CreateTask>(task_null).is_err());
        let mut task_extra = task;
        task_extra["unknown"] = json!(true);
        assert!(serde_json::from_value::<CreateTask>(task_extra).is_err());
        assert!(matches!(
            serde_json::from_value::<ListQuery>(json!({}))
                .unwrap()
                .filter,
            Filter::All
        ));
        assert!(serde_json::from_value::<ListQuery>(json!({"filter":null})).is_err());
        assert!(serde_json::from_value::<ListQuery>(json!({"filter":"unknown"})).is_err());
        assert!(serde_json::from_value::<ReadBatch>(json!({})).is_err());
        assert!(
            serde_json::from_value::<ReadBatch>(json!({"notification_ids":[],"extra":1})).is_err()
        );
        assert!(serde_json::from_value::<UpdatePlugin>(json!({})).is_err());
    }
    #[test]
    fn declared_fields_and_required_match_actual_deserialization() {
        let schema = CreateTask::schema();
        assert_eq!(
            schema["required"],
            json!(["title", "message", "model_id", "run_at"])
        );
        assert_eq!(schema["properties"]["max_retries"]["default"], 0);
        assert_eq!(
            schema["properties"]["model_id"]["x-native-max-utf8-bytes"],
            255
        );
        assert!(schema["properties"]["model_id"].get("maxLength").is_none());
        assert_eq!(
            ListQuery::schema()["properties"]["limit"]["maximum"],
            MAX_PAGE
        );
        assert_eq!(
            ReadBatch::schema()["properties"]["notification_ids"]["maxItems"],
            MAX_PAGE
        );
    }
    #[test]
    fn bounded_document_never_claims_full_response_coverage() {
        let doc = document();
        let declared_paths = OPERATIONS.iter().map(|op| op.path.to_owned())
            .chain(api_keys::SELF_SERVICE_API_KEY_ROUTES.iter().map(|(_, path, _)| format!("/api/v1{path}")))
            .chain(connectors::PERSONAL_ROUTES.iter().map(|(_, path, _)| format!("/api/v1{path}")))
            .chain(invocation_tasks::ROUTES.iter().map(|(_, path, _)| (*path).to_owned()))
            .chain(auth::ROUTES.iter().map(|(_, path, _)| (*path).to_owned()))
            .chain(roles::ROLES_ROUTES.iter().map(|(_, path, _)| format!("/api/v1{path}")))
            .chain(services::SERVICES_ROUTES.iter().map(|(_, path, _)| format!("/api/v1{path}")))
            .chain(users::USERS_ROUTES.iter().map(|(_, path, _)| format!("/api/v1{path}")))
            .chain(sessions::ROUTES.iter().map(|(_, path, _)| (*path).to_owned()))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(doc["paths"].as_object().unwrap().len(), declared_paths.len());
        let declared_operations: usize = doc["paths"].as_object().unwrap().values()
            .map(|item| item.as_object().unwrap().len()).sum();
        assert_eq!(declared_operations, OPERATIONS.len() + api_keys::SELF_SERVICE_API_KEY_ROUTES.len() + connectors::PERSONAL_ROUTES.len() + invocation_tasks::ROUTES.len() + auth::ROUTES.len() + roles::ROLES_ROUTES.len() + services::SERVICES_ROUTES.len() + users::USERS_ROUTES.len() + sessions::ROUTES.len());
        assert_eq!(doc["paths"][SERVICE_REGISTRATIONS]["post"]["operationId"], "create_service_registration");
        assert_eq!(doc["x-native-contract"]["complete"], false);
        for op in OPERATIONS {
            assert_eq!(doc["paths"][op.path][op.method]["operationId"], op.id);
            assert_eq!(
                doc["paths"][op.path][op.method]["x-native-response-schema"],
                "bounded-source-owned"
            );
        }
        assert_eq!(document(), doc);
    }
}

#[cfg(test)]
mod schedule_history_tests {
    use super::*;
    #[test]
    fn schedule_page_defaults_bounds_and_paired_cursor() {
        for maximum in [100,200] {
            assert!(serde_json::from_value::<SchedulePageQuery>(json!({})).unwrap().validate(maximum).is_ok());
            for n in [0, maximum+1] {
                assert!(serde_json::from_value::<SchedulePageQuery>(json!({"limit":n})).unwrap().validate(maximum).is_err());
            }
            assert!(serde_json::from_value::<SchedulePageQuery>(json!({"limit":maximum})).unwrap().validate(maximum).is_ok());
        }
        for v in [json!({"before_at":"2026-10-13T00:00:00Z"}),json!({"before_id":"a"})] {
            assert!(serde_json::from_value::<SchedulePageQuery>(v).unwrap().validate(100).is_err());
        }
        for v in [json!({"limit":-1}),json!({"limit":1.5}),json!({"before_at":"bad"}),json!({"unknown":true})] {
            assert!(serde_json::from_value::<SchedulePageQuery>(v).is_err());
        }
    }
    #[test]
    fn schedule_page_preserves_rows_and_uses_last_visible_cursor() {
        let rows = vec![json!({"created_at":"same","task_id":"c","message":"old"}),json!({"created_at":"same","task_id":"b"}),json!({"created_at":"same","task_id":"a"})];
        let page = responses::schedule_page(rows.clone(),2,"tasks","task_id");
        assert_eq!(page["tasks"],json!(rows[..2]));
        assert_eq!(page["next_cursor"],json!({"before_at":"same","before_id":"b"}));
        assert_eq!(responses::schedule_page(rows,3,"tasks","task_id")["next_cursor"],Value::Null);
        assert_eq!(responses::schedule_page(vec![],20,"executions","execution_id"),json!({"executions":[],"next_cursor":null}));
    }
    #[test]
    fn exact_receipts_and_page_parameters_are_source_owned() {
        let doc=document();
        for (path,id) in [("/api/v1/scheduled-tasks/{task_id}","get_scheduled_task"),("/api/v1/scheduled-tasks/{task_id}/executions/{execution_id}","get_scheduled_task_execution")] {
            assert_eq!(doc["paths"][path]["get"]["operationId"],id);
            assert!(doc["paths"][path]["get"]["responses"]["200"].is_object());
        }
        for (path,maximum) in [(TASK_BASE,200),("/api/v1/scheduled-tasks/{task_id}/executions",100)] {
            let params=doc["paths"][path]["get"]["parameters"].as_array().unwrap();
            assert_eq!(params.iter().find(|p|p["name"]=="limit").unwrap()["schema"]["maximum"],maximum);
        }
    }
}

/// This declaration is used by runtime validation and the source-owned exporter.
pub fn service_registration_schema() -> Value {
    json!({
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "service_id": {
      "type": "string",
      "pattern": "^[a-z0-9][a-z0-9_-]{0,127}$"
    },
    "name": {
      "type": "string",
      "minLength": 1
    },
    "service_type": {
      "const": "custom"
    },
    "connector_type": {
      "const": "http"
    },
    "status": {
      "const": "active"
    },
    "supported_modes": {
      "enum": [
        [
          "sync",
          "stream"
        ],
        [
          "stream"
        ]
      ]
    },
    "accepted_content_types": {
      "const": [
        "text"
      ]
    },
    "output_content_types": {
      "const": [
        "text"
      ]
    },
    "session_enabled": {
      "const": true
    },
    "max_retries": {
      "const": 0
    },
    "connector_config": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "sdk_provider_api_key": {
          "type": "string",
          "writeOnly": true,
          "minLength": 16,
          "maxLength": 4096,
          "x-native-rule": "16..4096 UTF-8 bytes; no bytes <= 32; must not begin enc:"
        }
      },
      "required": [
        "sdk_provider_api_key"
      ]
    },
    "metadata": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "adapter_type": {
          "enum": [
            "openai_agents",
            "claude_agent_sdk"
          ]
        },
        "sdk_agent": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "model": {
              "type": "string",
              "minLength": 1,
              "x-native-rule": "trimmed nonempty; at most 160 UTF-8 bytes"
            },
            "instructions": {
              "type": "string",
              "x-native-rule": "at most 32768 UTF-8 bytes; whitespace normalized to default"
            },
            "max_turns": {
              "type": "integer",
              "minimum": 1,
              "maximum": 32
            }
          },
          "required": [
            "model",
            "instructions",
            "max_turns"
          ]
        },
        "sdk_tools": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "knowledge_dataset_ids": {
              "type": "array",
              "items": {
                "type": "string"
              },
              "maxItems": 16
            },
            "capability_ids": {
              "type": "array",
              "items": {
                "type": "string"
              }
            }
          },
          "required": []
        }
      },
      "required": [
        "adapter_type",
        "sdk_agent"
      ]
    }
  },
  "required": [
    "service_id",
    "name",
    "service_type",
    "connector_type",
    "status",
    "supported_modes",
    "accepted_content_types",
    "output_content_types",
    "session_enabled",
    "max_retries",
    "connector_config",
    "metadata"
  ],
  "description": "Native-only atomic create-only official SDK service registration. Existing IDs return 409 without replacement. No fallback to legacy POST /services. Existing SDK credential, Knowledge and capability admission remains authoritative."
})
}
