//! Real isolated worker API assertions. These commands never substitute fixture responses.
use crate::*;
use ai_platform_capability_contract::{
    CapabilityEffect, RuntimeCapabilityLeaseV1, canonical_json_hash,
};
use reqwest::{
    Client, Method,
    header::{HeaderMap, HeaderValue},
};
use std::time::Duration;
fn env(key: &str) -> Result<String> {
    Ok(std::env::var(format!("CAPABILITY_SMOKE_{key}"))?)
}
fn terminal(v: &Value) -> bool {
    matches!(
        v.as_str(),
        Some("succeeded" | "failed" | "cancelled" | "timeout" | "side_effect_unknown")
    )
}
struct Worker {
    client: Client,
    base: String,
    headers: HeaderMap,
}
impl Worker {
    fn new() -> Result<Self> {
        let base = env("URL")?;
        let url = reqwest::Url::parse(&base)?;
        require(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
            "smoke URL must be loopback HTTP",
        )?;
        let mut headers = HeaderMap::new();
        for (key, name) in [
            ("TOKEN", "x-ai-platform-internal-token"),
            ("TENANT", "x-ai-tenant-id"),
            ("USER", "x-ai-user-id"),
            ("SESSION", "x-ai-session-id"),
        ] {
            headers.insert(name, HeaderValue::from_str(&env(key)?)?);
        }
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()?,
            base,
            headers,
        })
    }
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        forged: bool,
    ) -> Result<(u16, Value)> {
        let mut headers = self.headers.clone();
        if forged {
            headers.insert("x-ai-tenant-id", HeaderValue::from_static("tenant-forged"));
        }
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .headers(headers);
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().await?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            require(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "smoke response exceeds bound",
            )?;
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, strict_json(&bytes)?))
    }
}
fn execution(descriptor: &Value, call: &str, args: Value, idempotency: &str) -> Result<Value> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let mut lease = RuntimeCapabilityLeaseV1 {
        schema_version: "runtime-capability-lease/v1".into(),
        lease_id: uuid::Uuid::new_v4().to_string(),
        tenant_id: env("TENANT")?,
        user_id: env("USER")?,
        session_id: env("SESSION")?,
        run_id: env("RUN")?,
        tool_call_id: call.into(),
        attempt_id: "attempt-1".into(),
        capability_id: "platform.read_fixture".into(),
        capability_revision: 1,
        arguments_hash: canonical_json_hash(&args)?,
        effect: CapabilityEffect::Read,
        approval_id: None,
        issued_at_epoch_ms: now,
        expires_at_epoch_ms: now + 120_000,
        nonce: uuid::Uuid::new_v4().simple().to_string(),
        signature: String::new(),
    };
    lease.sign(env("LEASE_SECRET")?.as_bytes())?;
    lease.validate(now)?;
    Ok(
        json!({"schema_version":"capability-execution/v2","lease":lease,"idempotency_key":idempotency,"descriptor":descriptor,"arguments":args}),
    )
}
fn id(v: &Value) -> Result<&str> {
    let id = v["execution_id"].as_str().ok_or("missing execution ID")?;
    require(
        id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "unsafe execution ID",
    )?;
    Ok(id)
}
async fn page(worker: &Worker, id: &str, after: u64) -> Result<Vec<Value>> {
    let (status, value) = worker
        .call(
            Method::GET,
            &format!("/internal/v2/capabilities/executions/{id}/events?after_sequence={after}"),
            None,
            false,
        )
        .await?;
    require(status == 200, "event page HTTP status mismatch")?;
    let events = value["events"].as_array().ok_or("missing events")?;
    require(
        events.len() <= 1,
        "event page exceeds configured single-item bound",
    )?;
    Ok(events.clone())
}
pub fn run(args: &Args) -> Result<Value> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let worker = Worker::new()?; let scratch = PathBuf::from(env("SCRATCH")?);
        if args.positionals.get(1).is_some_and(|s|s=="recovery") {
            let request = object(&scratch.join("recovery-request.json"))?;
            let expected = std::fs::read_to_string(scratch.join("recovery-execution-id"))?;
            let (status,replay) = worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&request),false).await?;
            require(status==200&&id(&replay)?==expected,"restart replay must preserve execution")?;
            let mut recovered = false;
            for _ in 0..800 { let (status,current)=worker.call(Method::GET,&format!("/internal/v2/capabilities/executions/{expected}"),None,false).await?; require(status==200,"restart poll failed")?; if terminal(&current["status"]) { require(current["status"]=="succeeded","recovery did not succeed")?; recovered=true; break; } tokio::time::sleep(Duration::from_millis(50)).await; }
            require(recovered,"worker restart did not recover read execution")?;
            let mut after=0; let mut terminals=Vec::new();
            for _ in 0..32 { let events=page(&worker,&expected,after).await?; let Some(event)=events.first() else {break}; let sequence=event["sequence"].as_u64().ok_or("invalid sequence")?; require(sequence>after,"event cursor not increasing")?; after=sequence; if terminal(&event["status"]) { terminals.push(event.clone()); } }
            require(terminals.len()==1&&terminals[0]["status"]=="succeeded","recovery terminal event must be unique and succeeded")?;
            return Ok(json!({"result":"pass","phase":"restart-recovery"}));
        }
        let (status,catalog)=worker.call(Method::POST,"/internal/v2/capabilities/catalog",Some(&json!({"schema_version":"capability-catalog/v2","tenant_id":env("TENANT")?,"user_id":env("USER")?,"session_id":env("SESSION")?,"capability_revision":1})),false).await?;
        require(status==200,"catalog HTTP status mismatch")?; let capabilities=catalog["capabilities"].as_array().ok_or("missing capabilities")?;
        for name in ["platform.echo","platform.read_fixture","search_knowledge_base"] { require(capabilities.iter().any(|v|v["id"]==name),format!("catalog missing {name}"))?; }
        let descriptor=capabilities.iter().find(|v|v["id"]=="platform.read_fixture").ok_or("fixture descriptor missing")?;
        let request=execution(descriptor,"call-basic",json!({"key":"basic"}),"idem-basic")?;
        let (status,created)=worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&request),false).await?; require(status==202,"execution must be accepted")?; let execution_id=id(&created)?;
        let (status,replay)=worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&request),false).await?; require(status==200&&id(&replay)?==execution_id,"idempotent replay mismatch")?;
        let conflict=execution(descriptor,"call-basic",json!({"key":"changed"}),"idem-basic")?;
        require(worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&conflict),false).await?.0==409,"conflicting replay must fail 409")?;
        require(worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&request),true).await?.0==403,"forged scope must fail 403")?;
        let mut after=0; let mut success=false;
        for _ in 0..100 { let events=page(&worker,execution_id,after).await?; if let Some(event)=events.first() { let sequence=event["sequence"].as_u64().ok_or("invalid sequence")?; require(sequence>after,"event cursor not increasing")?; after=sequence; if terminal(&event["status"]) { require(event["status"]=="succeeded","basic execution failed")?; success=true;break; } } tokio::time::sleep(Duration::from_millis(20)).await; }
        require(success,"basic execution did not finish")?;
        let cancel=execution(descriptor,"call-cancel",json!({"key":"cancel","delay_ms":5000}),"idem-cancel")?;
        let (status,created)=worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&cancel),false).await?; require(status==202,"cancel fixture not accepted")?;
        let (status,cancelled)=worker.call(Method::POST,&format!("/internal/v2/capabilities/executions/{}:cancel",id(&created)?),None,false).await?; require(status==200&&cancelled["status"]=="cancelled","cancel did not complete")?;
        let request=execution(descriptor,"call-recover",json!({"key":"recover","delay_ms":5000}),"idem-recover")?;
        let (status,created)=worker.call(Method::POST,"/internal/v2/capabilities/executions",Some(&request),false).await?; require(status==202,"recovery fixture not accepted")?;
        write_json(&scratch.join("recovery-request.json"),&request)?; std::fs::write(scratch.join("recovery-execution-id"),id(&created)?)?;
        Ok(json!({"result":"pass","phase":"catalog-idempotency-scope-cursor-cancel"}))
    })
}
