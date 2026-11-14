//! Automations reuse the Gateway scheduler, current actor authority and Runtime.
//! Every effect has a committed dispatch marker. Unknown outcomes are never replayed.
pub mod cron;
mod engine;
pub mod webhooks;
use super::{Actor, AgentError, AgentFuture, AgentState, routes, text};
use ai_platform_gateway_foundation::{
    provider_cipher::NativeProviderCipher, provider_service::ProviderCipher,
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::Utc;
pub use engine::tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};

/// Opaque runtime-only key material. It is deliberately neither Debug nor Serialize.
#[derive(Clone, Default)]
pub struct AutomationCipherKey(Option<std::sync::Arc<str>>);
impl AutomationCipherKey {
    pub(crate) fn from_configuration(value: &str) -> Self {
        Self((!value.trim().is_empty()).then(|| std::sync::Arc::from(value)))
    }
    pub(crate) fn key(&self) -> Result<&str, AgentError> {
        self.0
            .as_deref()
            .ok_or_else(|| AgentError::code(503, "AUTOMATION_CIPHER_UNAVAILABLE"))
    }
}

pub trait AutomationKnowledge: Send + Sync {
    fn read<'a>(&'a self, actor: &'a Actor, item: &'a str) -> AgentFuture<'a, Value>;
    fn append<'a>(
        &'a self,
        actor: &'a Actor,
        item: &'a str,
        markdown: &'a str,
    ) -> AgentFuture<'a, Value>;
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Cron {
        expression: String,
        timezone: String,
    },
    Webhook {},
    SpacePageChanged {
        item_id: String,
    },
    ConnectorEvent {
        entry_id: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    RunAgent { prompt: String },
    RunSkill { skill_id: String, prompt: String },
    WriteSpacePage { item_id: String, markdown: String },
    Notify { message: String },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub title: String,
    pub model_id: String,
    pub trigger: Trigger,
    pub actions: Vec<Action>,
}
fn invalid() -> AgentError {
    AgentError::code(422, "AUTOMATION_INVALID")
}
fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 255
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn bounded(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= maximum
}
impl Definition {
    pub fn validate(&self) -> Result<(), AgentError> {
        if !bounded(&self.title, 120)
            || !bounded(&self.model_id, 255)
            || self.actions.is_empty()
            || self.actions.len() > 8
        {
            return Err(invalid());
        }
        match &self.trigger {
            Trigger::Cron {
                expression,
                timezone,
            } => {
                cron::Cron::parse(expression)?;
                if !bounded(timezone, 80) {
                    return Err(invalid());
                }
            }
            Trigger::SpacePageChanged { item_id } if !safe_id(item_id) => return Err(invalid()),
            Trigger::ConnectorEvent { entry_id } if !safe_id(entry_id) => return Err(invalid()),
            _ => {}
        }
        for action in &self.actions {
            let valid = match action {
                Action::RunAgent { prompt } => bounded(prompt, 20000),
                Action::RunSkill { skill_id, prompt } => {
                    safe_id(skill_id) && bounded(prompt, 20000)
                }
                Action::WriteSpacePage { item_id, markdown } => {
                    safe_id(item_id) && bounded(markdown, 200000)
                }
                Action::Notify { message } => bounded(message, 4000),
            };
            if !valid {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
fn definition(row: &Value) -> Result<Definition, AgentError> {
    let d: Definition = serde_json::from_value(row["definition"].clone()).map_err(|_| invalid())?;
    d.validate()?;
    Ok(d)
}
fn public(row: Value) -> Value {
    let mut row = row;
    if let Some(object) = row.as_object_mut() {
        object.remove("webhook_secret_ciphertext");
    }
    row
}
fn key(state: &AgentState) -> Result<&str, AgentError> {
    state.automation_cipher.key()
}

pub fn router(state: AgentState) -> Router {
    Router::new()
        .route("/api/v1/automations", any(handle))
        .route("/api/v1/automations/{*rest}", any(handle))
        .with_state(state)
}
async fn handle(State(state): State<AgentState>, request: Request) -> Response {
    let result = dispatch(&state, request).await;
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
async fn dispatch(state: &AgentState, request: Request) -> Result<Value, AgentError> {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    let parts = path
        .trim_start_matches("/api/v1/automations")
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if method == "POST" && parts.len() == 2 && parts[1] == "webhook" {
        return webhook(state, request, parts[0]).await;
    }
    super::public::reject_forgery(request.headers(), None)?;
    let actor = Actor::resolve(&state.auth, request.headers(), &path).await?;
    let pool = &state.repository.pool;
    let expected = request
        .headers()
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim_matches('"').parse::<i64>().ok());
    match (method.as_str(), parts.as_slice()) {
        ("GET", []) => {
            let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(r)-'webhook_secret_ciphertext' FROM gateway.automation_rules r WHERE tenant_id=$1 AND user_id=$2 ORDER BY created_at DESC,automation_id DESC LIMIT 100").bind(&actor.tenant_id).bind(&actor.user_id).fetch_all(pool).await?;
            Ok(json!({"automations":rows}))
        }
        ("GET", ["notifications"]) => {
            let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(n) FROM gateway.automation_notifications n WHERE tenant_id=$1 AND user_id=$2 ORDER BY created_at DESC LIMIT 100").bind(&actor.tenant_id).bind(&actor.user_id).fetch_all(pool).await?;
            Ok(json!({"notifications":rows}))
        }
        ("POST", []) => {
            let bytes = to_bytes(request.into_body(), 262144)
                .await
                .map_err(|_| invalid())?;
            let d: Definition = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            create(pool, &actor, d).await
        }
        ("POST", ["draft"]) => {
            let bytes = to_bytes(request.into_body(), 16384)
                .await
                .map_err(|_| invalid())?;
            let body: DraftRequest = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if !bounded(&body.prompt, 8000) {
                return Err(invalid());
            }
            let d = generate_draft(state, &actor, &body.prompt).await?;
            create(pool, &actor, d).await
        }
        ("GET", [id]) => Ok(json!({"automation":public(owned(pool,&actor,id).await?)})),
        ("GET", [id, "executions"]) => {
            let row = owned(pool, &actor, id).await?;
            let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(d)-'action' FROM gateway.automation_deliveries d WHERE automation_id=$1::uuid AND tenant_id=$2 AND user_id=$3 ORDER BY created_at DESC,action_index LIMIT 100").bind(text(&row,"automation_id")).bind(&actor.tenant_id).bind(&actor.user_id).fetch_all(pool).await?;
            Ok(json!({"executions":rows}))
        }
        ("POST", [id, "confirm"]) => {
            let bytes = to_bytes(request.into_body(), 1024)
                .await
                .map_err(|_| invalid())?;
            require_confirmation(&bytes)?;
            let expected =
                expected.ok_or_else(|| AgentError::code(428, "AUTOMATION_REVISION_REQUIRED"))?;
            let row = owned(pool, &actor, id).await?;
            let d = definition(&row)?;
            let (next, watermark) = match &d.trigger {
                Trigger::Cron {
                    expression,
                    timezone,
                } => (
                    Some(
                        cron::Cron::parse(expression)?
                            .next(pool, timezone, Utc::now())
                            .await?,
                    ),
                    None,
                ),
                Trigger::SpacePageChanged { item_id } => {
                    let value = state
                        .automation_knowledge
                        .as_ref()
                        .ok_or_else(|| AgentError::code(503, "AUTOMATION_KNOWLEDGE_UNAVAILABLE"))?
                        .read(&actor, item_id)
                        .await?;
                    (
                        None,
                        Some(value["revision"].as_i64().ok_or_else(invalid)?.to_string()),
                    )
                }
                Trigger::ConnectorEvent { .. } => (
                    None,
                    Some(json!({"at":Utc::now().to_rfc3339(),"id":""}).to_string()),
                ),
                _ => (None, None),
            };
            let secret = matches!(d.trigger, Trigger::Webhook {}).then(|| {
                format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                )
            });
            let cipher = secret
                .as_ref()
                .map(|s| {
                    NativeProviderCipher::new()
                        .encrypt(s, key(state)?)
                        .map_err(|_| AgentError::code(503, "AUTOMATION_CIPHER_UNAVAILABLE"))
                })
                .transpose()?;
            let updated:Option<Value>=sqlx::query_scalar("UPDATE gateway.automation_rules SET status='active',revision=revision+1,next_run_at=$5,watermark=$6,webhook_secret_ciphertext=$7,updated_at=now() WHERE automation_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND revision=$4 AND status='draft' RETURNING to_jsonb(automation_rules)-'webhook_secret_ciphertext'").bind(super::uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).bind(expected).bind(next).bind(watermark).bind(cipher).fetch_optional(pool).await?;
            let updated =
                updated.ok_or_else(|| AgentError::code(409, "AUTOMATION_DRAFT_CHANGED"))?;
            Ok(json!({"automation":updated,"webhook_secret":secret}))
        }
        ("POST", [id, action]) if matches!(*action, "pause" | "resume" | "cancel") => {
            let expected =
                expected.ok_or_else(|| AgentError::code(428, "AUTOMATION_REVISION_REQUIRED"))?;
            let status = match *action {
                "resume" => "active",
                "cancel" => "cancelled",
                _ => "paused",
            };
            // UPDATE owns the row lock and rechecks owner, revision and state
            // atomically. Draft withdrawal must not open a resume-to-active path.
            let row:Option<Value>=sqlx::query_scalar("UPDATE gateway.automation_rules SET status=$5,revision=revision+1,updated_at=now() WHERE automation_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND revision=$4 AND status=ANY($6::text[]) RETURNING to_jsonb(automation_rules)-'webhook_secret_ciphertext'").bind(super::uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).bind(expected).bind(status).bind(transition_sources(action)).fetch_optional(pool).await?;
            Ok(
                json!({"automation":row.ok_or_else(||AgentError::code(409,"AUTOMATION_STATE_CHANGED"))?}),
            )
        }
        _ => Err(AgentError::code(404, "AUTOMATION_NOT_FOUND")),
    }
}
fn transition_sources(action: &str) -> &'static [&'static str] {
    if action == "cancel" {
        &["draft", "active", "paused"]
    } else {
        &["active", "paused"]
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftRequest {
    prompt: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    confirm: bool,
}
fn require_confirmation(bytes: &[u8]) -> Result<(), AgentError> {
    let confirmation: Confirmation = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if !confirmation.confirm {
        return Err(invalid());
    }
    Ok(())
}
async fn create(pool: &PgPool, actor: &Actor, d: Definition) -> Result<Value, AgentError> {
    d.validate()?;
    let id = uuid::Uuid::new_v4();
    let row:Value=sqlx::query_scalar("INSERT INTO gateway.automation_rules(automation_id,tenant_id,user_id,title,definition) VALUES($1,$2,$3,$4,$5) RETURNING to_jsonb(automation_rules)-'webhook_secret_ciphertext'").bind(id).bind(&actor.tenant_id).bind(&actor.user_id).bind(&d.title).bind(serde_json::to_value(&d).map_err(|_|invalid())?).fetch_one(pool).await?;
    Ok(json!({"automation":row}))
}
async fn owned(pool: &PgPool, actor: &Actor, id: &str) -> Result<Value, AgentError> {
    sqlx::query_scalar("SELECT to_jsonb(r) FROM gateway.automation_rules r WHERE automation_id=$1::uuid AND tenant_id=$2 AND user_id=$3").bind(super::uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(pool).await?.ok_or_else(||AgentError::code(404,"AUTOMATION_NOT_FOUND"))
}
async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    row: &Value,
    event: &str,
) -> Result<(), AgentError> {
    for (index, action) in definition(row)?.actions.iter().enumerate() {
        sqlx::query("INSERT INTO gateway.automation_deliveries(delivery_id,automation_id,tenant_id,user_id,event_key,action_index,action) VALUES($1,$2::uuid,$3,$4,$5,$6,$7) ON CONFLICT(automation_id,event_key,action_index) DO NOTHING").bind(uuid::Uuid::new_v4()).bind(text(row,"automation_id")).bind(text(row,"tenant_id")).bind(text(row,"user_id")).bind(event).bind(index as i32).bind(serde_json::to_value(action).map_err(|_|invalid())?).execute(&mut **tx).await?;
    }
    Ok(())
}
async fn webhook(state: &AgentState, request: Request, id: &str) -> Result<Value, AgentError> {
    let headers = request.headers().clone();
    let read = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let bytes = to_bytes(request.into_body(), 65536)
        .await
        .map_err(|_| invalid())?;
    let mut tx = state.repository.pool.begin().await?;
    let row:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(r) FROM gateway.automation_rules r WHERE automation_id=$1::uuid AND status='active' FOR UPDATE").bind(super::uuid(id)?).fetch_optional(&mut *tx).await?;
    let row = row.ok_or_else(|| AgentError::code(404, "AUTOMATION_NOT_FOUND"))?;
    if !matches!(definition(&row)?.trigger, Trigger::Webhook {}) {
        return Err(AgentError::code(404, "AUTOMATION_NOT_FOUND"));
    }
    let secret = NativeProviderCipher::new()
        .decrypt(text(&row, "webhook_secret_ciphertext"), key(state)?)
        .map_err(|_| AgentError::code(503, "AUTOMATION_CIPHER_UNAVAILABLE"))?;
    let delivery = read("x-automation-delivery");
    webhooks::verify(
        &secret,
        &read("x-automation-timestamp"),
        &delivery,
        &read("x-automation-signature"),
        &bytes,
        Utc::now().timestamp(),
    )?;
    let event = format!("webhook:{delivery}");
    let duplicate:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.automation_deliveries WHERE automation_id=$1::uuid AND event_key=$2)").bind(text(&row,"automation_id")).bind(&event).fetch_one(&mut *tx).await?;
    if duplicate {
        return Err(AgentError::code(409, "AUTOMATION_WEBHOOK_REPLAY"));
    }
    enqueue(&mut tx, &row, &event).await?;
    tx.commit().await?;
    Ok(json!({"accepted":true}))
}
async fn generate_draft(
    state: &AgentState,
    actor: &Actor,
    prompt: &str,
) -> Result<Definition, AgentError> {
    if !state.runtime_enabled {
        return Err(AgentError::code(503, "AGENT_RUNTIME_DISABLED"));
    }
    state.edge_rate.check(actor, "assistant_chat").await?;
    let session = uuid::Uuid::new_v4().to_string();
    routes::ensure_builtin_session(state, actor, &session).await?;
    let instructions = format!(
        "Return only a JSON automation draft, never execute actions or activate anything. Schema: {{\"title\":string,\"model_id\":string,\"trigger\":{{\"kind\":\"cron\",\"expression\":\"0 9 * * 1-5\",\"timezone\":\"America/New_York\"}}|{{\"kind\":\"webhook\"}}|{{\"kind\":\"space_page_changed\",\"item_id\":string}}|{{\"kind\":\"connector_event\",\"entry_id\":string}},\"actions\":[{{\"kind\":\"run_agent\",\"prompt\":string}}|{{\"kind\":\"run_skill\",\"skill_id\":string,\"prompt\":string}}|{{\"kind\":\"write_space_page\",\"item_id\":string,\"markdown\":string}}|{{\"kind\":\"notify\",\"message\":string}}]}}. Default model {}. Never invent an item or skill ID; use IDs provided by the user. Request: {}",
        state.default_model, prompt
    );
    let turn=state.control.start_turn(super::control::TurnRequest{actor:actor.clone(),session_id:session.clone(),message:instructions,model_id:state.default_model.clone(),memory_mode:"off".into(),enable_dynamic_tools:false,readonly_capabilities:json!({"knowledge":{"dataset_ids":[],"mode":"off"},"web_search":{"enabled":false},"responses_tool_choice":"none"}),..Default::default()}).await?;
    let mut events = state
        .control
        .stream_thread_events(
            actor.scope(&session),
            turn.runtime_thread_id.clone(),
            turn.after_sequence,
            1000,
            Some(turn.run_id.clone()),
        )
        .await?;
    let read = async {
        let mut output = String::new();
        while let Some(event) = events.recv().await {
            let event = event?;
            if event["event_type"] == "text_delta" {
                if let Some(text) = event["data"]["content"].as_str() {
                    output.push_str(text);
                    if output.len() > 65536 {
                        return Err(invalid());
                    }
                }
            }
        }
        let draft: Definition = serde_json::from_str(output.trim())
            .map_err(|_| AgentError::code(502, "AUTOMATION_DRAFT_INVALID"))?;
        draft.validate()?;
        Ok(draft)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(120), read).await {
        Ok(result) => result,
        Err(_) => {
            let _ = state
                .control
                .interrupt_turn(
                    &actor.scope(&session),
                    &turn.runtime_thread_id,
                    &turn.run_id,
                    "automation_draft_timeout",
                )
                .await;
            Err(AgentError::code(504, "AUTOMATION_DRAFT_TIMEOUT"))
        }
    }
}

#[cfg(test)]
mod scheduled_tasks {
    use super::*;
    fn draft() -> Value {
        json!({"title":"Weekday reminder","model_id":"fixture-model","trigger":{"kind":"cron","expression":"0 9 * * 1-5","timezone":"America/New_York"},"actions":[{"kind":"notify","message":"Review checklist"}]})
    }
    #[test]
    fn actions_are_bounded_and_destinations_cannot_escape_the_space_route() {
        let d: Definition = serde_json::from_value(draft()).unwrap();
        d.validate().unwrap();
        let mut empty = d.clone();
        empty.actions.clear();
        assert!(empty.validate().is_err());
        let mut overflow = d.clone();
        overflow.actions = vec![
            Action::Notify {
                message: "fixture".into()
            };
            9
        ];
        assert!(overflow.validate().is_err());
        for id in ["..", ".", "../foreign", "item/elsewhere", ""] {
            let mut value = d.clone();
            value.actions = vec![Action::WriteSpacePage {
                item_id: id.into(),
                markdown: "fixture".into(),
            }];
            assert!(value.validate().is_err());
        }
        let mut invalid_cron = d;
        invalid_cron.trigger = Trigger::Cron {
            expression: "*/0 * * * *".into(),
            timezone: "UTC".into(),
        };
        assert!(invalid_cron.validate().is_err());
        let mut webhook = draft();
        webhook["trigger"] = json!({"kind":"webhook"});
        assert!(serde_json::from_value::<Definition>(webhook.clone()).is_ok());
        webhook["trigger"]["timezone"] = json!("UTC");
        assert!(serde_json::from_value::<Definition>(webhook).is_err());
    }
    #[test]
    fn only_explicit_unambiguous_confirmation_can_activate_a_draft() {
        require_confirmation(br#"{"confirm":true}"#).unwrap();
        for bytes in [
            br#"{}"#.as_slice(),
            br#"{"confirm":false}"#,
            br#"{"confirm":"true"}"#,
            br#"{"confirm":true,"actions":[]}"#,
            br#"{"confirm":false,"confirm":true}"#,
        ] {
            assert!(require_confirmation(bytes).is_err());
        }
        let mut value = draft();
        value["status"] = json!("active");
        assert!(serde_json::from_value::<Definition>(value).is_err());
        assert!(transition_sources("cancel").contains(&"draft"));
        for action in ["pause", "resume"] {
            assert!(!transition_sources(action).contains(&"draft"));
        }
        for action in ["pause", "resume", "cancel"] {
            assert!(!transition_sources(action).contains(&"cancelled"));
        }
    }
    #[test]
    fn public_receipt_never_contains_the_encrypted_webhook_secret() {
        let receipt = public(
            json!({"automation_id":"fixture","webhook_secret_ciphertext":"enc:synthetic-fixture"}),
        );
        assert!(receipt.get("webhook_secret_ciphertext").is_none());
        assert_eq!(receipt["automation_id"], "fixture");
    }
    #[test]
    fn webhook_cipher_binds_injected_configuration_and_missing_keys_fail_closed() {
        for value in ["", "   "] {
            let error = AutomationCipherKey::from_configuration(value)
                .key()
                .unwrap_err();
            assert_eq!(error.status, 503);
        }
        assert!(AutomationCipherKey::default().key().is_err());
        let configured = AutomationCipherKey::from_configuration("synthetic-automation-key");
        let other = AutomationCipherKey::from_configuration("different-synthetic-key");
        let cipher = NativeProviderCipher::new();
        let sealed = cipher
            .encrypt("synthetic-webhook-secret", configured.key().unwrap())
            .unwrap();
        assert_eq!(
            cipher.decrypt(&sealed, configured.key().unwrap()).unwrap(),
            "synthetic-webhook-secret"
        );
        assert!(cipher.decrypt(&sealed, other.key().unwrap()).is_err());
    }
}
