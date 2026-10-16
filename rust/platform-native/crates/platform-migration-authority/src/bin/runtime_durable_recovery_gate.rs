//! Controlled local crash harness; only explicit CLI execution performs operations.
mod runtime_recovery_common;
use runtime_recovery_common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
const STAGES: [&str; 6] = [
    "invocation_saved",
    "approval_saved",
    "before_dispatch",
    "worker_result_observed",
    "invocation_result_saved",
    "core_output_saved",
];
const TERMINAL: [&str; 3] = ["succeeded", "failed", "cancelled"];
async fn docker(args: &[&str], stdin: Option<&str>, allow_failure: bool) -> Result<String> {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn()?;
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().ok_or("Docker input unavailable")?;
        pipe.write_all(input.as_bytes()).await?;
        pipe.shutdown().await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() && !allow_failure {
        return Err("Docker acceptance operation failed".into());
    }
    Ok(String::from_utf8(out.stdout)?)
}
fn check(ok: bool, why: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(why.to_owned().into())
    }
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array()
        .ok_or_else(|| "missing observation array".into())
}
struct Gate {
    db: PgConnection,
    client: reqwest::Client,
    token: String,
    run: Option<Uuid>,
    thread: String,
}
impl Gate {
    async fn open() -> Result<Self> {
        let owner = docker(
            &[
                "inspect",
                "ai-gateway-agent-runtime",
                "ai-gateway-agent-capability-worker-1",
                "ai-gateway-backend",
                "--format",
                "{{index .Config.Labels \"com.docker.compose.project.working_dir\"}}",
            ],
            None,
            false,
        )
        .await?;
        let expected = root().to_string_lossy().into_owned();
        check(
            owner.lines().collect::<Vec<_>>() == vec![expected.as_str(); 3],
            "Wrong Compose owner",
        )?;
        let db = connect().await?;
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()?;
        let credentials: Value =
            serde_json::from_slice(&std::fs::read("web/.playwright/e2e-user.json")?)?;
        let response = client
            .post("http://127.0.0.1:8081/api/v1/auth/login")
            .json(&credentials)
            .send()
            .await?;
        check(response.status() == 200, "Dedicated local E2E login failed")?;
        let token = response.json::<Value>().await?["access_token"]
            .as_str()
            .ok_or("missing access token")?
            .to_owned();
        Ok(Self {
            db,
            client,
            token,
            run: None,
            thread: String::new(),
        })
    }
    fn id(&self) -> Result<Uuid> {
        self.run.ok_or_else(|| "original run not admitted".into())
    }
    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let r = self
            .client
            .post(format!("http://127.0.0.1:8081{path}"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        check(
            r.status().as_u16() < 400,
            &format!("Acceptance HTTP request rejected ({})", r.status().as_u16()),
        )?;
        Ok(r.json().await?)
    }
    async fn create(&mut self, title: &str) -> Result<()> {
        self.thread = self
            .post("/api/v2/agent/threads", json!({"model_id":"qwen3.8-flash"}))
            .await?["thread"]["id"]
            .as_str()
            .ok_or("missing thread")?
            .into();
        let params = json!({"title":title,"description":"持久恢复验收","difficulty":"easy","questions":[{"question_num":1,"question_type":"mc_single","question_text":"2+3 等于多少？","options":[{"label":"A","text":"4"},{"label":"B","text":"5"},{"label":"C","text":"6"},{"label":"D","text":"7"}],"correct_answer":["B"],"explanation":"2加3等于5。"}]});
        let message = format!(
            "持久恢复验收：只调用 generate_quiz 一次，使用下面精确JSON参数。不得添加其他字段（尤其不得添加 option）。不调用其他工具；审批拒绝、校验失败或结果未知时说明情况，不能再次申请或重试。JSON={}",
            serde_json::to_string(&params)?
        );
        self.run = Some(
            self.post(
                &format!("/api/v2/agent/threads/{}/turns", self.thread),
                json!({"message":message,"model_id":"qwen3.8-flash"}),
            )
            .await?["turn"]["id"]
                .as_str()
                .ok_or("missing run")?
                .parse()?,
        );
        println!(
            "{}",
            json!({"phase":"original_run_admitted","run_id":self.id()?.to_string(),"title":title})
        );
        Ok(())
    }
    async fn poll(&mut self, kind: &str) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline {
            let id = self.id()?;
            let found=match kind{
 "approval"=>sqlx::query_scalar::<_,Uuid>("SELECT approval_id FROM assistant.assistant_tool_approvals WHERE run_id=$1 AND status='pending' ORDER BY created_at LIMIT 1").bind(id).fetch_optional(&mut self.db).await?.map(|x|json!(x.to_string())),
 "terminal"=>{let s=sqlx::query_scalar::<_,String>("SELECT status FROM assistant.assistant_runs WHERE run_id=$1").bind(id).fetch_optional(&mut self.db).await?;s.filter(|s|TERMINAL.contains(&s.as_str())).map(Value::String)},
 "recovered"|"dispatched"=>{let sql=if kind=="recovered"{"SELECT fence>=2 FROM assistant.assistant_runtime_execution_owners WHERE run_id=$1"}else{"SELECT EXISTS(SELECT 1 FROM assistant.assistant_capability_executions WHERE run_id=$1 AND dispatch_fence IS NOT NULL AND status IN ('dispatched','running') AND result_summary IS NULL)"};sqlx::query_scalar::<_,bool>(sql).bind(id).fetch_optional(&mut self.db).await?.filter(|x|*x).map(Value::Bool)},
 stage=>{let output=docker(&["exec","ai-gateway-agent-runtime","sh","-c","cat \"$AI_PLATFORM_AGENT_HOME/recovery-acceptance-hit.json\""],None,true).await?;serde_json::from_str::<Value>(&output).ok().filter(|v|v["stage"]==stage&&v["run_id"]==id.to_string())}};
            if let Some(v) = found {
                return Ok(v);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!("Acceptance timeout: {kind}").into())
    }
    async fn decide(&self, approval: &str, approved: bool) -> Result<()> {
        self.post(
            &format!(
                "/api/v2/agent/threads/{}/approvals/{approval}/decision",
                self.thread
            ),
            json!({"approved":approved,"reason":"controlled local recovery acceptance"}),
        )
        .await?;
        Ok(())
    }
    async fn arm(&self, stage: &str, title: &str) -> Result<()> {
        docker(
            &[
                "exec",
                "-i",
                "ai-gateway-agent-runtime",
                "sh",
                "-c",
                "umask 077\ncat > \"$AI_PLATFORM_AGENT_HOME/recovery-acceptance-fault.json\"",
            ],
            Some(&serde_json::to_string(
                &json!({"stage":stage,"quiz_title":title}),
            )?),
            false,
        )
        .await?;
        Ok(())
    }
    async fn identity(&mut self) -> Result<Value> {
        let id = self.id()?;
        let r=sqlx::query("SELECT o.runtime_thread_id,o.fence,l.lease_id,l.snapshot_id,l.model_id,s.snapshot::text AS snapshot,o.context::text AS context FROM assistant.assistant_runtime_execution_owners o JOIN assistant.assistant_runtime_model_leases l USING(run_id) JOIN assistant.assistant_runtime_snapshots s USING(snapshot_id) WHERE o.run_id=$1").bind(id).fetch_one(&mut self.db).await?;
        Ok(
            json!({"runtime_thread_id":r.get::<Uuid,_>("runtime_thread_id").to_string(),"lease_id":r.get::<Uuid,_>("lease_id").to_string(),"snapshot_id":r.get::<Uuid,_>("snapshot_id").to_string(),"model_id":r.get::<String,_>("model_id"),"snapshot_hash":format!("{:x}",Sha256::digest(r.get::<String,_>("snapshot").as_bytes())),"context_hash":format!("{:x}",Sha256::digest(r.get::<String,_>("context").as_bytes()))}),
        )
    }
    async fn facts(&mut self, path: &Path) -> Result<Value> {
        let id = self.id()?;
        observe_on(&mut self.db, id, path).await
    }
    async fn restart(&self, worker: bool) -> Result<()> {
        let mut args = vec!["compose", "restart", "-t", "0", "agent-runtime"];
        if worker {
            args.push("agent-capability-worker");
        }
        docker(&args, None, false).await?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if !docker(
                &[
                    "exec",
                    "ai-gateway-agent-runtime",
                    "sh",
                    "-c",
                    "curl -fsS http://127.0.0.1:8094/health/ready",
                ],
                None,
                true,
            )
            .await?
            .is_empty()
            {
                println!(
                    "{}",
                    json!({"phase":"controlled_process_crash","run_id":self.id()?.to_string(),"worker":worker})
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("Acceptance timeout: restarted Runtime readiness".into())
    }
    async fn no_extra(&mut self) -> Result<()> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM assistant.assistant_runs WHERE harness_thread_id=$1",
        )
        .bind(self.thread.parse::<Uuid>()?)
        .fetch_one(&mut self.db)
        .await?;
        check(n == 1, "extra run created")
    }
    async fn duplicate(&mut self) -> Result<()> {
        for _ in 0..2 {
            self.post(
                &format!(
                    "/api/v2/agent/threads/{}/turns/{}:recover",
                    self.thread,
                    self.id()?
                ),
                json!({}),
            )
            .await?;
        }
        self.no_extra().await
    }
    async fn interrupt(&self) -> Result<()> {
        self.post(
            &format!(
                "/api/v2/agent/threads/{}/turns/{}:interrupt",
                self.thread,
                self.id()?
            ),
            json!({}),
        )
        .await?;
        Ok(())
    }
    async fn quizzes(&mut self, title: &str) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM assistant.quizzes WHERE title=$1")
                .bind(title)
                .fetch_one(&mut self.db)
                .await?,
        )
    }
    async fn cleanup(mut self) -> Result<()> {
        let outcome: Result<()> = async {
            if let Some(id) = self.run {
                let status = sqlx::query_scalar::<_, String>(
                    "SELECT status FROM assistant.assistant_runs WHERE run_id=$1",
                )
                .bind(id)
                .fetch_optional(&mut self.db)
                .await?;
                if !status.as_deref().is_some_and(|s| TERMINAL.contains(&s)) {
                    self.interrupt().await?;
                }
            }
            Ok(())
        }
        .await;
        let removal=docker(&["exec","ai-gateway-agent-runtime","sh","-c","rm -f \"$AI_PLATFORM_AGENT_HOME/recovery-acceptance-fault.json\" \"$AI_PLATFORM_AGENT_HOME/recovery-acceptance-hit.json\" \"$AI_PLATFORM_AGENT_HOME/recovery-acceptance-consumed.json\""],None,true).await;
        self.db.close().await?;
        outcome?;
        removal?;
        Ok(())
    }
}
fn before_contract(name: &str, stage: &str, b: &Value) -> Result<()> {
    if STAGES.contains(&name) || ["cancel", "revoke", "reject"].contains(&name) {
        check(
            b["core"]["complete"] == 0 && array(&b["invocations"])?.len() == 1,
            "unexpected pre-crash core/invocations",
        )?;
        if stage == "invocation_saved" {
            check(
                array(&b["approvals"])?.is_empty() && array(&b["executions"])?.is_empty(),
                "invocation pause not isolated",
            )?;
        }
        if ["approval_saved", "before_dispatch"].contains(&stage) {
            check(
                b["approvals"][0]["status"]
                    == if stage == "approval_saved" {
                        "pending"
                    } else {
                        "approved"
                    },
                "approval pause status",
            )?;
            check(array(&b["executions"])?.is_empty(), "premature execution")?;
        }
        if [
            "worker_result_observed",
            "invocation_result_saved",
            "core_output_saved",
        ]
        .contains(&stage)
        {
            check(
                array(&b["executions"])?.len() == 1 && b["executions"][0]["status"] == "succeeded",
                "worker result missing",
            )?;
        }
        check(
            b["invocations"][0]["has_response"]
                == ["invocation_result_saved", "core_output_saved"].contains(&stage),
            "invocation response stage mismatch",
        )?;
        check(
            !b["core"]["outputs"]
                .as_object()
                .ok_or("outputs missing")?
                .is_empty()
                == (stage == "core_output_saved"),
            "core output stage mismatch",
        )?;
    }
    Ok(())
}
async fn scenario(name: &str) -> Result<()> {
    let mut g = Gate::open().await?;
    let title = format!(
        "DR-CRASH-{name}-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let evidence = Path::new("reports/runtime-recovery/cases").join(&title);
    std::fs::create_dir_all(&evidence)?;
    let mut blocker: Option<PgConnection> = None;
    let result:Result<()>=async {let stage=if STAGES.contains(&name){name}else{"approval_saved"};if name!="worker_unknown"{g.arm(stage,&title).await?;}g.create(&title).await?;
 if !["invocation_saved","approval_saved"].contains(&stage)||name=="worker_unknown"{let approval=g.poll("approval").await?;if name=="worker_unknown"{let mut db=connect().await?;sqlx::raw_sql("BEGIN; LOCK TABLE assistant.quizzes IN SHARE MODE").execute(&mut db).await?;blocker=Some(db);}g.decide(approval.as_str().ok_or("approval UUID missing")?,true).await?;}
 g.poll(if name=="worker_unknown"{"dispatched"}else{stage}).await?;let original=g.identity().await?;let before=g.facts(&evidence.join("before-crash.json")).await?;before_contract(name,stage,&before)?;g.restart(name=="worker_unknown").await?;if let Some(db)=blocker.take(){db.close().await?;}
 let after;if ["cancel","revoke"].contains(&name){let expected;if name=="cancel"{g.interrupt().await?;g.interrupt().await?;expected="cancelled";}else{let id=g.id()?;sqlx::query("INSERT INTO assistant.assistant_runtime_snapshot_revocations(snapshot_id,tenant_id,user_id,session_id,reason_code,revoked_by) SELECT snapshot_id,tenant_id,user_id,session_id,'controlled_acceptance_revocation','local-test' FROM assistant.assistant_runtime_snapshots WHERE run_id=$1").bind(id).execute(&mut g.db).await?;expected="failed";}check(g.poll("terminal").await?==expected,"terminal status mismatch")?;g.duplicate().await?;after=g.facts(&evidence.join("after.json")).await?;check(array(&after["executions"])?.is_empty(),"cancel/revoke dispatched execution")?;
 }else{g.poll("recovered").await?;check(g.identity().await?==original,"recovered identity changed")?;g.duplicate().await?;if name!="worker_unknown"&&["invocation_saved","approval_saved"].contains(&stage){let approval=g.poll("approval").await?;if !array(&before["approvals"])?.is_empty(){check(approval==before["approvals"][0]["approval_id"],"approval identity changed")?;}g.decide(approval.as_str().ok_or("approval missing")?,name!="reject").await?;}check(g.poll("terminal").await?=="succeeded","original run did not succeed")?;after=g.facts(&evidence.join("after.json")).await?;
 if name=="reject"{check(array(&after["executions"])?.is_empty(),"rejected execution dispatched")?;}else{check(array(&after["executions"])?.len()==1,"execution not exactly once")?;let e=&after["executions"][0];check(e["status"]==if name=="worker_unknown"{"side_effect_unknown"}else{"succeeded"},"execution terminal mismatch")?;check(e["attempt_id"]==e["tool_call_id"],"attempt identity changed")?;if !array(&before["executions"])?.is_empty(){check(e["execution_id"]==before["executions"][0]["execution_id"],"execution identity changed")?;}for key in ["calls","outputs"]{let values=after["core"][key].as_object().ok_or("core evidence missing")?;check(values.len()==1&&values.values().all(|x|x==1),"core call/output not exactly once")?;}check(g.quizzes(&title).await?==if name=="worker_unknown"{0}else{1},"side effect count mismatch")?;}g.duplicate().await?;}
 g.no_extra().await?;check(g.identity().await?==original,"original identity changed")?;check(array(&after["invocations"])?.len()==1&&array(&after["approvals"])?.len()==1,"invocation/approval count mismatch")?;if !array(&before["approvals"])?.is_empty(){check(after["approvals"][0]["approval_id"]==before["approvals"][0]["approval_id"],"approval replaced")?;}check(after["core"]["aborted"]==0,"core aborted")?;if !["cancel","revoke"].contains(&name){check(after["core"]["complete"]==1,"core completion missing")?;check(after["core"]["calls"].as_object().ok_or("calls missing")?.keys().collect::<Vec<_>>()==after["core"]["outputs"].as_object().ok_or("outputs missing")?.keys().collect::<Vec<_>>(),"core calls outputs unpaired")?;}if ["cancel","revoke","reject"].contains(&name){check(g.quizzes(&title).await?==0,"denied side effect occurred")?;}
 write(&evidence.join("result.json"),&json!({"scenario":name,"result":"PASS","evidence_kind":"real Qwen + controlled dev pause / SIGKILL or DB lock injection","original_identity":original,"run_id":g.id()?.to_string()}))?;println!("{}",json!({"scenario":name,"result":"PASS","run_id":g.id()?.to_string()}));Ok(())}.await;
    let receipt = if result.is_err() {
        write(
            &evidence.join("result.json"),
            &json!({"scenario":name,"result":"FAILED","failure_type":"NativeRecoveryError","run_id":g.run.map(|x|x.to_string()),"evidence_kind":"real Qwen + controlled dev pause / SIGKILL or DB lock injection"}),
        )
    } else {
        Ok(())
    };
    let blocker_cleanup = if let Some(db) = blocker {
        db.close().await
    } else {
        Ok(())
    };
    let cleanup = g.cleanup().await;
    result?;
    receipt?;
    blocker_cleanup?;
    cleanup
}
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("Controlled recovery acceptance failed; see redacted case result");
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] || args == ["-h"] {
        println!(
            "runtime_durable_recovery_gate --scenario {}",
            STAGES
                .into_iter()
                .chain(["cancel", "revoke", "reject", "worker_unknown"])
                .collect::<Vec<_>>()
                .join("|")
        );
        return Ok(());
    }
    check(
        args.len() == 2 && args[0] == "--scenario",
        "--scenario is required",
    )?;
    let name = &args[1];
    check(
        STAGES.contains(&name.as_str())
            || ["cancel", "revoke", "reject", "worker_unknown"].contains(&name.as_str()),
        "unknown scenario",
    )?;
    scenario(name).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invocation_pause_refuses_early_approval() {
        let b = json!({"core":{"complete":0,"outputs":{}},"invocations":[{"has_response":false}],"approvals":[],"executions":[]});
        assert!(before_contract("invocation_saved", "invocation_saved", &b).is_ok());
        let mut b = b;
        b["approvals"] = json!([{}]);
        assert!(before_contract("invocation_saved", "invocation_saved", &b).is_err());
    }
}
