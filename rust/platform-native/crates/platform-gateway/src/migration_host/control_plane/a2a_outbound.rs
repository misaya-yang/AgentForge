//! Pinned external Agent Cards and caller-bound, one-shot A2A task effects.
//! GET reconciliation never transmits a second SendMessage or cancellation.
use super::*;
use ed25519_dalek::{Signature, VerifyingKey};
use std::time::{SystemTime, UNIX_EPOCH};
const RECONCILE: &str = "https://agentforge.dev/a2a/extensions/task-by-message/v1";
const AUTHORITY: &str = "https://agentforge.dev/a2a/extensions/task-authority/v1";
fn base(config: &OutboundAgent) -> Result<url::Url, AgentError> {
    let mut u =
        url::Url::parse(&config.endpoint).map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))?;
    let path = u
        .path()
        .strip_suffix("/message:send")
        .ok_or_else(|| err(503, "EXTERNAL_AGENT_CONFIGURATION"))?
        .to_owned();
    u.set_path(&format!("{path}/"));
    Ok(u)
}
fn client(config: &OutboundAgent) -> Result<reqwest::Client, AgentError> {
    let u = base(config)?;
    let host = u
        .host_str()
        .ok_or_else(|| err(503, "EXTERNAL_AGENT_CONFIGURATION"))?;
    reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve(
            host,
            std::net::SocketAddr::new(config.address, u.port_or_known_default().unwrap_or(443)),
        )
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(config.timeout_seconds))
        .build()
        .map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))
}
fn now() -> Result<u64, AgentError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| err(503, "EXTERNAL_AGENT_CLOCK_UNAVAILABLE"))
}
#[derive(Serialize)]
struct TaskAuthority<'a> {
    v: &'static str,
    iss: String,
    aud: String,
    tenant_id: &'a str,
    user_id: &'a str,
    agent_name: &'a str,
    message_id: &'a str,
    body_sha256: String,
    method: &'a str,
    path: &'a str,
    iat: u64,
    exp: u64,
    jti: &'a str,
}
fn validate_actor(actor: &Actor) -> Result<(), AgentError> {
    if [&actor.tenant_id, &actor.user_id]
        .iter()
        .any(|v| v.is_empty() || v.len() > 255 || v.bytes().any(|b| b <= 32 || b == 127))
    {
        return Err(err(403, "EXTERNAL_AGENT_AUTHORITY_INVALID"));
    }
    Ok(())
}
fn proof(
    key: &SigningKey,
    kid: &str,
    issuer: &str,
    config: &OutboundAgent,
    actor: &Actor,
    message: &str,
    method: &str,
    url: &url::Url,
    body: &[u8],
    at: u64,
    jti: &str,
) -> Result<String, AgentError> {
    validate_actor(actor)?;
    if uuid::Uuid::parse_str(message).is_err() || uuid::Uuid::parse_str(jti).is_err() {
        return Err(err(403, "EXTERNAL_AGENT_AUTHORITY_INVALID"));
    }
    if url.query().is_some() || url.fragment().is_some() || url.origin() != base(config)?.origin() {
        return Err(err(503, "EXTERNAL_AGENT_CONFIGURATION"));
    }
    let issuer = url::Url::parse(issuer)
        .map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))?
        .origin()
        .ascii_serialization();
    let claims = TaskAuthority {
        v: "agentforge-a2a-task/v1",
        iss: issuer,
        aud: url.origin().ascii_serialization(),
        tenant_id: &actor.tenant_id,
        user_id: &actor.user_id,
        agent_name: &config.name,
        message_id: message,
        body_sha256: format!("{:x}", Sha256::digest(body)),
        method,
        path: url.path(),
        iat: at,
        exp: at.saturating_add(config.timeout_seconds.saturating_add(5).min(120)),
        jti,
    };
    let header = B64.encode(card_bytes(
        &json!({"alg":"EdDSA","typ":"agentforge-a2a-task+jwt","kid":kid}),
    )?);
    let payload = B64.encode(card_bytes(
        &serde_json::to_value(claims).map_err(|_| err(503, "EXTERNAL_AGENT_AUTHORITY_INVALID"))?,
    )?);
    let input = format!("{header}.{payload}");
    Ok(format!(
        "{input}.{}",
        B64.encode(key.sign(input.as_bytes()).to_bytes())
    ))
}
async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Value, AgentError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| err(502, "EXTERNAL_AGENT_EFFECT_UNKNOWN"))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(err(502, "EXTERNAL_AGENT_EFFECT_UNKNOWN"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| err(502, "EXTERNAL_AGENT_EFFECT_UNKNOWN"))
}
struct Card {
    sha256: String,
    reconcile: bool,
}
fn verify_card(config: &OutboundAgent, card: &Value) -> Result<Card, AgentError> {
    let raw = B64
        .decode(&config.agent_card_public_key)
        .map_err(|_| err(503, "EXTERNAL_AGENT_CARD_PIN_INVALID"))?;
    let raw: [u8; 32] = raw
        .try_into()
        .map_err(|_| err(503, "EXTERNAL_AGENT_CARD_PIN_INVALID"))?;
    let key =
        VerifyingKey::from_bytes(&raw).map_err(|_| err(503, "EXTERNAL_AGENT_CARD_PIN_INVALID"))?;
    let mut payload = card.clone();
    let signatures = payload
        .as_object_mut()
        .and_then(|v| v.remove("signatures"))
        .and_then(|v| v.as_array().cloned())
        .filter(|v| !v.is_empty() && v.len() <= 16)
        .ok_or_else(|| err(502, "EXTERNAL_AGENT_CARD_SIGNATURE_INVALID"))?;
    let canonical = card_bytes(&payload)?;
    let verified = signatures.iter().any(|signature| {
        let Some(protected) = signature["protected"].as_str().filter(|v| v.len() <= 8192) else {
            return false;
        };
        let Ok(bytes) = B64.decode(protected) else {
            return false;
        };
        let Ok(header) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        if header["alg"] != "EdDSA"
            || header["kid"] != config.agent_card_key_id
            || header.get("crit").is_some()
            || header["b64"] == false
        {
            return false;
        }
        let Some(signature) = signature["signature"].as_str().filter(|v| v.len() == 86) else {
            return false;
        };
        let Ok(bytes) = B64.decode(signature) else {
            return false;
        };
        let Ok(signature) = Signature::from_slice(&bytes) else {
            return false;
        };
        key.verify_strict(
            format!("{protected}.{}", B64.encode(&canonical)).as_bytes(),
            &signature,
        )
        .is_ok()
    });
    if !verified {
        return Err(err(502, "EXTERNAL_AGENT_CARD_SIGNATURE_INVALID"));
    }
    let base = base(config)?.as_str().trim_end_matches('/').to_owned();
    if payload["name"] != config.name
        || !payload["description"].is_string()
        || !payload["capabilities"].is_object()
        || !payload["skills"].as_array().is_some_and(|skills| {
            !skills.is_empty()
                && skills.iter().all(|s| {
                    s["id"].as_str().is_some_and(|s| !s.is_empty())
                        && s["name"].is_string()
                        && s["description"].is_string()
                        && s["tags"].is_array()
                })
        })
        || !payload["defaultOutputModes"]
            .as_array()
            .is_some_and(|modes| modes.iter().any(|m| m == "text/plain"))
        || !payload["supportedInterfaces"].as_array().is_some_and(|v| {
            v.iter().any(|v| {
                v["url"].as_str().map(|v| v.trim_end_matches('/')) == Some(base.as_str())
                    && v["protocolBinding"] == "HTTP+JSON"
                    && v["protocolVersion"] == "1.0"
            })
        })
        || !payload["defaultInputModes"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == "text/plain"))
        || payload["version"].as_str().is_none_or(str::is_empty)
    {
        return Err(err(502, "EXTERNAL_AGENT_CARD_INTERFACE_INVALID"));
    }
    let extensions = payload["capabilities"]["extensions"].as_array();
    if extensions.is_some_and(|v| {
        v.iter()
            .any(|v| v["required"] == true && v["uri"] != RECONCILE && v["uri"] != AUTHORITY)
    }) {
        return Err(err(502, "EXTERNAL_AGENT_CARD_EXTENSION_UNSUPPORTED"));
    }
    if !extensions.is_some_and(|v| v.iter().any(|v| v["uri"] == AUTHORITY)) {
        return Err(err(502, "EXTERNAL_AGENT_AUTHORITY_UNSUPPORTED"));
    }
    Ok(Card {
        sha256: format!("{:x}", Sha256::digest(canonical)),
        reconcile: extensions.is_some_and(|v| v.iter().any(|v| v["uri"] == RECONCILE)),
    })
}
async fn card(config: &OutboundAgent, http: &reqwest::Client) -> Result<Card, AgentError> {
    let u = base(config)?
        .join(".well-known/agent-card.json")
        .map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))?;
    let response = http
        .get(u)
        .bearer_auth(&config.bearer_token)
        .header("A2A-Version", "1.0")
        .send()
        .await
        .map_err(|_| err(502, "EXTERNAL_AGENT_CARD_UNAVAILABLE"))?;
    if !response.status().is_success() {
        return Err(err(502, "EXTERNAL_AGENT_CARD_UNAVAILABLE"));
    }
    verify_card(
        config,
        &bounded(response, config.max_response_bytes.min(512 * 1024)).await?,
    )
}
#[allow(clippy::too_many_arguments)]
fn prepare(
    config: &OutboundAgent,
    http: &reqwest::Client,
    key: &SigningKey,
    kid: &str,
    issuer: &str,
    actor: &Actor,
    message: &str,
    method: reqwest::Method,
    url: url::Url,
    body: Option<Value>,
    at: u64,
    jti: &str,
) -> Result<reqwest::RequestBuilder, AgentError> {
    let bytes = body
        .as_ref()
        .map(card_bytes)
        .transpose()?
        .unwrap_or_default();
    if bytes.len() > config.max_request_bytes {
        return Err(err(413, "REQUEST_TOO_LARGE"));
    }
    let authority = proof(
        key,
        kid,
        issuer,
        config,
        actor,
        message,
        method.as_str(),
        &url,
        &bytes,
        at,
        jti,
    )?;
    let mut request = http
        .request(method, url)
        .bearer_auth(&config.bearer_token)
        .header("A2A-Version", "1.0")
        .header("x-agentforge-a2a-task", authority);
    if body.is_some() {
        request = request
            .header("content-type", "application/json")
            .body(bytes);
    }
    Ok(request)
}
async fn request(
    server: &Server,
    config: &OutboundAgent,
    http: &reqwest::Client,
    actor: &Actor,
    message: &str,
    method: reqwest::Method,
    url: url::Url,
    body: Option<Value>,
) -> Result<Value, AgentError> {
    let response = prepare(
        config,
        http,
        &server.signing,
        &server.configuration.signing_key_id,
        &server.configuration.origin,
        actor,
        message,
        method,
        url,
        body,
        now()?,
        &uuid::Uuid::new_v4().to_string(),
    )?
    .send()
    .await
    .map_err(|_| err(502, "EXTERNAL_AGENT_EFFECT_UNKNOWN"))?;
    if !response.status().is_success() {
        return Err(err(502, "EXTERNAL_AGENT_EFFECT_UNKNOWN"));
    }
    bounded(response, config.max_response_bytes).await
}
fn task_id(value: &Value) -> Result<Option<String>, AgentError> {
    if value["task"].is_object() {
        let id = value["task"]["id"]
            .as_str()
            .filter(|v| uuid::Uuid::parse_str(v).is_ok())
            .ok_or_else(|| err(502, "EXTERNAL_AGENT_TASK_INVALID"))?;
        let state = value["task"]["status"]["state"]
            .as_str()
            .ok_or_else(|| err(502, "EXTERNAL_AGENT_TASK_INVALID"))?;
        if ![
            "TASK_STATE_UNSPECIFIED",
            "TASK_STATE_SUBMITTED",
            "TASK_STATE_WORKING",
            "TASK_STATE_COMPLETED",
            "TASK_STATE_FAILED",
            "TASK_STATE_CANCELED",
            "TASK_STATE_INPUT_REQUIRED",
            "TASK_STATE_REJECTED",
            "TASK_STATE_AUTH_REQUIRED",
        ]
        .contains(&state)
        {
            return Err(err(502, "EXTERNAL_AGENT_TASK_INVALID"));
        }
        Ok(Some(id.into()))
    } else if value["message"].is_object()
        && value["message"]["role"] == "ROLE_AGENT"
        && value["message"]["messageId"]
            .as_str()
            .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
        && value["message"]["parts"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
    {
        Ok(None)
    } else {
        Err(err(502, "EXTERNAL_AGENT_TASK_INVALID"))
    }
}
fn reconcile_url(config: &OutboundAgent, message: &str) -> Result<url::Url, AgentError> {
    uuid::Uuid::parse_str(message).map_err(|_| err(502, "EXTERNAL_AGENT_TASK_INVALID"))?;
    base(config)?
        .join(&format!("./tasks:byMessage/{message}"))
        .map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))
}
fn operation_url(config: &OutboundAgent, id: &str, cancel: bool) -> Result<url::Url, AgentError> {
    uuid::Uuid::parse_str(id).map_err(|_| err(502, "EXTERNAL_AGENT_TASK_INVALID"))?;
    base(config)?
        .join(&format!(
            "tasks/{id}{}",
            if cancel { ":cancel" } else { "" }
        ))
        .map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))
}
fn terminal(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_CANCELED"
            | "TASK_STATE_COMPLETED"
            | "TASK_STATE_FAILED"
            | "TASK_STATE_REJECTED"
    )
}
fn monotonic_response(current: &Value, incoming: &Value) -> Value {
    if terminal(text(&current["task"]["status"], "state")) {
        current.clone()
    } else {
        incoming.clone()
    }
}
async fn receipt(
    server: &Server,
    actor: &Actor,
    name: &str,
    message: &str,
    value: &Value,
    expected: Option<&str>,
    card: &Card,
) -> Result<Value, AgentError> {
    let id = task_id(value)?;
    if expected.is_some() && id.as_deref() != expected {
        return Err(err(409, "EXTERNAL_AGENT_TASK_ID_CONFLICT"));
    }
    let mut tx = server.agent.repository.pool.begin().await?;
    let current:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM gateway.gateway_a2a_outbound_effects e WHERE tenant_id=$1 AND user_id=$2 AND agent_name=$3 AND message_id=$4::uuid FOR UPDATE").bind(&actor.tenant_id).bind(&actor.user_id).bind(name).bind(message).fetch_one(&mut *tx).await?;
    if current["card_sha256"] != card.sha256
        || current["remote_task_id"]
            .as_str()
            .is_some_and(|known| Some(known) != id.as_deref())
    {
        return Err(err(409, "EXTERNAL_AGENT_TASK_ID_CONFLICT"));
    }
    let value = monotonic_response(&current["response"], value);
    let state = text(&value["task"]["status"], "state");
    let cancel = if state == "TASK_STATE_CANCELED" {
        "confirmed"
    } else if terminal(state) {
        "rejected"
    } else {
        "unknown"
    };
    sqlx::query("UPDATE gateway.gateway_a2a_outbound_effects SET effect_state='confirmed',response=$5,remote_task_id=COALESCE(remote_task_id,$6),cancel_state=CASE WHEN cancel_state='unknown' THEN $7 ELSE cancel_state END,updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND agent_name=$3 AND message_id=$4::uuid").bind(&actor.tenant_id).bind(&actor.user_id).bind(name).bind(message).bind(&value).bind(id.as_deref()).bind(cancel).execute(&mut *tx).await?;
    let audit = ai_platform_gateway_foundation::external_a2a_ledger::Receipt {
        kind: ai_platform_gateway_foundation::external_a2a_ledger::Kind::A2a,
        request_id: message.into(),
        client_request_id: None,
        request_sha256: None,
        wire_response_id: None,
        tool: Some(name.into()),
        local_message_id: Some(message.into()),
        remote_task_id: id.clone(),
        outcome: match state {
            "TASK_STATE_COMPLETED" => "completed",
            "TASK_STATE_CANCELED" => "canceled",
            "TASK_STATE_FAILED" | "TASK_STATE_REJECTED" => "denied",
            _ => "unknown",
        }
        .into(),
        http_status: 200,
    };
    ai_platform_gateway_foundation::external_a2a_ledger::record_in(
        &mut tx,
        &actor.tenant_id,
        &actor.user_id,
        &audit,
    )
    .await
    .map_err(|_| err(503, "EXTERNAL_AGENT_AUDIT_UNCONFIRMED"))?;
    tx.commit().await?;
    Ok(value)
}
pub(super) async fn send(server: Server, name: String, request_in: Request) -> Response {
    respond(send_inner(&server, &name, request_in).await)
}
async fn send_inner(server: &Server, name: &str, request_in: Request) -> Result<Value, AgentError> {
    let actor = server.actor(request_in.headers(), true).await?;
    validate_actor(&actor)?;
    let config = server
        .configuration
        .outbound
        .iter()
        .find(|v| v.name == name)
        .ok_or_else(|| err(404, "EXTERNAL_AGENT_NOT_FOUND"))?;
    tools::authorize(server, &actor, name).await?;
    let raw = to_bytes(request_in.into_body(), config.max_request_bytes)
        .await
        .map_err(|_| err(413, "REQUEST_TOO_LARGE"))?;
    let body: SendRequest =
        serde_json::from_slice(&raw).map_err(|_| err(400, "INVALID_ARGUMENT"))?;
    validate_message(&body)?;
    if body.message.context_id.is_some() {
        return Err(err(400, "OUTBOUND_CONTEXT_UNSUPPORTED"));
    }
    let value = serde_json::to_value(&body).map_err(|_| err(400, "INVALID_ARGUMENT"))?;
    let hash = format!("{:x}", Sha256::digest(card_bytes(&value)?));
    let prior:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(e) FROM gateway.gateway_a2a_outbound_effects e WHERE tenant_id=$1 AND user_id=$2 AND agent_name=$3 AND message_id=$4::uuid").bind(&actor.tenant_id).bind(&actor.user_id).bind(name).bind(&body.message.message_id).fetch_optional(&server.agent.repository.pool).await?;
    if let Some(prior) = prior {
        if prior["request_hash"] != hash {
            return Err(err(409, "MESSAGE_ID_CONFLICT"));
        }
        if prior["effect_state"] == "confirmed" {
            return Ok(prior["response"].clone());
        }
        return Err(err(409, "EXTERNAL_AGENT_EFFECT_UNKNOWN"));
    }
    let http = client(config)?;
    let card = card(config, &http).await?;
    if !card.reconcile {
        return Err(err(502, "EXTERNAL_AGENT_RECONCILIATION_UNSUPPORTED"));
    }
    let remote = external_message_id(&actor, name, &body.message.message_id);
    let mut transmitted = value;
    transmitted["message"]["messageId"] = json!(remote);
    if card_bytes(&transmitted)?.len() > config.max_request_bytes {
        return Err(err(413, "REQUEST_TOO_LARGE"));
    }
    let mut tx = server.agent.repository.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("a2a:{}:{name}", actor.tenant_id))
        .execute(&mut *tx)
        .await?;
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM gateway.gateway_a2a_outbound_effects WHERE tenant_id=$1 AND agent_name=$2 AND created_at>=date_trunc('day',NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'").bind(&actor.tenant_id).bind(name).fetch_one(&mut *tx).await?;
    if count >= config.max_calls_per_day as i64 {
        let audit = ai_platform_gateway_foundation::external_a2a_ledger::Receipt {
            kind: ai_platform_gateway_foundation::external_a2a_ledger::Kind::A2a,
            request_id: body.message.message_id.clone(),
            client_request_id: None,
            request_sha256: None,
            wire_response_id: None,
            tool: Some(name.into()),
            local_message_id: Some(body.message.message_id.clone()),
            remote_task_id: None,
            outcome: "denied".into(),
            http_status: 429,
        };
        ai_platform_gateway_foundation::external_a2a_ledger::record_in(
            &mut tx,
            &actor.tenant_id,
            &actor.user_id,
            &audit,
        )
        .await
        .map_err(|_| err(503, "EXTERNAL_AGENT_AUDIT_UNCONFIRMED"))?;
        tx.commit().await?;
        return Err(err(429, "EXTERNAL_AGENT_BUDGET_EXHAUSTED"));
    }
    let inserted=sqlx::query("INSERT INTO gateway.gateway_a2a_outbound_effects(tenant_id,user_id,agent_name,message_id,request_hash,effect_state,remote_message_id,card_sha256,card_key_id) VALUES($1,$2,$3,$4::uuid,$5,'unknown',$6::uuid,$7,$8) ON CONFLICT(tenant_id,user_id,agent_name,message_id) DO NOTHING").bind(&actor.tenant_id).bind(&actor.user_id).bind(name).bind(&body.message.message_id).bind(hash).bind(&remote).bind(&card.sha256).bind(&config.agent_card_key_id).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    if inserted != 1 {
        return Err(err(409, "EXTERNAL_AGENT_EFFECT_UNKNOWN"));
    }
    // The debit and unknown marker are committed before the only SendMessage.
    let response = request(
        server,
        config,
        &http,
        &actor,
        &remote,
        reqwest::Method::POST,
        config
            .endpoint
            .parse()
            .map_err(|_| err(503, "EXTERNAL_AGENT_CONFIGURATION"))?,
        Some(transmitted),
    )
    .await?;
    receipt(
        server,
        &actor,
        name,
        &body.message.message_id,
        &response,
        None,
        &card,
    )
    .await
}
#[derive(PartialEq, Eq, Debug)]
enum CancelAdmission {
    Transmit,
    Confirmed,
}
fn cancel_admission(row: &Value) -> Result<CancelAdmission, AgentError> {
    let state = text(&row["response"]["task"]["status"], "state");
    if row["cancel_state"] == "confirmed" && state == "TASK_STATE_CANCELED" {
        return Ok(CancelAdmission::Confirmed);
    }
    if terminal(state) || row["cancel_state"] == "rejected" {
        return Err(err(409, "TASK_NOT_CANCELABLE"));
    }
    if row["cancel_state"] != "none" {
        return Err(err(409, "EXTERNAL_AGENT_CANCELLATION_UNKNOWN"));
    }
    Ok(CancelAdmission::Transmit)
}
pub(super) async fn task(
    server: Server,
    name: String,
    local: String,
    request_in: Request,
) -> Response {
    respond(task_inner(&server, &name, &local, request_in).await)
}
async fn task_inner(
    server: &Server,
    name: &str,
    local: &str,
    request_in: Request,
) -> Result<Value, AgentError> {
    let actor = server.actor(request_in.headers(), false).await?;
    validate_actor(&actor)?;
    let config = server
        .configuration
        .outbound
        .iter()
        .find(|v| v.name == name)
        .ok_or_else(|| err(404, "EXTERNAL_AGENT_NOT_FOUND"))?;
    tools::authorize(server, &actor, name).await?;
    let (local, cancel, reconcile) = if let Some(id) = local.strip_suffix(":cancel") {
        (id, true, false)
    } else if let Some(id) = local.strip_suffix(":reconcile") {
        (id, false, true)
    } else {
        (local, false, false)
    };
    uuid::Uuid::parse_str(local).map_err(|_| err(404, "TASK_NOT_FOUND"))?;
    if (cancel || reconcile) && request_in.method() != axum::http::Method::POST
        || !cancel && !reconcile && request_in.method() != axum::http::Method::GET
    {
        return Err(err(405, "METHOD_NOT_ALLOWED"));
    }
    let bytes = to_bytes(request_in.into_body(), 4096)
        .await
        .map_err(|_| err(413, "REQUEST_TOO_LARGE"))?;
    if !bytes.is_empty() && serde_json::from_slice::<Value>(&bytes).ok() != Some(json!({})) {
        return Err(err(400, "INVALID_ARGUMENT"));
    }
    let row:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM gateway.gateway_a2a_outbound_effects e WHERE tenant_id=$1 AND user_id=$2 AND agent_name=$3 AND message_id=$4::uuid").bind(&actor.tenant_id).bind(&actor.user_id).bind(name).bind(local).fetch_optional(&server.agent.repository.pool).await?.ok_or_else(||err(404,"TASK_NOT_FOUND"))?;
    let remote = external_message_id(&actor, name, local);
    if row["remote_message_id"] != remote {
        return Err(err(409, "EXTERNAL_AGENT_AUTHORITY_UNAVAILABLE"));
    }
    let http = client(config)?;
    let card = card(config, &http).await?;
    if row["card_sha256"] != card.sha256 || row["card_key_id"] != config.agent_card_key_id {
        return Err(err(409, "EXTERNAL_AGENT_CARD_CHANGED"));
    }
    if row["effect_state"] == "unknown" {
        if cancel {
            return Err(err(409, "EXTERNAL_AGENT_EFFECT_UNKNOWN"));
        }
        if !card.reconcile {
            return Err(err(409, "EXTERNAL_AGENT_RECONCILIATION_UNSUPPORTED"));
        }
        let url = reconcile_url(config, &remote)?;
        let response = request(
            server,
            config,
            &http,
            &actor,
            &remote,
            reqwest::Method::GET,
            url,
            None,
        )
        .await?;
        let response = if response["task"].is_object() || response["message"].is_object() {
            response
        } else {
            json!({"task":response})
        };
        let response = receipt(server, &actor, name, local, &response, None, &card).await?;
        return response
            .get("task")
            .cloned()
            .ok_or_else(|| err(409, "EXTERNAL_AGENT_RESPONSE_HAS_NO_TASK"));
    }
    let id = row["remote_task_id"]
        .as_str()
        .ok_or_else(|| err(409, "EXTERNAL_AGENT_RESPONSE_HAS_NO_TASK"))?;
    if cancel {
        if cancel_admission(&row)? == CancelAdmission::Confirmed {
            return Ok(row["response"]["task"].clone());
        }
        let claimed=sqlx::query("UPDATE gateway.gateway_a2a_outbound_effects SET cancel_state='unknown',updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND agent_name=$3 AND message_id=$4::uuid AND cancel_state='none' AND effect_state='confirmed' AND COALESCE(response->'task'->'status'->>'state','') NOT IN ('TASK_STATE_CANCELED','TASK_STATE_COMPLETED','TASK_STATE_FAILED','TASK_STATE_REJECTED')").bind(&actor.tenant_id).bind(&actor.user_id).bind(name).bind(local).execute(&server.agent.repository.pool).await?.rows_affected();
        if claimed != 1 {
            return Err(err(409, "EXTERNAL_AGENT_CANCELLATION_UNKNOWN"));
        }
    }
    let value = request(
        server,
        config,
        &http,
        &actor,
        &remote,
        if cancel {
            reqwest::Method::POST
        } else {
            reqwest::Method::GET
        },
        operation_url(config, id, cancel)?,
        cancel.then(|| json!({})),
    )
    .await?;
    let response = receipt(
        server,
        &actor,
        name,
        local,
        &json!({"task":value}),
        Some(id),
        &card,
    )
    .await?;
    if cancel && response["task"]["status"]["state"] != "TASK_STATE_CANCELED" {
        return Err(err(409, "EXTERNAL_AGENT_CANCELLATION_UNKNOWN"));
    }
    Ok(response["task"].clone())
}
#[cfg(test)]
#[path = "a2a_outbound/tests.rs"]
mod tests;
