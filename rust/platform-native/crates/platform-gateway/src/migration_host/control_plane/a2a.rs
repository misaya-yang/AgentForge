//! A2A 1.0 HTTP+JSON binding over immutable publications and managed turns.
//! Dispatch is durable and at-most-once: an unconfirmed external effect is never
//! retried automatically. Optional streaming/push capabilities are not advertised.
use super::config::{A2aConfiguration, OutboundAgent};
use crate::migration_agent::{Actor, AgentError, entry::SourceAccess, routes::AgentState, text};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, require_capability},
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
#[derive(Clone)]
pub struct Server {
    configuration: A2aConfiguration,
    pub(super) agent: AgentState,
    admission: Arc<AdmissionService>,
    rbac: Arc<Rbac>,
    signing: Arc<SigningKey>,
}
pub(super) fn err(status: u16, code: &str) -> AgentError {
    AgentError::code(status, code)
}
fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string()))
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    role: String,
    parts: Vec<TextPart>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextPart {
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendConfiguration {
    #[serde(default)]
    return_immediately: bool,
    #[serde(default)]
    accepted_output_modes: Vec<String>,
    history_length: Option<u32>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendRequest {
    message: Message,
    #[serde(default)]
    configuration: SendConfiguration,
}
fn validate_message(request: &SendRequest) -> Result<String, AgentError> {
    let m = &request.message;
    if uuid::Uuid::parse_str(&m.message_id).is_err()
        || m.role != "ROLE_USER"
        || m.parts.is_empty()
        || m.parts.len() > 32
        || m.task_id.is_some()
        || m.context_id
            .as_ref()
            .is_some_and(|v| uuid::Uuid::parse_str(v).is_err())
        || request
            .configuration
            .accepted_output_modes
            .iter()
            .any(|v| v != "text/plain")
        || request
            .configuration
            .history_length
            .is_some_and(|n| n > 100)
    {
        return Err(err(400, "INVALID_ARGUMENT"));
    }
    let mut message = String::new();
    for part in &m.parts {
        if part.media_type.as_ref().is_some_and(|v| v != "text/plain") {
            return Err(err(400, "CONTENT_TYPE_NOT_SUPPORTED"));
        }
        if !message.is_empty() {
            message.push('\n');
        }
        message.push_str(&part.text);
    }
    if message.trim().is_empty() || message.len() > 256 * 1024 {
        return Err(err(400, "INVALID_ARGUMENT"));
    }
    Ok(message)
}
impl Server {
    pub fn new(
        configuration: A2aConfiguration,
        agent: AgentState,
        admission: Arc<AdmissionService>,
        rbac: Arc<Rbac>,
    ) -> Result<Self, AgentError> {
        if !agent.runtime_enabled || !agent.publish_enabled || agent.public.is_none() {
            return Err(err(503, "A2A_PUBLICATION_RUNTIME_UNAVAILABLE"));
        }
        let signing = SigningKey::from_pkcs8_pem(&configuration.signing_private_key)
            .map_err(|_| err(503, "A2A_SIGNING_KEY_INVALID"))?;
        Ok(Self {
            configuration,
            agent,
            admission,
            rbac,
            signing: Arc::new(signing),
        })
    }
    async fn actor(&self, headers: &HeaderMap, invoke: bool) -> Result<Actor, AgentError> {
        if headers
            .get("a2a-version")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v != "1.0")
        {
            return Err(err(400, "VERSION_NOT_SUPPORTED"));
        }
        let admission = self.admission.request(headers, "/a2a");
        let p = admission
            .resolve()
            .await
            .map_err(|e| err(e.status(), "AUTHENTICATION_REQUIRED"))?;
        if invoke {
            self.agent
                .edge_rate
                .check(
                    &Actor {
                        tenant_id: p.tenant_id().into(),
                        user_id: p.user_id().into(),
                        roles: p.roles().to_vec(),
                        tier: p.tier().into(),
                    },
                    "assistant_chat",
                )
                .await?;
            require_capability(
                &self.rbac,
                p.roles(),
                p.permissions(),
                Capability::AgentInvoke,
            )
            .map_err(|_| err(403, "PERMISSION_DENIED"))?;
        }
        Ok(Actor {
            tenant_id: p.tenant_id().into(),
            user_id: p.user_id().into(),
            roles: p.roles().to_vec(),
            tier: p.tier().into(),
        })
    }
    async fn publication(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let pin = self
            .configuration
            .publications
            .iter()
            .find(|p| p.publication_id == id)
            .ok_or_else(|| err(404, "AGENT_NOT_FOUND"))?;
        let resolution = self
            .agent
            .repository
            .resolve_publication(actor, id, Some(&pin.version_id))
            .await?;
        // A rollover cannot silently retarget an advertised immutable interface.
        if resolution["publication"]["version_id"] != pin.version_id
            || resolution["version"]["agent_version_id"] != pin.version_id
        {
            return Err(err(409, "PUBLICATION_VERSION_CHANGED"));
        }
        Ok(resolution)
    }
    async fn card(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let r = self.publication(actor, id).await?;
        let name = format!("Published Agent {}", text(&r["agent"], "agent_id"));
        let description = format!(
            "Immutable Agent version {}",
            text(&r["version"], "agent_version_id")
        );
        let card = json!({"name":name,"description":description,"version":r["version"]["agent_version_id"],"supportedInterfaces":[{"url":format!("{}/a2a/{id}",self.configuration.origin.trim_end_matches('/')),"protocolBinding":"HTTP+JSON","protocolVersion":"1.0"}],"capabilities":{"streaming":false,"pushNotifications":false,"extendedAgentCard":false},"securitySchemes":{"gateway":{"httpAuthSecurityScheme":{"scheme":"Bearer","bearerFormat":"JWT"}}},"securityRequirements":[{"schemes":{"gateway":{"list":[]}}}],"defaultInputModes":["text/plain"],"defaultOutputModes":["text/plain"],"skills":[{"id":id,"name":name,"description":description,"tags":["immutable-publication"]}]});
        sign_card(card, &self.configuration.signing_key_id, &self.signing)
    }
    async fn task(&self, actor: &Actor, publication: &str, id: &str) -> Result<Value, AgentError> {
        uuid::Uuid::parse_str(id).map_err(|_| err(404, "TASK_NOT_FOUND"))?;
        self.publication(actor, publication).await?;
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM gateway.gateway_a2a_tasks t WHERE tenant_id=$1 AND user_id=$2 AND publication_id=$3::uuid AND task_id=$4::uuid").bind(&actor.tenant_id).bind(&actor.user_id).bind(publication).bind(id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await?.ok_or_else(||err(404,"TASK_NOT_FOUND"))?;
        let run = if let Some(run) = row["run_id"].as_str() {
            sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM assistant_runs r WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND run_id=$4::uuid AND engine='agent_runtime'").bind(&actor.tenant_id).bind(&actor.user_id).bind(text(&row,"context_id")).bind(run).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await?
        } else {
            None
        };
        let status = run.as_ref().map(|r| text(r, "status")).unwrap_or(
            if row["effect_state"] == "reserved" {
                "queued"
            } else if row["effect_state"] == "rejected" {
                "rejected"
            } else {
                "unknown"
            },
        );
        let state = task_state(status);
        let mut result = json!({"id":id,"contextId":row["context_id"],"status":{"state":state},"metadata":{"effectState":row["effect_state"]}});
        if state == "TASK_STATE_COMPLETED" {
            let scope = actor.scope(text(&row, "context_id"));
            self.agent
                .sources
                .require_conversation(&scope, true)
                .await?;
            if let Some((messages, _)) = self
                .agent
                .control
                .threads
                .history_messages(&scope, 500, &self.agent.sources)
                .await?
            {
                let output = messages
                    .iter()
                    .filter(|m| {
                        m["role"] == "assistant" && m["metadata"]["runtime_run_id"] == row["run_id"]
                    })
                    .filter_map(|m| m["content"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !output.is_empty() {
                    result["artifacts"] = json!([{"artifactId":format!("{id}-output"),"parts":[{"text":output,"mediaType":"text/plain"}]}]);
                }
            }
        }
        Ok(result)
    }
    async fn send(
        &self,
        actor: &Actor,
        publication: &str,
        request: SendRequest,
    ) -> Result<Value, AgentError> {
        let message = validate_message(&request)?;
        let resolution = self.publication(actor, publication).await?;
        let snapshot = self
            .agent
            .build_snapshot(
                actor,
                &resolution,
                text(&resolution["publication"], "channel"),
            )
            .await?;
        let hash =
            digest(&serde_json::to_value(&request).map_err(|_| err(400, "INVALID_ARGUMENT"))?);
        let existing:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM gateway.gateway_a2a_tasks t WHERE tenant_id=$1 AND user_id=$2 AND publication_id=$3::uuid AND message_id=$4::uuid").bind(&actor.tenant_id).bind(&actor.user_id).bind(publication).bind(&request.message.message_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await?;
        if let Some(row) = existing {
            if row["request_hash"] != hash {
                return Err(err(409, "MESSAGE_ID_CONFLICT"));
            }
            return Ok(json!({"task":self.task(actor,publication,text(&row,"task_id")).await?}));
        }
        self.agent
            .public
            .as_ref()
            .ok_or_else(|| err(503, "A2A_PUBLICATION_RUNTIME_UNAVAILABLE"))?
            .enforce_limits(
                &self.agent.repository,
                &resolution["publication"],
                &actor.user_id,
                "protocol:a2a",
            )
            .await?;
        let context = request
            .message
            .context_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let task_id = uuid::Uuid::new_v4().to_string();
        let mut tx = ai_platform_gateway_foundation::tenant_database::begin(
            &self.agent.repository.pool,
            &actor.tenant_id,
        )
        .await?;
        let context_exists=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM gateway.gateway_a2a_contexts WHERE context_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND publication_id=$4::uuid AND version_id=$5::uuid)").bind(&context).bind(&actor.tenant_id).bind(&actor.user_id).bind(publication).bind(text(&resolution["version"],"agent_version_id")).fetch_one(&mut *tx).await?;
        if request.message.context_id.is_some() && !context_exists {
            return Err(err(404, "CONTEXT_NOT_FOUND"));
        }
        if !context_exists {
            sqlx::query("INSERT INTO gateway.gateway_a2a_contexts(context_id,tenant_id,user_id,publication_id,version_id) VALUES($1::uuid,$2,$3,$4::uuid,$5::uuid)").bind(&context).bind(&actor.tenant_id).bind(&actor.user_id).bind(publication).bind(text(&resolution["version"],"agent_version_id")).execute(&mut *tx).await?;
        }
        // Context lock serializes message admission, including two Gateway replicas.
        sqlx::query(
            "SELECT context_id FROM gateway.gateway_a2a_contexts WHERE context_id=$1::uuid FOR UPDATE",
        )
        .bind(&context)
        .execute(&mut *tx)
        .await?;
        let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.gateway_a2a_tasks t WHERE t.context_id=$1::uuid AND (t.effect_state IN ('reserved','dispatched','unknown') OR t.effect_state='confirmed' AND NOT EXISTS(SELECT 1 FROM assistant_runs r WHERE r.run_id=t.run_id AND r.tenant_id=t.tenant_id AND r.user_id=t.user_id AND r.session_id=t.context_id::text AND r.status IN ('completed','succeeded','failed','error','cancelled','canceled','rejected'))))").bind(&context).fetch_one(&mut *tx).await?;
        if active {
            return Err(err(409, "CONTEXT_BUSY"));
        }
        let inserted=sqlx::query("INSERT INTO gateway.gateway_a2a_tasks(task_id,context_id,tenant_id,user_id,publication_id,message_id,request_hash,effect_state) VALUES($1::uuid,$2::uuid,$3,$4,$5::uuid,$6::uuid,$7,'reserved') ON CONFLICT(tenant_id,user_id,publication_id,message_id) DO NOTHING").bind(&task_id).bind(&context).bind(&actor.tenant_id).bind(&actor.user_id).bind(publication).bind(&request.message.message_id).bind(&hash).execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        if inserted != 1 {
            return Err(err(409, "MESSAGE_ALREADY_ADMITTED"));
        }
        let scope = actor.scope(&context);
        // Bind the immutable publication before any managed execution. A conflicting
        // existing session fails closed; no user-supplied model/tool overrides exist.
        if let Err(error) = self.agent.bind_pin(&scope, &snapshot, None).await {
            let _=sqlx::query("UPDATE gateway.gateway_a2a_tasks SET effect_state='rejected',updated_at=NOW() WHERE task_id=$1::uuid AND effect_state='reserved'").bind(&task_id).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await;
            return Err(error);
        }
        self.agent
            .sources
            .require_conversation(&scope, true)
            .await?;
        sqlx::query("UPDATE gateway.gateway_a2a_tasks SET effect_state='dispatched',updated_at=NOW() WHERE task_id=$1::uuid AND effect_state='reserved'").bind(&task_id).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await?;
        let turn = match self
            .agent
            .start_snapshot_turn(actor.clone(), context.clone(), message, snapshot)
            .await
        {
            Ok(turn) => turn,
            Err(error) => {
                let _=sqlx::query("UPDATE gateway.gateway_a2a_tasks SET effect_state='unknown',updated_at=NOW() WHERE task_id=$1::uuid").bind(&task_id).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await;
                return Err(error);
            }
        };
        sqlx::query("UPDATE gateway.gateway_a2a_tasks SET effect_state='confirmed',run_id=$2::uuid,thread_id=$3::uuid,updated_at=NOW() WHERE task_id=$1::uuid").bind(&task_id).bind(&turn.run_id).bind(&turn.runtime_thread_id).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await?;
        if request.configuration.return_immediately {
            return Ok(json!({"task":self.task(actor,publication,&task_id).await?}));
        }
        // HTTP deadlines do not cancel/re-submit a managed run. A timeout tells callers
        // the durable task ID and they can query it using GetTask.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(110);
        loop {
            let task = self.task(actor, publication, &task_id).await?;
            let state = text(&task["status"], "state");
            if !matches!(state, "TASK_STATE_WORKING" | "TASK_STATE_SUBMITTED") {
                return Ok(json!({"task":task}));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AgentError {
                    status: 504,
                    detail: json!({"code":"DEADLINE_EXCEEDED","taskId":task_id,"contextId":context}),
                });
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn cancel(
        &self,
        actor: &Actor,
        publication: &str,
        id: &str,
    ) -> Result<Value, AgentError> {
        let task = self.task(actor, publication, id).await?;
        if !matches!(
            text(&task["status"], "state"),
            "TASK_STATE_WORKING" | "TASK_STATE_SUBMITTED" | "TASK_STATE_INPUT_REQUIRED"
        ) {
            return Err(err(409, "TASK_NOT_CANCELABLE"));
        }
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM gateway.gateway_a2a_tasks t WHERE task_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND publication_id=$4::uuid").bind(id).bind(&actor.tenant_id).bind(&actor.user_id).bind(publication).fetch_one(ai_platform_gateway_foundation::tenant_database::executor(&self.agent.repository.pool, &actor.tenant_id)).await?;
        if text(&row, "thread_id").is_empty() || text(&row, "run_id").is_empty() {
            return Err(err(409, "TASK_NOT_CANCELABLE"));
        }
        self.agent
            .control
            .interrupt_turn(
                &actor.scope(text(&row, "context_id")),
                text(&row, "thread_id"),
                text(&row, "run_id"),
                "a2a_cancel",
            )
            .await?;
        // A successful interrupt request is not proof that execution is canceled.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let task = self.task(actor, publication, id).await?;
            match text(&task["status"], "state") {
                "TASK_STATE_CANCELED" => return Ok(task),
                "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_REJECTED" => {
                    return Err(err(409, "TASK_NOT_CANCELABLE"));
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AgentError {
                    status: 504,
                    detail: json!({"code":"CANCELLATION_UNCONFIRMED","taskId":id}),
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
pub async fn verify_schema(pool: &sqlx::PgPool) -> Result<(), AgentError> {
    // Consume the same named-column minimum/forbidden ACL contract as authority
    // admission. Requiring table UPDATE rejects the intended least-privilege role.
    let catalog: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../docs/design/proposals/service-successors/catalog.json"
    )))
    .map_err(|_| err(503, "A2A_SCHEMA_UNAVAILABLE"))?;
    let acl = catalog["admission_requirements"]["gateway_a2a"]
        .as_str()
        .ok_or_else(|| err(503, "A2A_SCHEMA_UNAVAILABLE"))?;
    let query = sqlx::query_scalar::<_, bool>(acl);
    let ready = tokio::time::timeout(Duration::from_secs(3), query.fetch_one(pool))
        .await
        .map_err(|_| err(503, "A2A_SCHEMA_UNAVAILABLE"))?
        .map_err(|_| err(503, "A2A_SCHEMA_UNAVAILABLE"))?;
    let columns:bool=tokio::time::timeout(Duration::from_secs(3),sqlx::query_scalar("SELECT COUNT(*)=5 AND has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','remote_task_id','UPDATE') AND has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','cancel_state','UPDATE') AND NOT has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','remote_message_id','UPDATE') AND NOT has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','card_sha256','UPDATE') AND NOT has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','card_key_id','UPDATE') FROM information_schema.columns WHERE table_schema='gateway' AND table_name='gateway_a2a_outbound_effects' AND column_name IN ('remote_message_id','remote_task_id','card_sha256','card_key_id','cancel_state')").fetch_one(pool)).await.map_err(|_|err(503,"A2A_SCHEMA_UNAVAILABLE"))?.map_err(|_|err(503,"A2A_SCHEMA_UNAVAILABLE"))?;
    if !ready || !columns {
        return Err(err(503, "A2A_SCHEMA_UNAVAILABLE"));
    }
    Ok(())
}
fn task_state(status: &str) -> &'static str {
    match status {
        "completed" | "succeeded" => "TASK_STATE_COMPLETED",
        "failed" | "error" => "TASK_STATE_FAILED",
        "rejected" => "TASK_STATE_REJECTED",
        "cancelled" | "canceled" => "TASK_STATE_CANCELED",
        "requires_action" | "waiting_approval" => "TASK_STATE_INPUT_REQUIRED",
        "queued" | "pending" => "TASK_STATE_SUBMITTED",
        "running" | "in_progress" => "TASK_STATE_WORKING",
        _ => "TASK_STATE_UNSPECIFIED",
    }
}
fn card_bytes(card: &Value) -> Result<Vec<u8>, AgentError> {
    let raw = serde_json::to_vec(card).map_err(|_| err(503, "A2A_CARD_INVALID"))?;
    ai_platform_gateway_foundation::canonical_json_v2::CanonicalDocument::from_json(&raw)
        .and_then(|document| document.to_bytes())
        .map_err(|_| err(503, "A2A_CARD_INVALID"))
}
fn sign_card(mut card: Value, kid: &str, key: &SigningKey) -> Result<Value, AgentError> {
    // Use the shared RFC 8785 implementation even when serde_json preserves input order.
    let protected=B64.encode(json!({"alg":"EdDSA","kid":kid,"jwk":{"kty":"OKP","crv":"Ed25519","x":B64.encode(key.verifying_key().as_bytes())}}).to_string());
    let payload = B64.encode(card_bytes(&card)?);
    let signature = key.sign(format!("{protected}.{payload}").as_bytes());
    card["signatures"] =
        json!([{"protected":protected,"signature":B64.encode(signature.to_bytes())}]);
    Ok(card)
}
pub fn router(server: Server) -> Router {
    Router::new()
        .route("/a2a/.well-known/jwks.json", get(jwks))
        .route("/a2a/{publication}/tasks", get(list_tasks))
        .route("/a2a/{publication}/.well-known/agent-card.json", get(card))
        .route("/a2a/{publication}/message:send", post(send))
        .route("/a2a/{publication}/tasks/{task}", get(task).post(task))
        .route("/a2a/outbound/{name}/message:send", post(outbound))
        .route(
            "/a2a/outbound/{name}/tasks/{message}",
            get(outbound_task).post(outbound_task),
        )
        .with_state(server)
}
async fn jwks(State(s): State<Server>) -> Json<Value> {
    Json(
        json!({"keys":[{"kty":"OKP","crv":"Ed25519","use":"sig","alg":"EdDSA","kid":s.configuration.signing_key_id,"x":B64.encode(s.signing.verifying_key().as_bytes())}]}),
    )
}
async fn list_tasks(
    State(s): State<Server>,
    Path(publication): Path<String>,
    request: Request,
) -> Response {
    let (parts, _) = request.into_parts();
    let result=async move{
 let actor=s.actor(&parts.headers,false).await?;s.publication(&actor,&publication).await?;
 let url=url::Url::parse(&format!("https://gateway{}",parts.uri)).map_err(|_|err(400,"INVALID_ARGUMENT"))?;
 let mut page_size=50i64;let mut context=None;let mut cursor=None;
 for (key,value) in url.query_pairs(){match key.as_ref(){"pageSize"=>page_size=value.parse::<i64>().ok().filter(|n|(1..=100).contains(n)).ok_or_else(||err(400,"INVALID_ARGUMENT"))?,"contextId"=>{uuid::Uuid::parse_str(&value).map_err(|_|err(400,"INVALID_ARGUMENT"))?;context=Some(value.into_owned());},"pageToken"=>{if !value.is_empty(){uuid::Uuid::parse_str(&value).map_err(|_|err(400,"INVALID_ARGUMENT"))?;cursor=Some(value.into_owned());}},"historyLength" if value=="0"=>{},_=>return Err(err(400,"UNSUPPORTED_FILTER"))}}
 let ids=sqlx::query_scalar::<_,String>("SELECT task_id::text FROM gateway.gateway_a2a_tasks t WHERE tenant_id=$1 AND user_id=$2 AND publication_id=$3::uuid AND ($4::text IS NULL OR context_id::text=$4) AND ($6::text IS NULL OR (created_at,task_id)<(SELECT created_at,task_id FROM gateway.gateway_a2a_tasks WHERE task_id=$6::uuid AND tenant_id=$1 AND user_id=$2 AND publication_id=$3::uuid)) ORDER BY created_at DESC,task_id DESC LIMIT $5").bind(&actor.tenant_id).bind(&actor.user_id).bind(&publication).bind(&context).bind(page_size+1).bind(cursor).fetch_all(&s.agent.repository.pool).await?;
 let total:i64=sqlx::query_scalar("SELECT COUNT(*) FROM gateway.gateway_a2a_tasks WHERE tenant_id=$1 AND user_id=$2 AND publication_id=$3::uuid AND ($4::text IS NULL OR context_id::text=$4)").bind(&actor.tenant_id).bind(&actor.user_id).bind(&publication).bind(context).fetch_one(&s.agent.repository.pool).await?;
 let next=if ids.len()>page_size as usize{ids.get(page_size as usize-1).cloned().unwrap_or_default()}else{String::new()};
 let mut tasks=Vec::new();for id in ids.into_iter().take(page_size as usize){tasks.push(s.task(&actor,&publication,&id).await?);}Ok(json!({"tasks":tasks,"pageSize":page_size,"nextPageToken":next,"totalSize":total}))
 }.await;
    respond(result)
}
async fn card(
    State(s): State<Server>,
    Path(publication): Path<String>,
    headers: HeaderMap,
) -> Response {
    let r = async {
        let actor = s.actor(&headers, false).await?;
        s.card(&actor, &publication).await
    }
    .await;
    respond(r)
}
async fn send(
    State(s): State<Server>,
    Path(publication): Path<String>,
    request: Request,
) -> Response {
    let r = async {
        let actor = s.actor(request.headers(), true).await?;
        let bytes = to_bytes(request.into_body(), 512 * 1024)
            .await
            .map_err(|_| err(413, "REQUEST_TOO_LARGE"))?;
        let request = serde_json::from_slice(&bytes).map_err(|_| err(400, "INVALID_ARGUMENT"))?;
        s.send(&actor, &publication, request).await
    }
    .await;
    respond(r)
}
async fn task(
    State(s): State<Server>,
    Path((publication, task)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (parts, _) = request.into_parts();
    let r = async move {
        let actor = s.actor(&parts.headers, false).await?;
        if let Some(id) = task.strip_suffix(":cancel") {
            if parts.method != axum::http::Method::POST {
                return Err(err(405, "METHOD_NOT_ALLOWED"));
            }
            return s.cancel(&actor, &publication, id).await;
        }
        if parts.method != axum::http::Method::GET {
            return Err(err(405, "METHOD_NOT_ALLOWED"));
        }
        s.task(&actor, &publication, &task).await
    }
    .await;
    respond(r)
}
fn respond(result: Result<Value, AgentError>) -> Response {
    match result {
        Ok(v) => ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response(),
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                [(header::CACHE_CONTROL,"no-store")],
                Json(json!({"error":{"code":e.status,"message":e.detail.as_str().or_else(||e.detail["code"].as_str()).unwrap_or("Agent request failed"),"details":[e.detail]}})),
            )
                .into_response()
        }
    }
}
async fn outbound(State(s): State<Server>, Path(name): Path<String>, request: Request) -> Response {
    outbound::send(s, name, request).await
}
async fn outbound_task(
    State(s): State<Server>,
    Path((name, message)): Path<(String, String)>,
    request: Request,
) -> Response {
    outbound::task(s, name, message, request).await
}
fn external_message_id(actor: &Actor, peer: &str, original: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!(
            "a2a:{}\0{}\0{peer}\0{original}",
            actor.tenant_id, actor.user_id
        )
        .as_bytes(),
    )
    .to_string()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_one_rejects_legacy_parts_and_unknown_effect_controls() {
        assert!(serde_json::from_value::<SendRequest>(json!({"message":{"messageId":uuid::Uuid::new_v4().to_string(),"role":"user","parts":[{"kind":"text","text":"x"}]}})).is_err());
        let r:SendRequest=serde_json::from_value(json!({"message":{"messageId":uuid::Uuid::new_v4().to_string(),"role":"ROLE_USER","parts":[{"text":"hello"}]},"configuration":{"returnImmediately":true}})).unwrap();
        assert_eq!(validate_message(&r).unwrap(), "hello");
    }
    #[test]
    fn managed_turn_states_map_to_v1_and_unknown_is_never_success() {
        for (status, expected) in [
            ("running", "TASK_STATE_WORKING"),
            ("succeeded", "TASK_STATE_COMPLETED"),
            ("completed", "TASK_STATE_COMPLETED"),
            ("cancelled", "TASK_STATE_CANCELED"),
            ("failed", "TASK_STATE_FAILED"),
            ("waiting_approval", "TASK_STATE_INPUT_REQUIRED"),
            ("unknown", "TASK_STATE_UNSPECIFIED"),
            ("provider_timeout", "TASK_STATE_UNSPECIFIED"),
        ] {
            assert_eq!(task_state(status), expected);
        }
    }
    #[test]
    fn immutable_card_signature_covers_entire_payload() {
        use ed25519_dalek::{Signature, Verifier};
        let key = SigningKey::from_bytes(&[19; 32]);
        let card = sign_card(
            json!({"name":"测试","version":"immutable","capabilities":{}}),
            "k1",
            &key,
        )
        .unwrap();
        let sig = &card["signatures"][0];
        let mut payload = card.clone();
        payload.as_object_mut().unwrap().remove("signatures");
        let input = format!(
            "{}.{}",
            sig["protected"].as_str().unwrap(),
            B64.encode(card_bytes(&payload).unwrap())
        );
        let signature =
            Signature::from_slice(&B64.decode(sig["signature"].as_str().unwrap()).unwrap())
                .unwrap();
        assert!(
            key.verifying_key()
                .verify(input.as_bytes(), &signature)
                .is_ok()
        );
        payload["version"] = json!("tampered");
        assert!(
            key.verifying_key()
                .verify(
                    format!(
                        "{}.{}",
                        sig["protected"].as_str().unwrap(),
                        B64.encode(card_bytes(&payload).unwrap())
                    )
                    .as_bytes(),
                    &signature
                )
                .is_err()
        );
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[tokio::test]
    async fn protocol_errors_keep_machine_code_and_unknown_task_identity() {
        let response = respond(Err(AgentError {
            status: 504,
            detail: json!({"code":"CANCELLATION_UNCONFIRMED","taskId":"task"}),
        }));
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["message"], "CANCELLATION_UNCONFIRMED");
        assert_eq!(value["error"]["details"][0]["taskId"], "task");
    }
    #[test]
    fn card_canonicalization_is_independent_of_json_property_insertion_order() {
        let first = json!({"z":"测试","a":{"last":true,"first":false}});
        let second = json!({"a":{"first":false,"last":true},"z":"测试"});
        assert_eq!(card_bytes(&first).unwrap(), card_bytes(&second).unwrap());
    }
}

#[cfg(test)]
mod outbound_identity_tests {
    use super::*;
    #[test]
    fn same_external_credential_cannot_collapse_distinct_callers_message_ids() {
        let actor = Actor {
            tenant_id: "tenant".into(),
            user_id: "first".into(),
            roles: vec![],
            tier: "normal".into(),
        };
        let id = external_message_id(&actor, "peer", "message");
        assert_eq!(id, external_message_id(&actor, "peer", "message"));
        let mut other = actor.clone();
        other.user_id = "second".into();
        assert_ne!(id, external_message_id(&other, "peer", "message"));
        other = actor.clone();
        other.tenant_id = "other".into();
        assert_ne!(id, external_message_id(&other, "peer", "message"));
        assert_ne!(id, external_message_id(&actor, "another-peer", "message"));
    }
}

#[cfg(test)]
mod protocol_fixtures {
    use super::*;
    #[test]
    fn checked_in_v1_wire_and_managed_state_fixtures_follow_the_supported_profile() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocols/a2a-1.0-profile.json"
        ))
        .unwrap();
        let specification = include_str!("../../../tests/fixtures/protocols/a2a-v1.0.0.proto");
        for state in fixture["states"].as_array().unwrap() {
            assert!(specification.contains(&format!("{} =", state["a2a"].as_str().unwrap())));
            assert_eq!(
                task_state(state["managed"].as_str().unwrap()),
                state["a2a"].as_str().unwrap()
            );
        }
        for value in fixture["accepted"].as_array().unwrap() {
            let request: SendRequest = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(validate_message(&request).unwrap(), "first\nsecond");
        }
        for value in fixture["unsupported"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<SendRequest>(value.clone())
                    .map(|r| validate_message(&r).is_err())
                    .unwrap_or(true)
            );
        }
    }
}

#[path = "a2a_tools.rs"]
mod tools;

#[path = "a2a_outbound.rs"]
mod outbound;
