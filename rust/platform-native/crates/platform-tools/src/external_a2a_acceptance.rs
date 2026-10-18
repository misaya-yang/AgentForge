//! Opt-in U-SW-03 external-framework acceptance. No listener, deployment, model
//! credentials, retry loop or platform kernel. Root CLI exports `command`.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::{Method, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn ensure(ok: bool, code: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(code.into()) }
}
fn read(path: &Path, private: bool) -> Result<Vec<u8>> {
    ensure(path.is_absolute(), "absolute file required")?;
    let m = fs::symlink_metadata(path).map_err(|_| "file unavailable")?;
    ensure(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= 4 * 1024 * 1024,
        "file policy rejected",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure(
            m.permissions().mode() & if private { 0o077 } else { 0o022 } == 0,
            "file permissions rejected",
        )?;
    }
    fs::read(path).map_err(|_| "file read failed".into())
}
fn token(path: &Path) -> Result<String> {
    let value = String::from_utf8(read(path, true)?).map_err(|_| "token encoding")?;
    let value = value.trim_end_matches(['\r', '\n']);
    ensure(
        value.len() >= 32 && value.bytes().all(|b| (33..=126).contains(&b)),
        "token policy rejected",
    )?;
    Ok(value.into())
}
fn origin(raw: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(raw).map_err(|_| "origin invalid")?;
    ensure(
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/"
            && u.query().is_none()
            && u.fragment().is_none()
            && raw == u.origin().ascii_serialization(),
        "exact HTTPS origin required",
    )?;
    Ok(u)
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    schema: String,
    gateway_origin: String,
    address: IpAddr,
    ca_file: PathBuf,
    peer: String,
    tenant_id: String,
    user_id: String,
    actor_token_file: PathBuf,
    same_tenant_other_user_token_file: PathBuf,
    other_tenant_token_file: PathBuf,
    exhausted_quota_token_file: PathBuf,
    quota_user_id: String,
    quota_model: String,
    guardrail_prompt_file: PathBuf,
    output_directory: PathBuf,
    repository: PathBuf,
    evidence_key_id: String,
    evidence_public_key: String,
}
impl Config {
    fn load(path: &Path) -> Result<Self> {
        let c: Self =
            serde_json::from_slice(&read(path, true)?).map_err(|_| "configuration invalid")?;
        ensure(
            c.schema == "agentforge-external-a2a-acceptance/v1"
                && !c.peer.is_empty()
                && c.peer.len() <= 64
                && c.peer
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !c.tenant_id.is_empty()
                && !c.user_id.is_empty()
                && !c.quota_user_id.is_empty()
                && !c.quota_model.is_empty(),
            "configuration invalid",
        )?;
        ensure(
            !c.evidence_key_id.starts_with("UNIT_TEST_ONLY")
                && c.evidence_public_key != "Zs1gi5KLiOUODv6qM_rxxDzv4HKUsLh-n-CrpqPPdjM",
            "synthetic fixture evidence key cannot be admitted",
        )?;
        origin(&c.gateway_origin)?;
        c.client()?;
        let repo = c
            .repository
            .canonicalize()
            .map_err(|_| "repository unavailable")?;
        let directory = c
            .output_directory
            .canonicalize()
            .map_err(|_| "existing external private output directory required")?;
        ensure(
            c.output_directory.is_absolute() && !directory.starts_with(repo),
            "external output directory required",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure(
                fs::metadata(directory)
                    .map_err(|_| "output unavailable")?
                    .permissions()
                    .mode()
                    & 0o077
                    == 0,
                "private output directory required",
            )?;
        }
        ensure(
            B64.decode(&c.evidence_public_key)
                .ok()
                .is_some_and(|x| x.len() == 32)
                && !c.evidence_key_id.is_empty(),
            "explicit evidence key pin required",
        )?;
        Ok(c)
    }
    fn client(&self) -> Result<Client> {
        let u = origin(&self.gateway_origin)?;
        let host = u.host_str().ok_or("host required")?;
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            ensure(
                ip == self.address,
                "literal Gateway origin must match pinned address",
            )?;
        }
        let cert = reqwest::Certificate::from_pem(&read(&self.ca_file, false)?)
            .map_err(|_| "CA invalid")?;
        Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_certs_only([cert])
            .resolve(
                host,
                SocketAddr::new(self.address, u.port_or_known_default().unwrap_or(443)),
            )
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(125))
            .build()
            .map_err(|_| "client configuration failed".into())
    }
}
fn write_new(directory: &Path, name: &str, value: &Value) -> Result<()> {
    let mut o = fs::OpenOptions::new();
    o.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(directory.join(name))
        .map_err(|_| "output exists or unavailable")?;
    f.write_all(&serde_json::to_vec_pretty(value).map_err(|_| "report encoding")?)
        .and_then(|_| f.sync_all())
        .map_err(|_| "report write failed".into())
}
fn call(
    client: &Client,
    c: &Config,
    auth: &str,
    method: Method,
    suffix: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let url = format!("{}/a2a/outbound/{}/{}", c.gateway_origin, c.peer, suffix);
    let mut req = client
        .request(method, url)
        .bearer_auth(auth)
        .header("a2a-version", "1.0");
    if let Some(body) = body {
        req = req.json(body);
    }
    let response = req
        .send()
        .map_err(|_| "transport effect unknown; do not resend")?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "response effect unknown; do not resend")?;
    ensure(
        bytes.len() <= 4 * 1024 * 1024,
        "response too large; effect unknown",
    )?;
    let value = serde_json::from_slice(&bytes).map_err(|_| "response invalid; effect unknown")?;
    Ok((status, value))
}
fn task(value: &Value) -> &Value {
    value.get("task").unwrap_or(value)
}
fn message(id: &str, text: &str, immediate: bool) -> Value {
    json!({"message":{"messageId":id,"role":"ROLE_USER","parts":[{"text":text}]},"configuration":{"returnImmediately":immediate}})
}
fn required_text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing {key}"))
}
/// No network unless both flags are supplied. Every uncertain send stops; the
/// operator may reconcile its recorded message ID but must never create a resend.
fn run(c: &Config) -> Result<Value> {
    let client = c.client()?;
    let auth = token(&c.actor_token_file)?;
    let same = token(&c.same_tenant_other_user_token_file)?;
    let other = token(&c.other_tenant_token_file)?;
    let quota = token(&c.exhausted_quota_token_file)?;
    let guardrail = String::from_utf8(read(&c.guardrail_prompt_file, true)?)
        .map_err(|_| "guardrail fixture encoding")?;
    ensure(
        !guardrail.trim().is_empty(),
        "explicit blocked guardrail fixture required",
    )?;
    let id = uuid::Uuid::new_v4().to_string();
    let cancel_id = uuid::Uuid::new_v4().to_string();
    let quota_id = uuid::Uuid::new_v4().to_string();
    let guardrail_id = uuid::Uuid::new_v4().to_string();
    // Persist operation identities BEFORE dispatch; no prompts or credentials.
    write_new(
        &c.output_directory,
        "intent.json",
        &json!({"schema":"agentforge-external-a2a-intent/v1","tenantId":c.tenant_id,"userId":c.user_id,"peer":c.peer,"messageId":id,"cancelMessageId":cancel_id,"quotaMessageId":quota_id,"guardrailMessageId":guardrail_id,"status":"not_run"}),
    )?;
    let send = message(
        &id,
        "Use the supplied read-only MCP tool once, then answer 'governed external framework'.",
        false,
    );
    let (status, answer) = call(&client, c, &auth, Method::POST, "message:send", Some(&send))?;
    ensure(
        status == 200,
        "framework send failed or unknown; inspect intent before reconciliation",
    )?;
    let completed = task(&answer);
    ensure(
        completed["status"]["state"] == "TASK_STATE_COMPLETED"
            && completed["metadata"]["effectState"] == "confirmed"
            && completed["metadata"]["framework"] == "openai-agents-js@0.20.0"
            && completed["metadata"]["toolCalls"]
                .as_u64()
                .is_some_and(|n| n >= 1)
            && completed["metadata"]["frameworkCalls"]
                .as_u64()
                .is_some_and(|n| n >= 2),
        "real framework/tool completion required",
    )?;
    validate_calls(completed)?;
    let remote_id = required_text(completed, "id")?;
    // Repeating an already confirmed operation must return the same task. The
    // Gateway's durable effect adapter owns deduplication, not this test client.
    let (duplicate_status, duplicate) =
        call(&client, c, &auth, Method::POST, "message:send", Some(&send))?;
    ensure(
        duplicate_status == 200 && task(&duplicate)["id"] == remote_id,
        "idempotency failed",
    )?;
    let (conflict, _) = call(
        &client,
        c,
        &auth,
        Method::POST,
        "message:send",
        Some(&message(&id, "changed input", false)),
    )?;
    ensure(conflict == 409, "message conflict not rejected")?;
    for foreign in [&same, &other] {
        let (status, _) = call(
            &client,
            c,
            foreign,
            Method::GET,
            &format!("tasks/{id}"),
            None,
        )?;
        ensure(
            status == 404 || status == 403,
            "cross-principal task leaked",
        )?;
    }
    let (get_status, got) = call(&client, c, &auth, Method::GET, &format!("tasks/{id}"), None)?;
    ensure(
        get_status == 200 && task(&got)["id"] == remote_id,
        "owner task lookup failed",
    )?;
    let (reconcile_status, reconciled) = call(
        &client,
        c,
        &auth,
        Method::POST,
        &format!("tasks/{id}:reconcile"),
        Some(&json!({})),
    )?;
    ensure(
        reconcile_status == 200 && task(&reconciled)["id"] == remote_id,
        "read-only reconcile failed",
    )?;
    let quota_response=client.post(format!("{}/v1/chat/completions",c.gateway_origin)).bearer_auth(&quota).header("x-request-id",&quota_id).header("x-agentforge-a2a-task-id",remote_id).json(&json!({"model":c.quota_model,"messages":[{"role":"user","content":"Controlled exhausted-quota probe."}],"max_tokens":16,"stream":false})).send().map_err(|_|"quota probe transport unconfirmed")?;
    ensure(
        quota_response.status().as_u16() == 429,
        "exhausted model quota was not rejected",
    )?;
    let quota_gateway_request_id = quota_response
        .headers()
        .get("x-gateway-request-id")
        .and_then(|h| h.to_str().ok())
        .ok_or("quota source request identity missing")?
        .to_owned();
    let (guard_status, guarded) = call(
        &client,
        c,
        &auth,
        Method::POST,
        "message:send",
        Some(&message(&guardrail_id, &guardrail, false)),
    )?;
    ensure(
        [400, 403, 422].contains(&guard_status)
            || guard_status == 200
                && ["TASK_STATE_FAILED", "TASK_STATE_REJECTED"]
                    .contains(&task(&guarded)["status"]["state"].as_str().unwrap_or("")),
        "preconfigured guardrail was not rejected",
    )?;
    let (cancel_send_status, cancel_send) = call(
        &client,
        c,
        &auth,
        Method::POST,
        "message:send",
        Some(&message(
            &cancel_id,
            "Use the read-only tool and produce a detailed answer.",
            true,
        )),
    )?;
    ensure(
        cancel_send_status == 200 && task(&cancel_send)["id"].is_string(),
        "cancel target admission failed",
    )?;
    let (cancel_status, canceled) = call(
        &client,
        c,
        &auth,
        Method::POST,
        &format!("tasks/{cancel_id}:cancel"),
        Some(&json!({})),
    )?;
    ensure(
        cancel_status == 200 && task(&canceled)["status"]["state"] == "TASK_STATE_CANCELED",
        "cancellation was not confirmed; a terminal race is inconclusive",
    )?;
    Ok(
        json!({"schema":"agentforge-external-a2a-observation/v1","gatewayOrigin":c.gateway_origin,"tenantId":c.tenant_id,"userId":c.user_id,"peer":c.peer,"messageId":id,"cancelMessageId":cancel_id,"quotaMessageId":quota_id,"guardrailMessageId":guardrail_id,"task":completed,"cancelTask":task(&canceled),"guardrailTask":task(&guarded),"quotaGatewayRequestId":quota_gateway_request_id,"quotaUserId":c.quota_user_id,"checks":{"idempotency":true,"conflict":true,"crossPrincipal":true,"quotaRejected":true,"guardrailRejected":true,"cancelConfirmed":true,"readOnlyReconcile":true},"governanceAcceptance":"not_run","reason":"trusted ledger evidence required; SDK usage is not a price-book receipt"}),
    )
}
fn validate_calls(t: &Value) -> Result<()> {
    let calls = t["metadata"]["modelCalls"]
        .as_array()
        .filter(|x| !x.is_empty())
        .ok_or("model call evidence missing")?;
    for call in calls {
        ensure(
            call["effect"] == "confirmed"
                && call["response_id"].is_string()
                && call["request_id"].is_string()
                && call["input_tokens"].as_u64().is_some()
                && call["output_tokens"].as_u64().is_some(),
            "unconfirmed model call",
        )?;
    }
    ensure(
        t["metadata"]["capabilityCalls"]
            .as_array()
            .is_some_and(|x| {
                x.iter()
                    .any(|v| v["effect"] == "confirmed" && v["request_id"].is_string())
            }),
        "real governed MCP call required",
    )
}
#[path = "external_a2a_acceptance/ledger.rs"]
mod ledger;
fn signed_evidence(raw: &str, kid: &str, public: &str) -> Result<Value> {
    signed(raw, kid, public, "agentforge-external-a2a-evidence+jwt")
}
fn signed(raw: &str, kid: &str, public: &str, typ: &str) -> Result<Value> {
    let p: Vec<_> = raw.trim().split('.').collect();
    ensure(p.len() == 3, "compact evidence JWS required")?;
    let bytes = B64.decode(public).map_err(|_| "evidence key invalid")?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| "evidence key length")?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| "evidence key invalid")?;
    let header: Value = serde_json::from_slice(&B64.decode(p[0]).map_err(|_| "evidence header")?)
        .map_err(|_| "evidence header")?;
    ensure(
        header["alg"] == "EdDSA"
            && header["typ"] == typ
            && header["kid"] == kid
            && header.as_object().is_some_and(|o| o.len() == 3),
        "evidence header rejected",
    )?;
    let sig = Signature::from_slice(&B64.decode(p[2]).map_err(|_| "evidence signature")?)
        .map_err(|_| "evidence signature")?;
    key.verify_strict(format!("{}.{}", p[0], p[1]).as_bytes(), &sig)
        .map_err(|_| "evidence signature rejected")?;
    serde_json::from_slice(&B64.decode(p[1]).map_err(|_| "evidence payload")?)
        .map_err(|_| "evidence payload rejected".into())
}
fn verify_evidence(c: &Config, observed_bytes: &[u8], evidence: &Value, now: u64) -> Result<Value> {
    let o: Value = serde_json::from_slice(observed_bytes).map_err(|_| "observation invalid")?;
    ensure(
        o["schema"] == "agentforge-external-a2a-observation/v1"
            && o["gatewayOrigin"] == c.gateway_origin
            && o["tenantId"] == c.tenant_id
            && o["userId"] == c.user_id
            && o["peer"] == c.peer,
        "observation scope mismatch",
    )?;
    ensure(
        o["task"]["status"]["state"] == "TASK_STATE_COMPLETED"
            && o["task"]["metadata"]["framework"] == "openai-agents-js@0.20.0"
            && o["task"]["metadata"]["effectState"] == "confirmed"
            && o["cancelTask"]["status"]["state"] == "TASK_STATE_CANCELED",
        "completed framework and cancellation required",
    )?;
    for key in [
        "idempotency",
        "conflict",
        "crossPrincipal",
        "quotaRejected",
        "guardrailRejected",
        "cancelConfirmed",
        "readOnlyReconcile",
    ] {
        ensure(o["checks"][key] == true, "missing observed check")?;
    }
    validate_calls(&o["task"])?;
    ensure(
        evidence["schema"] == "agentforge-external-a2a-ledger-evidence/v1"
            && evidence["observationSha256"] == hash(observed_bytes)
            && evidence["gatewayOrigin"] == c.gateway_origin
            && evidence["tenantId"] == c.tenant_id
            && evidence["userId"] == c.user_id
            && evidence["messageId"] == o["messageId"]
            && evidence["peer"] == c.peer
            && evidence["admission"] == o["task"]["metadata"]["admission"]
            && ["rehearsal", "production"].contains(&evidence["admission"].as_str().unwrap_or("")),
        "trusted evidence scope mismatch",
    )?;
    let iat = evidence["iat"].as_u64().ok_or("evidence iat missing")?;
    let exp = evidence["exp"].as_u64().ok_or("evidence exp missing")?;
    ensure(
        iat <= now + 5
            && exp > now
            && exp > iat
            && exp - iat <= 3600
            && (evidence["admission"] == "rehearsal" && evidence["uGw15"]["status"] == "not_run"
                || evidence["admission"] != "rehearsal"
                    && evidence["uGw15"]["status"] == "direct_verified")
            && evidence["uGw15"]["evidenceSha256"]
                .as_str()
                .is_some_and(hex_sha),
        "direct verification or evidence time gate closed",
    )?;
    let rows = evidence["modelReceipts"]
        .as_array()
        .ok_or("real ledger receipts required")?;
    let calls = o["task"]["metadata"]["modelCalls"]
        .as_array()
        .ok_or("calls missing")?;
    ensure(
        rows.len() == calls.len(),
        "every model call needs one real receipt",
    )?;
    let mut receipt_ids = std::collections::BTreeSet::new();
    for call in calls {
        let matches: Vec<_> = rows
            .iter()
            .filter(|r| {
                r["requestId"] == call["request_id"] && r["responseId"] == call["response_id"]
            })
            .collect();
        ensure(matches.len() == 1, "receipt identity mismatch")?;
        let r = matches[0];
        let rid = required_text(r, "receiptId")?;
        ensure(receipt_ids.insert(rid), "duplicate receipt")?;
        ensure(
            r["tenantId"] == c.tenant_id
                && r["userId"] == c.user_id
                && r["inputTokens"] == call["input_tokens"]
                && r["outputTokens"] == call["output_tokens"]
                && r["state"] == "settled"
                && r["amountMicros"].as_u64().is_some()
                && (r["cachedInputTokens"].is_null() && r["cacheState"] == "not_reported"
                    || r["cachedInputTokens"]
                        .as_u64()
                        .is_some_and(|n| n <= r["inputTokens"].as_u64().unwrap_or(0)))
                && r["priceBookVersion"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                && r["budgetReservationId"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                && r["guardrailDecisionId"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                && r["sourceRowsSha256"].as_str().is_some_and(hex_sha),
            "real scoped budget/pricebook/cache/guardrail settlement missing",
        )?;
    }
    for (name, message_key) in [
        ("quota", "quotaMessageId"),
        ("guardrail", "guardrailMessageId"),
        ("cancel", "cancelMessageId"),
    ] {
        let row = &evidence["negativeCases"][name];
        ensure(
            row["messageId"] == o[message_key]
                && row["verified"] == true
                && row["sourceRowsSha256"].as_str().is_some_and(hex_sha),
            "negative-case ledger evidence missing",
        )?;
    }
    let cap_rows = evidence["capabilityReceipts"]
        .as_array()
        .ok_or("governed MCP evidence missing")?;
    for call in o["task"]["metadata"]["capabilityCalls"]
        .as_array()
        .ok_or("MCP calls missing")?
    {
        ensure(
            cap_rows.iter().any(|r| {
                r["requestId"] == call["request_id"]
                    && r["tenantId"] == c.tenant_id
                    && r["userId"] == c.user_id
                    && r["authorized"] == true
                    && r["sourceRowsSha256"].as_str().is_some_and(hex_sha)
            }),
            "MCP authority receipt missing",
        )?;
    }
    Ok(
        json!({"schema":"agentforge-external-a2a-result/v1","status":if evidence["admission"]=="rehearsal"{"rehearsal_verified"}else{"direct_verified"},"admission":evidence["admission"],"observationSha256":hash(observed_bytes),"evidenceSha256":hash(&serde_json::to_vec(evidence).map_err(|_| "evidence encoding")?),"modelReceipts":rows.len(),"framework":"openai-agents-js@0.20.0"}),
    )
}
fn hex_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// root: `external-a2a-acceptance run CONFIG --apply --allow-model-calls`
/// or `external-a2a-acceptance verify CONFIG OBSERVATION EVIDENCE_JWS`.
pub fn command(args: &[String]) -> Result<Value> {
    match args.first().map(String::as_str) {
        Some("export") if args.len()==5 && args[4]=="--read-only" => {let c=Config::load(Path::new(&args[1]))?;ledger::command(&c,Path::new(&args[2]),Path::new(&args[3]))},
        Some("run") if args.len() == 4 && args[2] == "--apply" && args[3] == "--allow-model-calls" => { let c = Config::load(Path::new(&args[1]))?; let observed = run(&c)?; write_new(&c.output_directory, "observation.json", &observed)?; Ok(json!({"status":"observed","governanceAcceptance":"not_run","output":c.output_directory.join("observation.json")})) }
        Some("verify") if args.len() == 4 => { let c = Config::load(Path::new(&args[1]))?; let observed = read(Path::new(&args[2]), true)?; let compact = String::from_utf8(read(Path::new(&args[3]), true)?).map_err(|_| "evidence encoding")?; let e = signed_evidence(&compact, &c.evidence_key_id, &c.evidence_public_key)?; let result = verify_evidence(&c, &observed, &e, SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "clock invalid")?.as_secs())?; write_new(&c.output_directory, if result["admission"]=="rehearsal"{"rehearsal-verified.json"}else{"accepted.json"}, &result)?; Ok(result) }
        _ => Err("usage: external-a2a-acceptance run CONFIG --apply --allow-model-calls | verify CONFIG OBSERVATION EVIDENCE_JWS; no implicit execution".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_implicit_paid_execution() {
        assert!(command(&[]).is_err());
        assert!(command(&["run".into(), "/missing".into()]).is_err());
    }
    #[test]
    fn exact_origin_only() {
        for u in [
            "http://gateway.example",
            "https://u:p@gateway.example",
            "https://gateway.example/path",
            "https://gateway.example?x=1",
            "https://gateway.example/",
        ] {
            assert!(origin(u).is_err());
        }
        assert!(origin("https://gateway.example").is_ok());
    }
    #[test]
    fn fabricated_usage_is_not_a_receipt() {
        assert!(validate_calls(&json!({"metadata":{"modelCalls":[{"effect":"confirmed","input_tokens":1,"output_tokens":2}]}})).is_err());
    }
    #[test]
    fn unknown_effect_and_missing_mcp_fail() {
        assert!(validate_calls(&json!({"metadata":{"modelCalls":[{"effect":"dispatched","request_id":"r","response_id":"s","input_tokens":1,"output_tokens":2}],"capabilityCalls":[]}})).is_err());
    }
    #[test]
    fn wrong_signature_and_self_key_fail() {
        assert!(signed_evidence("e30.e30.AA", "pin", &B64.encode([0u8; 32])).is_err());
    }
    #[test]
    fn node_interop_fixture_signature_and_header_are_native_verified() {
        let f: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/external-a2a/native-evidence.test-only.json"
        ))
        .unwrap();
        let e = signed_evidence(
            f["jws"].as_str().unwrap(),
            f["kid"].as_str().unwrap(),
            f["publicKey"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(e, f["payload"]);
        assert!(
            signed_evidence(
                f["jws"].as_str().unwrap(),
                "other-kid",
                f["publicKey"].as_str().unwrap()
            )
            .is_err()
        );
    }
    pub(super) fn fixture_config() -> Config {
        Config {
            schema: "agentforge-external-a2a-acceptance/v1".into(),
            gateway_origin: "https://gateway.example".into(),
            address: "127.0.0.1".parse().unwrap(),
            ca_file: PathBuf::new(),
            peer: "fixture-peer".into(),
            tenant_id: "tenant-fixture".into(),
            user_id: "user-fixture".into(),
            actor_token_file: PathBuf::new(),
            same_tenant_other_user_token_file: PathBuf::new(),
            other_tenant_token_file: PathBuf::new(),
            exhausted_quota_token_file: PathBuf::new(),
            quota_user_id: "quota-fixture".into(),
            quota_model: "unit-model".into(),
            guardrail_prompt_file: PathBuf::new(),
            output_directory: PathBuf::new(),
            repository: PathBuf::new(),
            evidence_key_id: "UNIT_TEST_ONLY_NEVER_TRUST".into(),
            evidence_public_key: String::new(),
        }
    }
    #[test]
    fn real_receipt_contract_rejects_scope_time_usage_budget_and_fabricated_promotion() {
        let f: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/external-a2a/native-evidence.test-only.json"
        ))
        .unwrap();
        let raw = f["rawObservation"].as_str().unwrap().as_bytes();
        let c = fixture_config();
        let evidence = &f["payload"];
        assert!(verify_evidence(&c, raw, evidence, 1800000000).is_ok());
        for (pointer, value) in [
            ("/tenantId", json!("other-tenant")),
            ("/userId", json!("other-user")),
            ("/observationSha256", json!("f".repeat(64))),
            ("/exp", json!(1800000000)),
            ("/uGw15/status", json!("fixture_passed")),
            ("/modelReceipts", json!([])),
            ("/modelReceipts/0/state", json!("pending")),
            ("/modelReceipts/0/responseId", json!("different-response")),
            ("/modelReceipts/0/inputTokens", json!(99)),
            ("/modelReceipts/0/cachedInputTokens", json!(99)),
            ("/modelReceipts/0/amountMicros", json!(-1)),
            ("/modelReceipts/0/budgetReservationId", json!("")),
            ("/modelReceipts/0/guardrailDecisionId", json!("")),
            ("/modelReceipts/0/priceBookVersion", json!("")),
            ("/negativeCases/quota/verified", json!(false)),
            ("/negativeCases/cancel/messageId", json!("other")),
            ("/capabilityReceipts/0/authorized", json!(false)),
        ] {
            let mut changed = evidence.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                verify_evidence(&c, raw, &changed, 1800000000).is_err(),
                "{pointer}"
            );
        }
    }
    #[test]
    fn rehearsal_evidence_never_promotes_a_rehearsal_transcript_to_production() {
        let f: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/external-a2a/native-evidence.test-only.json"
        ))
        .unwrap();
        let mut o: Value = serde_json::from_str(f["rawObservation"].as_str().unwrap()).unwrap();
        o["task"]["metadata"]["admission"] = json!("rehearsal");
        let raw = serde_json::to_vec(&o).unwrap();
        let mut evidence = f["payload"].clone();
        evidence["observationSha256"] = json!(hash(&raw));
        assert!(verify_evidence(&fixture_config(), &raw, &evidence, 1800000000).is_err());
        evidence["admission"] = json!("rehearsal");
        evidence["uGw15"]["status"] = json!("not_run");
        let result = verify_evidence(&fixture_config(), &raw, &evidence, 1800000000).unwrap();
        assert_eq!(result["status"], "rehearsal_verified");
        assert_ne!(result["status"], "direct_verified");
    }
}
