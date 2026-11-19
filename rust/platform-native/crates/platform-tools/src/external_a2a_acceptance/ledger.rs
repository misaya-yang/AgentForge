//! Real read-only source export. Uses canonical existing Gateway role/mTLS pool,
//! a verified row owner, local tenant binding and one repeatable-read snapshot.
//! Signs only after every observed operation resolves against committed sources.
use super::*;
use ai_platform_service_primitives::data_connection::{
    ConnectionConfig, ConnectionPurpose, DatabaseRole, TenantScope, bind_transaction,
};
use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    schema: String,
    data_prefix: String,
    signing_key_file: PathBuf,
    admission_permit_file: PathBuf,
    admission_key_id: String,
    admission_public_key: String,
}
const WIRE: &str = "SELECT jsonb_build_object('auditId',id,'requestId',resource_id,'summary',response_summary) FROM gateway.audit_logs WHERE tenant_id=$1 AND user_id=$2 AND event_type='external_framework_wire_v1' AND (resource_id=$3 OR response_summary->>'client_request_id'=$3) ORDER BY id LIMIT 32";
const USAGE: &str = "SELECT jsonb_build_object('id',id,'tenantId',tenant_id,'userId',user_id,'requestId',request_id,'serviceId',service_id,'requestType',request_type,'model',model,'provider',provider,'inputTokens',input_tokens,'outputTokens',output_tokens,'inputCostCents',input_cost_cents,'outputCostCents',output_cost_cents,'status',status,'pricing',metadata->'pricing_snapshot','cache',(metadata->'provider_cache_usage')-'raw','cachePricingState',metadata->'cache_pricing_state') FROM gateway.usage_records WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 ORDER BY id LIMIT 8";
const PRICE: &str = "SELECT jsonb_build_object('tenantId',tenant_id,'requestId',request_id,'serviceId',service_id,'requestType',request_type,'snapshot',snapshot) FROM gateway.price_book_v2_usage_receipts WHERE tenant_id=$1 AND request_id=$3 AND EXISTS(SELECT 1 FROM gateway.usage_records u WHERE u.tenant_id=$1 AND u.user_id=$2 AND u.request_id=$3 AND u.service_id=price_book_v2_usage_receipts.service_id AND u.request_type=price_book_v2_usage_receipts.request_type) LIMIT 8";
const BUDGET: &str = "SELECT jsonb_build_object('member',member,'tenantId',tenant_id,'userId',user_id,'requestId',request_id,'serviceId',service_id,'requestType',request_type,'state',state,'receiptId',candidate_receipt_id,'observation',jsonb_build_object('tenant_id',settlement_observation->'tenant_id','user_id',settlement_observation->'user_id','request_id',settlement_observation->'request_id','input_tokens',settlement_observation->'input_tokens','output_tokens',settlement_observation->'output_tokens','status',settlement_observation->'status','pricing_snapshot',settlement_observation->'metadata'->'pricing_snapshot')) FROM gateway.dispatch_token_reservations WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 ORDER BY created_at LIMIT 8";
const CHECKPOINT: &str = "SELECT jsonb_build_object('tenantId',tenant_id,'requestId',request_id,'serviceId',service_id,'requestType',request_type,'stage',stage,'userId',observation->>'user_id','inputTokens',observation->'input_tokens','outputTokens',observation->'output_tokens','pricing',observation->'metadata'->'pricing_snapshot') FROM gateway.price_book_v2_accounting_checkpoints WHERE tenant_id=$1 AND request_id=$3 AND observation->>'user_id'=$2 AND stage='quota_counters' LIMIT 8";
const GUARD: &str = "SELECT jsonb_build_object('auditId',id,'requestId',resource_id,'decision',response_summary,'status',status) FROM gateway.audit_logs WHERE tenant_id=$1 AND user_id=$2 AND resource_id=$3 AND event_type='guardrail_decision' ORDER BY id LIMIT 16";
const EFFECT: &str = "SELECT jsonb_build_object('tenantId',tenant_id,'userId',user_id,'messageId',message_id,'remoteMessageId',remote_message_id,'remoteTaskId',remote_task_id,'effectState',effect_state,'cancelState',cancel_state,'cardSha256',card_sha256,'task',jsonb_build_object('id',response->'task'->'id','contextId',response->'task'->'contextId','status',response->'task'->'status','metadata',response->'task'->'metadata')) FROM gateway.gateway_a2a_outbound_effects WHERE tenant_id=$1 AND user_id=$2 AND message_id=$3::uuid AND agent_name=$4";
async fn rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    c: &Config,
    id: &str,
) -> Result<Vec<Value>> {
    sqlx::query_scalar(sql)
        .bind(&c.tenant_id)
        .bind(&c.user_id)
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| "ledger source absent, denied or unconfirmed; no privilege fallback".into())
}
fn one(rows: &[Value]) -> Result<&Value> {
    ensure(rows.len() == 1, "exactly one canonical source row required")?;
    Ok(&rows[0])
}
fn digest(v: &Value) -> Result<String> {
    Ok(hash(
        &serde_json::to_vec(v).map_err(|_| "source projection encoding")?,
    ))
}
fn matching<'a>(rows: &'a [Value], kind: &str, outcome: &str) -> Result<&'a Value> {
    rows.iter()
        .rev()
        .find(|r| r["summary"]["kind"] == kind && r["summary"]["outcome"] == outcome)
        .ok_or_else(|| "actual server wire receipt missing".into())
}
fn project_model(
    c: &Config,
    call: &Value,
    wire: &[Value],
    usage: &[Value],
    price: &[Value],
    budget: &[Value],
    checkpoint: &[Value],
    guard: &[Value],
) -> Result<Value> {
    let w = matching(wire, "model", "completed")?;
    let u = one(usage)?;
    let p = one(price)?;
    let b = one(budget)?;
    let k = one(checkpoint)?;
    ensure(
        w["summary"]["client_request_id"] == call["request_id"]
            && w["summary"]["request_sha256"] == call["request_hash"]
            && w["summary"]["wire_response_id"] == call["response_id"]
            && w["requestId"] == call["gateway_request_id"]
            && u["requestId"] == w["requestId"]
            && u["inputTokens"] == call["input_tokens"]
            && u["outputTokens"] == call["output_tokens"]
            && u["status"] == "success",
        "actual usage/wire identity mismatch",
    )?;
    ensure(
        u["pricing"]["price_book_schema"] == "agentforge.price_book/v2"
            && p["snapshot"] == u["pricing"]
            && k["pricing"] == u["pricing"]
            && k["inputTokens"] == u["inputTokens"]
            && k["outputTokens"] == u["outputTokens"]
            && b["state"] == "settled"
            && b["observation"]["pricing_snapshot"] == u["pricing"]
            && b["observation"]["input_tokens"] == u["inputTokens"]
            && b["observation"]["output_tokens"] == u["outputTokens"],
        "pricebook/checkpoint/budget unsettled or inconsistent",
    )?;
    for row in [p, b, k] {
        ensure(
            row["serviceId"] == u["serviceId"] && row["requestType"] == u["requestType"],
            "canonical service identity mismatch",
        )?;
    }
    let g = guard
        .iter()
        .find(|g| {
            g["decision"]["direction"] == "input"
                && g["decision"]["decision"] == "allow"
                && g["decision"]["policy_pin"].is_string()
        })
        .ok_or("actual enabled input guardrail decision required")?;
    let input = u["inputCostCents"]
        .as_u64()
        .ok_or("canonical input cost missing")?;
    let output = u["outputCostCents"]
        .as_u64()
        .ok_or("canonical output cost missing")?;
    let micros = input
        .checked_add(output)
        .and_then(|n| n.checked_mul(10000))
        .ok_or("ledger unit conversion overflow")?;
    let cache = match u["cache"]["read"]["state"].as_str() {
        Some("reported") => u["cache"]["read"]["tokens"]
            .as_u64()
            .map(Value::from)
            .ok_or("cache observation invalid")?,
        Some("not_reported") => Value::Null,
        _ => return Err("cache observation missing/invalid".into()),
    };
    ensure(
        cache.is_null()
            || cache
                .as_u64()
                .is_some_and(|n| n <= u["inputTokens"].as_u64().unwrap_or(0)),
        "cache count invalid",
    )?;
    let source = json!({"wire":w,"usage":u,"price":p,"budget":b,"checkpoint":k,"guardrails":guard});
    Ok(
        json!({"requestId":call["request_id"],"gatewayRequestId":w["requestId"],"responseId":call["response_id"],"receiptId":u["id"],"tenantId":c.tenant_id,"userId":c.user_id,"inputTokens":u["inputTokens"],"outputTokens":u["outputTokens"],"cachedInputTokens":cache,"cacheState":if cache.is_null(){"not_reported"}else{"reported"},"state":"settled","amountMicros":micros,"amountPrecision":"ledger_cents","priceBookVersion":digest(&p["snapshot"])? ,"budgetReservationId":b["member"],"guardrailDecisionId":g["auditId"].to_string(),"sourceRowsSha256":digest(&source)?,"source":source}),
    )
}
pub(super) fn command(c: &Config, observation_file: &Path, settings_file: &Path) -> Result<Value> {
    let settings: Settings = serde_json::from_slice(&read(settings_file, true)?)
        .map_err(|_| "ledger settings invalid")?;
    ensure(
        settings.schema == "agentforge-external-a2a-ledger-export/v1"
            && settings.data_prefix == "DATA_GATEWAY_EVIDENCE",
        "explicit Gateway evidence connection prefix required",
    )?;
    let raw = read(observation_file, true)?;
    let observed: Value = serde_json::from_slice(&raw).map_err(|_| "observation invalid")?;
    ensure(
        observed["tenantId"] == c.tenant_id
            && observed["userId"] == c.user_id
            && observed["peer"] == c.peer
            && observed["gatewayOrigin"] == c.gateway_origin,
        "observation owner mismatch",
    )?;
    let permit_raw = read(&settings.admission_permit_file, true)?;
    let permit = signed(
        std::str::from_utf8(&permit_raw).map_err(|_| "permit encoding")?,
        &settings.admission_key_id,
        &settings.admission_public_key,
        "agentforge-external-a2a-activation+jwt",
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock invalid")?
        .as_secs();
    let iat = permit["iat"].as_u64().ok_or("permit time")?;
    let exp = permit["exp"].as_u64().ok_or("permit expiry")?;
    ensure(
        iat <= now + 5
            && exp > now
            && exp > iat
            && exp - iat <= 3600
            && permit["gateway_origin"] == c.gateway_origin
            && permit["allow_model_calls"] == true,
        "admission permit expired or mismatched",
    )?;
    ensure(
        observed["task"]["metadata"]["admissionPermitSha256"] == digest(&permit)?,
        "actual task is not bound to exact signed admission claims",
    )?;
    let rehearsal = permit["admission"] == "rehearsal";
    ensure(
        observed["task"]["metadata"]["admission"]
            == if rehearsal { "rehearsal" } else { "production" },
        "rehearsal transcript cannot be promoted by a production permit",
    )?;
    if rehearsal {
        ensure(
            exp - iat <= 900
                && permit["u_gw15"]["status"] == "not_run"
                && permit["rehearsal"]["tenant_id"] == c.tenant_id
                && permit["rehearsal"]["user_id"] == c.user_id
                && permit["rehearsal"]["profile_sha256"]
                    == observed["task"]["metadata"]["profileSha256"]
                && private_origin(&c.gateway_origin)
                && private_origin(permit["agent_origin"].as_str().unwrap_or("")),
            "rehearsal cannot grant production or foreign tenant/profile/origin access",
        )?;
    } else {
        ensure(
            permit["u_gw15"]["status"] == "direct_verified"
                && permit["u_gw15"]["evidence_sha256"]
                    .as_str()
                    .is_some_and(hex_sha),
            "production admission gate closed",
        )?;
    }
    let key = SigningKey::from_pkcs8_pem(
        std::str::from_utf8(&read(&settings.signing_key_file, true)?)
            .map_err(|_| "signer encoding")?,
    )
    .map_err(|_| "signer invalid")?;
    ensure(
        B64.encode(key.verifying_key().as_bytes()) == c.evidence_public_key,
        "source signer does not match explicit evidence pin",
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "export runtime unavailable")?;
    let (models, capabilities, negatives, sources) =
        runtime.block_on(extract(c, &settings, &observed))?;
    let evidence = json!({"schema":"agentforge-external-a2a-ledger-evidence/v1","admission":if rehearsal{"rehearsal"}else{"production"},"observationSha256":hash(&raw),"gatewayOrigin":c.gateway_origin,"tenantId":c.tenant_id,"userId":c.user_id,"peer":c.peer,"messageId":observed["messageId"],"iat":now,"exp":now+600,"uGw15":{"status":if rehearsal{"not_run"}else{"direct_verified"},"evidenceSha256":if rehearsal{hash(&permit_raw)}else{permit["u_gw15"]["evidence_sha256"].as_str().unwrap_or("").into()}},"modelReceipts":models,"capabilityReceipts":capabilities,"negativeCases":negatives,"sourceSnapshot":sources});
    let header=B64.encode(serde_json::to_vec(&json!({"alg":"EdDSA","typ":"agentforge-external-a2a-evidence+jwt","kid":c.evidence_key_id})).map_err(|_|"header encoding")?);
    let input = format!(
        "{}.{}",
        header,
        B64.encode(serde_json::to_vec(&evidence).map_err(|_| "evidence encoding")?)
    );
    let compact = format!(
        "{}.{}",
        input,
        B64.encode(key.sign(input.as_bytes()).to_bytes())
    );
    // Validate exactly what is signed before producing a source-bound receipt.
    let result = verify_evidence(c, &raw, &evidence, now)?;
    write_new(&c.output_directory, "ledger-source.json", &evidence)?;
    let mut opts = fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut out = opts
        .open(c.output_directory.join("ledger-evidence.jwt"))
        .map_err(|_| "evidence output exists")?;
    out.write_all(compact.as_bytes())
        .and_then(|_| out.sync_all())
        .map_err(|_| "evidence write unconfirmed")?;
    Ok(
        json!({"status":"source_exported","admission":evidence["admission"],"validation":result,"evidence":c.output_directory.join("ledger-evidence.jwt")}),
    )
}
fn private_origin(value: &str) -> bool {
    let Ok(u) = origin(value) else {
        return false;
    };
    let Some(host) = u.host_str() else {
        return false;
    };
    let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(x) => x.is_private() || x.is_loopback(),
        IpAddr::V6(x) => x.is_loopback() || x.is_unique_local(),
    }
}
async fn extract(
    c: &Config,
    s: &Settings,
    o: &Value,
) -> Result<(Vec<Value>, Vec<Value>, Value, Value)> {
    let env = std::env::vars().collect();
    let cfg = ConnectionConfig::from_environment(
        &s.data_prefix,
        DatabaseRole::Gateway,
        ConnectionPurpose::TenantTransactions,
        &env,
    )
    .map_err(|_| "role/file/mTLS configuration rejected")?;
    ensure(
        cfg.max_connections <= 2,
        "evidence pool must be <=2 connections",
    )?;
    let pool = cfg
        .connect()
        .await
        .map_err(|_| "verified Gateway evidence pool unavailable")?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| "read-only snapshot begin failed")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|_| "read-only snapshot rejected")?;
    let scope = TenantScope::new(&c.tenant_id).map_err(|_| "trusted owner tenant invalid")?;
    bind_transaction(&mut tx, &scope, DatabaseRole::Gateway, 5000)
        .await
        .map_err(|_| "tenant binding failed")?;
    let snapshot: String = sqlx::query_scalar("SELECT pg_current_snapshot()::text")
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "snapshot identity unavailable")?;
    let effect: Value = sqlx::query_scalar(EFFECT)
        .bind(&c.tenant_id)
        .bind(&c.user_id)
        .bind(required_text(o, "messageId")?)
        .bind(&c.peer)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| "A2A ownership source unavailable")?
        .ok_or("verified durable A2A owner/task missing")?;
    ensure(
        effect["effectState"] == "confirmed"
            && effect["task"]["id"] == o["task"]["id"]
            && effect["task"]["metadata"] == o["task"]["metadata"],
        "observation not bound to exact durable Gateway task",
    )?;
    let calls = o["task"]["metadata"]["modelCalls"]
        .as_array()
        .filter(|x| !x.is_empty() && x.len() <= 8)
        .ok_or("bounded model call list required")?;
    let mut models = Vec::new();
    for call in calls {
        let id = required_text(call, "gateway_request_id")?;
        let wire = rows(&mut tx, WIRE, c, id).await?;
        let usage = rows(&mut tx, USAGE, c, id).await?;
        let price = rows(&mut tx, PRICE, c, id).await?;
        let budget = rows(&mut tx, BUDGET, c, id).await?;
        let checkpoint = rows(&mut tx, CHECKPOINT, c, id).await?;
        let guard = rows(&mut tx, GUARD, c, id).await?;
        models.push(project_model(
            c,
            call,
            &wire,
            &usage,
            &price,
            &budget,
            &checkpoint,
            &guard,
        )?);
    }
    let mut capabilities = Vec::new();
    for call in o["task"]["metadata"]["capabilityCalls"]
        .as_array()
        .filter(|x| !x.is_empty() && x.len() <= 8)
        .ok_or("bounded MCP calls required")?
    {
        let wire = rows(&mut tx, WIRE, c, required_text(call, "request_id")?).await?;
        let row = matching(&wire, "mcp", "completed")?;
        ensure(
            row["summary"]["tool"] == call["name"],
            "MCP tool authority mismatch",
        )?;
        capabilities.push(json!({"requestId":call["request_id"],"tenantId":c.tenant_id,"userId":c.user_id,"authorized":true,"sourceRowsSha256":digest(row)?,"source":row}));
    }
    let mut negative = serde_json::Map::new();
    for (name, field, outcome, status) in [
        ("guardrail", "guardrailMessageId", "denied", 200),
        ("cancel", "cancelMessageId", "canceled", 200),
    ] {
        let id = required_text(o, field)?;
        let wire = rows(&mut tx, WIRE, c, id).await?;
        let row = matching(&wire, "a2a", outcome)?;
        ensure(
            row["summary"]["http_status"] == status
                && row["summary"]["local_message_id"] == id
                && row["summary"]["tool"] == c.peer,
            "negative boundary did not match actual Gateway outcome",
        )?;
        let mut extra = json!({"boundary":row});
        if name == "cancel" {
            let canceled: Value = sqlx::query_scalar(EFFECT)
                .bind(&c.tenant_id)
                .bind(&c.user_id)
                .bind(id)
                .bind(&c.peer)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "cancel ownership source unavailable")?
                .ok_or("cancel effect source missing")?;
            ensure(
                canceled["cancelState"] == "confirmed"
                    && canceled["task"]["status"]["state"] == "TASK_STATE_CANCELED"
                    && canceled["task"]["id"] == o["cancelTask"]["id"],
                "actual cancel ownership receipt missing",
            )?;
            let pending = o["cancelTask"]["metadata"]["modelCalls"]
                .as_array()
                .filter(|v| v.len() <= 8)
                .ok_or("bounded cancel dispatches required")?;
            let mut effects = Vec::new();
            for call in pending {
                let wires = rows(&mut tx, WIRE, c, required_text(call, "request_id")?).await?;
                let w = wires
                    .iter()
                    .rev()
                    .find(|r| {
                        r["summary"]["kind"] == "model"
                            && r["summary"]["client_request_id"] == call["request_id"]
                            && r["summary"]["request_sha256"] == call["request_hash"]
                    })
                    .ok_or("cancel model dispatch cannot be reconciled to actual Gateway wire")?;
                let request = required_text(w, "requestId")?;
                let budget = rows(&mut tx, BUDGET, c, request).await?;
                let usage = rows(&mut tx, USAGE, c, request).await?;
                ensure(
                    !budget.is_empty() || w["summary"]["outcome"] == "denied",
                    "cancelled model lacks actual reservation or proven denial",
                )?;
                effects.push(json!({"wire":w,"budget":budget,"usage":usage,"financialState":if budget.iter().any(|r|r["state"]!="settled"&&r["state"]!="cancelled"){"unknown"}else{"confirmed"}}));
            }
            extra["cancellation"] =
                json!({"ownership":canceled,"modelEffects":effects,"externalEffectsUndone":false});
        }
        if name == "guardrail" {
            let failures = o["guardrailTask"]["metadata"]["modelCalls"]
                .as_array()
                .filter(|v| !v.is_empty() && v.len() <= 8)
                .ok_or("actual failed guardrail model call required")?;
            let mut decisions = Vec::new();
            for call in failures {
                let id = required_text(call, "gateway_request_id")?;
                let g = rows(&mut tx, GUARD, c, id).await?;
                ensure(
                    g.iter().any(|r| r["status"] == "blocked"),
                    "actual guardrail block absent",
                )?;
                decisions.extend(g);
            }
            extra["guardrails"] = json!(decisions);
        }
        negative.insert(name.into(),json!({"messageId":id,"verified":true,"sourceRowsSha256":digest(&extra)?,"source":extra}));
    }
    let quota_id = required_text(o, "quotaMessageId")?;
    ensure(o["quotaUserId"] == c.quota_user_id, "quota actor mismatch")?;
    let mut quota_owner = c.clone();
    quota_owner.user_id = c.quota_user_id.clone();
    let wire = rows(
        &mut tx,
        WIRE,
        &quota_owner,
        required_text(o, "quotaGatewayRequestId")?,
    )
    .await?;
    let denied = matching(&wire, "model", "denied")?;
    ensure(
        denied["summary"]["http_status"] == 429
            && denied["summary"]["client_request_id"] == quota_id,
        "real quota rejection missing",
    )?;
    let not_dispatched:bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM gateway.dispatch_token_reservations WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 AND state IN ('dispatched','awaiting_checkpoint','settled','outcome_unknown')) AND NOT EXISTS(SELECT 1 FROM gateway.usage_records WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3)").bind(&c.tenant_id).bind(&c.quota_user_id).bind(required_text(o,"quotaGatewayRequestId")?).fetch_one(&mut *tx).await.map_err(|_|"quota dispatch fence source unavailable")?;
    ensure(not_dispatched, "quota denial already dispatched model")?;
    let quota:Value=sqlx::query_scalar("SELECT jsonb_build_object('tenantId',tenant_id,'userId',user_id,'policy',overage_strategy,'dailyLimit',daily_token_limit,'monthlyLimit',monthly_token_limit,'dailyTokens',current_daily_tokens,'monthlyTokens',current_monthly_tokens,'exhausted',overage_strategy='hard_block' AND (temporary_expires_at IS NULL OR temporary_expires_at<=CURRENT_TIMESTAMP OR COALESCE(temporary_extra_tokens,0)=0) AND ((daily_token_limit>0 AND current_daily_tokens>=daily_token_limit AND daily_reset_at>CURRENT_TIMESTAMP) OR (monthly_token_limit>0 AND current_monthly_tokens>=monthly_token_limit AND monthly_reset_at>CURRENT_TIMESTAMP))) FROM gateway.user_quotas WHERE tenant_id=$1 AND user_id=$2").bind(&c.tenant_id).bind(&c.quota_user_id).fetch_optional(&mut *tx).await.map_err(|_|"actual quota policy source unavailable")?.ok_or("actual quota owner source missing")?;
    ensure(
        quota["exhausted"] == true,
        "controlled quota actor is not actually hard-block exhausted",
    )?;
    let source =
        json!({"wire":denied,"quota":quota,"notDispatched":true,"quotaUserId":c.quota_user_id});
    negative.insert("quota".into(),json!({"messageId":quota_id,"verified":true,"sourceRowsSha256":digest(&source)?,"source":source}));
    tx.rollback()
        .await
        .map_err(|_| "read-only snapshot rollback unconfirmed")?;
    pool.close().await;
    Ok((
        models,
        capabilities,
        Value::Object(negative),
        json!({"databaseRole":"ai_gateway_gateway","isolation":"repeatable_read","readOnly":true,"snapshot":snapshot,"taskOwnership":effect,"admissionPermitSha256":hash(&read(&s.admission_permit_file,true)?)}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rehearsal_origins_are_private_ips_only() {
        for s in [
            "https://127.0.0.1:9443",
            "https://10.12.0.5",
            "https://[::1]",
        ] {
            assert!(private_origin(s));
        }
        for s in [
            "https://public.example",
            "https://8.8.8.8",
            "http://127.0.0.1",
        ] {
            assert!(!private_origin(s));
        }
    }
    #[test]
    fn missing_real_sources_never_project() {
        let c = super::super::tests::fixture_config();
        assert!(project_model(&c, &json!({}), &[], &[], &[], &[], &[], &[]).is_err());
    }
    #[test]
    fn snapshots_cannot_become_writable_or_change_roles() {
        assert!(USAGE.contains("tenant_id=$1 AND user_id=$2"));
        assert!(BUDGET.contains("tenant_id=$1 AND user_id=$2"));
        assert!(GUARD.contains("tenant_id=$1 AND user_id=$2"));
        assert!(
            ![WIRE, USAGE, PRICE, BUDGET, CHECKPOINT, GUARD, EFFECT]
                .iter()
                .any(|q| q.contains("INSERT ") || q.contains("UPDATE ") || q.contains("DELETE "))
        );
    }
    #[test]
    fn source_projection_requires_real_matching_usage_price_budget_checkpoint_and_guardrail() {
        let c = super::super::tests::fixture_config();
        let price = json!({"price_book_schema":"agentforge.price_book/v2","reference":"unit-only"});
        let call = json!({"request_id":"client-unit","request_hash":"body-hash","gateway_request_id":"server-unit","response_id":"wire-unit","input_tokens":5,"output_tokens":3});
        let wire = vec![
            json!({"auditId":1,"requestId":"server-unit","summary":{"kind":"model","outcome":"completed","client_request_id":"client-unit","request_sha256":"body-hash","wire_response_id":"wire-unit"}}),
        ];
        let usage = vec![
            json!({"id":"unit-usage-id","requestId":"server-unit","inputTokens":5,"outputTokens":3,"inputCostCents":1,"outputCostCents":2,"status":"success","serviceId":"s","requestType":"invoke","pricing":price,"cache":{"read":{"state":"not_reported"}}}),
        ];
        let prices = vec![json!({"serviceId":"s","requestType":"invoke","snapshot":price})];
        let budgets = vec![
            json!({"member":"unit-budget","serviceId":"s","requestType":"invoke","state":"settled","observation":{"pricing_snapshot":price,"input_tokens":5,"output_tokens":3}}),
        ];
        let checkpoints = vec![
            json!({"serviceId":"s","requestType":"invoke","pricing":price,"inputTokens":5,"outputTokens":3}),
        ];
        let guards = vec![
            json!({"auditId":2,"decision":{"direction":"input","decision":"allow","policy_pin":"unit-pin"}}),
        ];
        let projected = project_model(
            &c,
            &call,
            &wire,
            &usage,
            &prices,
            &budgets,
            &checkpoints,
            &guards,
        )
        .unwrap();
        assert!(projected["cachedInputTokens"].is_null());
        assert_eq!(projected["cacheState"], "not_reported");
        assert_eq!(projected["amountMicros"], 30000);
        for (which, pointer, value) in [
            (0, "/0/summary/wire_response_id", json!("foreign")),
            (0, "/0/summary/request_sha256", json!("foreign")),
            (1, "/0/inputTokens", json!(99)),
            (1, "/0/cache/read/state", json!("invalid")),
            (2, "/0/snapshot/reference", json!("different")),
            (3, "/0/state", json!("outcome_unknown")),
            (4, "/0/outputTokens", json!(99)),
            (5, "/0/decision/decision", json!("block")),
        ] {
            let mut groups = vec![
                json!(wire),
                json!(usage),
                json!(prices),
                json!(budgets),
                json!(checkpoints),
                json!(guards),
            ];
            *groups[which].pointer_mut(pointer).unwrap() = value;
            assert!(
                project_model(
                    &c,
                    &call,
                    groups[0].as_array().unwrap(),
                    groups[1].as_array().unwrap(),
                    groups[2].as_array().unwrap(),
                    groups[3].as_array().unwrap(),
                    groups[4].as_array().unwrap(),
                    groups[5].as_array().unwrap()
                )
                .is_err(),
                "{pointer}"
            );
        }
    }
}
