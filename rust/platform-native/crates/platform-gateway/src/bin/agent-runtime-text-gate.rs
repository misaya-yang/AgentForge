//! Opt-in real-provider acceptance harness. No work occurs during compilation/tests.
//! Uses the production Rust control/model planes with isolated PostgreSQL/Runtime.
use ai_platform_gateway::migration_agent::{
    Actor, AgentError, AgentFuture, Scope,
    control::{
        RuntimeCatalog, RuntimeControlPlane, RuntimeModels, RuntimeOptions, RuntimeProviders,
        TurnRequest,
    },
    entry::{PgSourceAccess, SourceAuthority, SourceRefs},
    model::{AgentModelPlane, LeaseSigner, RuntimeProviderCredentials},
};
use serde_json::{Value, json};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const KERNEL: &str = "agent-runtime-text-gate";
const REVISION: &str = "qwen-responses-live-v1";
fn check(ok: bool, message: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn docker(args: &[String]) -> Result<String> {
    let output = Command::new("docker").args(args).output()?;
    check(output.status.success(), "isolated Docker operation failed")?;
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}
fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
#[derive(Clone)]
struct Provider {
    key: String,
    base: String,
}
impl RuntimeProviders for Provider {
    fn provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            if tenant.is_empty() || id != "dashscope" {
                return Err(AgentError::code(400, "GATE_PROVIDER_SCOPE_INVALID"));
            }
            Ok(
                json!({"provider_id":id,"api_type":"openai","is_enabled":true,"updated_at":REVISION}),
            )
        })
    }
    fn runtime_config<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        self.runtime_provider(tenant, id)
    }
}
impl RuntimeProviderCredentials for Provider {
    fn runtime_provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            if tenant.is_empty() || id != "dashscope" {
                return Err(AgentError::code(400, "GATE_PROVIDER_SCOPE_INVALID"));
            }
            Ok(json!({"updated_at":REVISION,"api_key":self.key,"runtime_base_url":self.base}))
        })
    }
}
struct Models;
impl RuntimeModels for Models {
    fn model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        _: Option<&'a str>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            if tenant.is_empty() || model != "qwen3.7-plus" {
                return Err(AgentError::code(400, "GATE_MODEL_SCOPE_INVALID"));
            }
            let profile = ai_platform_gateway_foundation::capabilities::builtin("dashscope", model)
                .ok_or_else(|| AgentError::code(503, "GATE_MODEL_PROFILE_MISSING"))?;
            if profile["wire_protocols"]["preferred"] != "responses_v1" {
                return Err(AgentError::code(503, "GATE_RESPONSES_PROFILE_REQUIRED"));
            }
            Ok(
                json!({"model_id":model,"provider_id":"dashscope","is_enabled":true,"effective_capabilities":profile,"capability_revision":1,"context_window":1000000,"max_output_tokens":65536,"input_price_per_1k":0.001,"output_price_per_1k":0.002}),
            )
        })
    }
}
struct NoTools;
impl RuntimeCatalog for NoTools {
    fn fetch<'a>(
        &'a self,
        _: &'a Scope,
        _: &'a str,
        _: i64,
        _: Option<&'a [Value]>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async { Err(AgentError::code(403, "TEXT_GATE_TOOLS_FORBIDDEN")) })
    }
}
// All source authority calls deny actual references. The pure text scenario has none.
struct NoSources;
impl SourceAuthority for NoSources {
    fn allowed<'a>(&'a self, _: &'a Scope, refs: &'a SourceRefs) -> AgentFuture<'a, bool> {
        Box::pin(async move {
            Ok(refs.datasets.is_empty() && refs.documents.is_empty() && refs.versions.is_empty())
        })
    }
}
struct Config {
    root: PathBuf,
    image: String,
    provider: Provider,
    ttft: f64,
    long_min: usize,
}
impl Config {
    fn load() -> Result<Self> {
        let mut args = BTreeMap::new();
        let mut iter = std::env::args().skip(1);
        while let Some(key) = iter.next() {
            if key == "--help" {
                println!(
                    "agent-runtime-text-gate [--repo-root ROOT] [--env-file FILE] [--runtime-image IMAGE]\nStarts isolated containers and makes real provider calls only when explicitly invoked."
                );
                std::process::exit(0);
            }
            check(
                matches!(
                    key.as_str(),
                    "--repo-root" | "--env-file" | "--runtime-image"
                ),
                "unknown text gate argument",
            )?;
            args.insert(key, iter.next().ok_or("argument value missing")?);
        }
        let root =
            std::fs::canonicalize(args.get("--repo-root").map(String::as_str).unwrap_or("."))?;
        let path = args
            .get("--env-file")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".env"));
        let mut values = BTreeMap::new();
        if path.is_file() {
            for pair in dotenvy::from_path_iter(path)? {
                let (k, v) = pair?;
                values.insert(k, v);
            }
        }
        let value = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| values.get(key).cloned())
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let first = |a: &str, b: &str| {
            let v = value(a);
            if v.is_empty() { value(b) } else { v }
        };
        let key = first("DASHSCOPE_CHAT_API_KEY", "DASHSCOPE_API_KEY");
        check(!key.is_empty(), "local DashScope credential required")?;
        let mut base = first("DASHSCOPE_CHAT_BASE_URL", "DASHSCOPE_BASE_URL");
        if base.is_empty() {
            base = "https://dashscope.aliyuncs.com/compatible-mode".into();
        } else {
            base = base.trim_end_matches('/').into();
            for suffix in [
                "/compatible-mode/v1/chat/completions",
                "/compatible-mode/v1",
                "/compatible-mode",
                "/api/v1",
            ] {
                if let Some(prefix) = base.strip_suffix(suffix) {
                    base = prefix.into();
                    break;
                }
            }
            base.push_str("/compatible-mode");
        }
        let image = args
            .get("--runtime-image")
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| value("AI_PLATFORM_AGENT_RUNTIME_IMAGE"));
        check(
            !image.is_empty(),
            "AI_PLATFORM_AGENT_RUNTIME_IMAGE required",
        )?;
        for (label, expected) in [
            ("artifact", "agent_runtime"),
            ("binary", "ai-platform-agent-runtime"),
        ] {
            let format = format!(
                "{{{{index .Config.Labels \"com.misaya.ai-platform.agent-runtime.{label}\"}}}}"
            );
            check(
                docker(&strings(&["image", "inspect", &image, "--format", &format]))? == expected,
                "runtime image identity invalid",
            )?;
        }
        let ttft = value("AI_PLATFORM_AGENT_RUNTIME_TEXT_TTFT_LIMIT_SECONDS");
        let ttft = if ttft.is_empty() {
            8.0
        } else {
            ttft.parse::<f64>()?
        };
        let min = value("AI_PLATFORM_AGENT_RUNTIME_LONG_OUTPUT_MIN_CHARS");
        let long_min = if min.is_empty() {
            700
        } else {
            min.parse::<usize>()?
        };
        check(
            ttft.is_finite() && ttft > 0.0 && (200..=5000).contains(&long_min),
            "text gate limits invalid",
        )?;
        Ok(Self {
            root,
            image,
            provider: Provider { key, base },
            ttft,
            long_min,
        })
    }
}
struct Gate {
    config: Config,
    network: String,
    postgres: String,
    runtime: String,
    password: String,
    internal: String,
    model_token: String,
    lease_secret: String,
    actor: Actor,
    session: String,
    pg_port: u16,
    runtime_port: u16,
    model_port: u16,
    pool: Option<PgPool>,
    control: Option<RuntimeControlPlane>,
    server: Option<tokio::task::JoinHandle<()>>,
    thread: Option<String>,
}
impl Drop for Gate {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
        for name in [&self.runtime, &self.postgres] {
            let _ = Command::new("docker")
                .args(["rm", "--force", name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = Command::new("docker")
            .args(["network", "rm", &self.network])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
impl Gate {
    fn new(config: Config) -> Result<Self> {
        let suffix = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
        Ok(Self {
            config,
            network: format!("agent-runtime-text-gate-{suffix}"),
            postgres: format!("agent-runtime-text-gate-pg-{suffix}"),
            runtime: format!("agent-runtime-text-gate-runtime-{suffix}"),
            password: secret(),
            internal: secret(),
            model_token: secret(),
            lease_secret: secret(),
            actor: Actor {
                tenant_id: format!("tenant-text-gate-{suffix}"),
                user_id: format!("user-text-gate-{suffix}"),
                ..Default::default()
            },
            session: format!("session-text-gate-{suffix}"),
            pg_port: port()?,
            runtime_port: port()?,
            model_port: port()?,
            pool: None,
            control: None,
            server: None,
            thread: None,
        })
    }
    async fn start(&mut self) -> Result<()> {
        docker(&strings(&["network", "create", &self.network]))?;
        docker(&strings(&[
            "run",
            "--detach",
            "--rm",
            "--name",
            &self.postgres,
            "--network",
            &self.network,
            "--publish",
            &format!("127.0.0.1:{}:5432", self.pg_port),
            "--env",
            "POSTGRES_USER=runtime_text_gate",
            "--env",
            &format!("POSTGRES_PASSWORD={}", self.password),
            "--env",
            "POSTGRES_DB=runtime_text_gate",
            "postgres:16-alpine",
        ]))?;
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(self.pg_port)
            .username("runtime_text_gate")
            .password(&self.password)
            .database("runtime_text_gate");
        let mut pool = None;
        for _ in 0..80 {
            if let Ok(Ok(p)) = tokio::time::timeout(
                Duration::from_secs(1),
                PgPoolOptions::new()
                    .min_connections(1)
                    .max_connections(8)
                    .connect_with(options.clone()),
            )
            .await
            {
                pool = Some(p);
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let pool = pool.ok_or("isolated PostgreSQL readiness timed out")?;
        for file in [
            "deploy/agent-runtime-source/runtime-smoke-base.sql",
            "database/migrations/089_agent_runtime_thread_store.sql",
            "database/migrations/090_agent_runtime_model_leases.sql",
            "database/migrations/092_agent_runtime_legacy_import.sql",
        ] {
            sqlx::raw_sql(&std::fs::read_to_string(self.config.root.join(file))?)
                .execute(&pool)
                .await?;
        }
        // The native production reader uses the canonical assistant schema. The old
        // isolated fixture intentionally supplied public.sessions; expose that same table.
        sqlx::raw_sql("CREATE SCHEMA assistant; CREATE VIEW assistant.sessions AS SELECT * FROM public.sessions;").execute(&pool).await?;
        sqlx::query("INSERT INTO sessions(session_id,service_id,user_id,tenant_id) VALUES($1,'__builtin_assistant__',$2,$3)").bind(&self.session).bind(&self.actor.user_id).bind(&self.actor.tenant_id).execute(&pool).await?;
        sqlx::query("INSERT INTO assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner,kernel_revision,assignment_reason) VALUES($1,$2,$3,'agent_runtime',$4,'live_gate')").bind(&self.actor.tenant_id).bind(&self.actor.user_id).bind(&self.session).bind(KERNEL).execute(&pool).await?;
        let provider = Arc::new(self.config.provider.clone());
        let signer = Arc::new(LeaseSigner::new(self.lease_secret.clone())?);
        let sources = Arc::new(PgSourceAccess {
            pool: pool.clone(),
            authority: Arc::new(NoSources),
        });
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(300))
            .build()?;
        let plane = Arc::new(AgentModelPlane {
            context_metrics: None,
            pool: pool.clone(),
            providers: provider.clone(),
            signer: signer.clone(),
            sources: sources.clone(),
            client,
            internal_token: Arc::new(self.model_token.clone()),
            admission: None,
            capacity: None,
        });
        let router = ai_platform_gateway::migration_agent::model::router(plane).route(
            "/healthz",
            axum::routing::get(|| async { axum::Json(json!({"status":"ok"})) }),
        );
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", self.model_port)).await?;
        self.server = Some(tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        }));
        self.control = Some(RuntimeControlPlane::new(
            pool.clone(),
            Arc::new(Models),
            provider,
            Arc::new(NoTools),
            sources,
            signer,
            RuntimeOptions {
                runtime_url: format!("http://127.0.0.1:{}", self.runtime_port),
                runtime_internal_token: self.internal.clone(),
                model_plane_base_url: format!(
                    "http://host.docker.internal:{}/internal/v1/agent-model-plane",
                    self.model_port
                ),
                kernel_revision: KERNEL.into(),
                lease_ttl_seconds: 900,
                max_model_calls: 1,
                max_cost_microusd: 1_000_000,
                worker_writes_enabled: false,
            },
        )?);
        self.pool = Some(pool);
        self.start_runtime()?;
        self.ready().await
    }
    fn start_runtime(&self) -> Result<()> {
        docker(&strings(&[
            "run",
            "--detach",
            "--name",
            &self.runtime,
            "--network",
            &self.network,
            "--publish",
            &format!("127.0.0.1:{}:8094", self.runtime_port),
            "--memory",
            "256m",
            "--pids-limit",
            "256",
            "--tmpfs",
            "/var/lib/ai-platform-agent-runtime/runtime-home:rw,uid=10001,gid=10001,mode=0700",
            "--env",
            &format!("AI_PLATFORM_INTERNAL_TOKEN={}", self.internal),
            "--env",
            "AI_PLATFORM_RUNTIME_BIND=0.0.0.0:8094",
            "--env",
            "AI_PLATFORM_RUNTIME_WORKDIR=/workspace",
            "--env",
            "AI_PLATFORM_AGENT_HOME=/var/lib/ai-platform-agent-runtime/runtime-home",
            "--env",
            &format!(
                "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN={}",
                self.model_token
            ),
            "--env",
            &format!("POSTGRES_HOST={}", self.postgres),
            "--env",
            "POSTGRES_PORT=5432",
            "--env",
            "POSTGRES_USER=runtime_text_gate",
            "--env",
            &format!("POSTGRES_PASSWORD={}", self.password),
            "--env",
            "POSTGRES_DB=runtime_text_gate",
            &self.config.image,
        ]))?;
        Ok(())
    }
    async fn ready(&self) -> Result<()> {
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()?;
        let mut ready = false;
        for _ in 0..120 {
            if client
                .get(format!(
                    "http://127.0.0.1:{}/health/ready",
                    self.runtime_port
                ))
                .send()
                .await
                .is_ok_and(|r| r.status() == 200)
            {
                ready = true;
                break;
            }
            check(
                docker(&strings(&[
                    "inspect",
                    &self.runtime,
                    "--format",
                    "{{.State.Running}}",
                ]))? == "true",
                "Runtime exited before readiness",
            )?;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        check(ready, "Runtime readiness timed out")?;
        for _ in 0..20 {
            if docker(&strings(&[
                "exec",
                &self.runtime,
                "curl",
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "2",
                &format!("http://host.docker.internal:{}/healthz", self.model_port),
            ]))
            .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err("Runtime cannot reach private model plane".into())
    }
    async fn restart(&mut self) -> Result<()> {
        let thread = self.thread.clone().ok_or("cannot resume before Thread")?;
        docker(&strings(&["rm", "--force", &self.runtime]))?;
        self.start_runtime()?;
        self.ready().await?;
        let control = self.control.as_ref().ok_or("control missing")?;
        let response=control.client.post(format!("http://127.0.0.1:{}/internal/v1/threads/{thread}/resume",self.runtime_port)).headers(control.headers(&self.actor.scope(&self.session),None)?).json(&json!({"model":"qwen3.7-plus","modelPlaneBaseUrl":format!("http://host.docker.internal:{}/internal/v1/agent-model-plane",self.model_port)})).send().await?;
        check(response.status() == 200, "Runtime resume failed")?;
        let body: Value = response.json().await?;
        check(
            body["thread"]["id"] == thread,
            "Runtime resumed different Thread",
        )
    }
    async fn turn(&mut self, label: &str, message: String, max: i64) -> Result<Receipt> {
        let started = Instant::now();
        let control = self.control.as_ref().ok_or("control missing")?;
        let turn = control
            .start_turn(TurnRequest {
                actor: self.actor.clone(),
                session_id: self.session.clone(),
                message,
                model_id: "qwen3.7-plus".into(),
                reasoning_option: Some("auto".into()),
                max_tokens: Some(max),
                enable_dynamic_tools: false,
                readonly_capabilities: json!({}),
                ..Default::default()
            })
            .await?;
        self.thread = Some(turn.runtime_thread_id.clone());
        let mut events = control
            .stream_thread_events(
                self.actor.scope(&self.session),
                turn.runtime_thread_id.clone(),
                turn.after_sequence,
                1000,
                Some(turn.run_id.clone()),
            )
            .await?;
        let mut first = None;
        let mut text = String::new();
        let mut kinds = Vec::new();
        let mut sequences = Vec::new();
        let mut terminals = 0;
        let mut status = String::new();
        while let Some(event) = events.recv().await {
            let event = event?;
            if event["data"]["run_id"] != turn.run_id {
                continue;
            }
            let kind = event["event_type"].as_str().unwrap_or("").to_owned();
            kinds.push(kind.clone());
            if let Some(sequence) = event["sequence"].as_i64() {
                sequences.push(sequence);
            }
            if matches!(kind.as_str(), "thinking_delta" | "text_delta") && first.is_none() {
                first = Some(started.elapsed().as_secs_f64());
            }
            if kind == "text_delta" {
                if let Some(part) = event["data"]["content"].as_str() {
                    text.push_str(part);
                }
            }
            if matches!(kind.as_str(), "run_finished" | "run_error") {
                terminals += 1;
                status = event["data"]["status"]
                    .as_str()
                    .unwrap_or("failed")
                    .to_owned();
            }
        }
        check(
            terminals == 1 && kinds.iter().any(|k| k == "run_started"),
            "turn start/terminal contract violated",
        )?;
        check(
            sequences.windows(2).all(|p| p[0] < p[1]),
            "event sequences duplicate/out of order",
        )?;
        check(status == "succeeded", "turn did not succeed")?;
        let first = first.ok_or("no visible reasoning/text")?;
        check(
            first <= self.config.ttft,
            "first-visible-token limit exceeded",
        )?;
        let text = text.trim().to_owned();
        check(!text.is_empty(), "turn completed without text")?;
        let pool = self.pool.as_ref().ok_or("pool missing")?;
        let run = uuid::Uuid::parse_str(&turn.run_id)?;
        let accounting=sqlx::query("SELECT COUNT(*)::int AS model_calls,COUNT(*) FILTER(WHERE status='completed')::int AS completed_calls,COALESCE(SUM(input_tokens),0)::bigint AS input_tokens,COALESCE(SUM(output_tokens),0)::bigint AS output_tokens,COUNT(*) FILTER(WHERE status IN ('failed','unknown'))::int AS bad_calls FROM assistant_runtime_model_calls WHERE run_id=$1").bind(run).fetch_one(pool).await?;
        let stored = sqlx::query("SELECT status,engine FROM assistant_runs WHERE run_id=$1")
            .bind(run)
            .fetch_one(pool)
            .await?;
        check(
            accounting.get::<i32, _>("model_calls") == 1
                && accounting.get::<i32, _>("completed_calls") == 1
                && accounting.get::<i32, _>("bad_calls") == 0
                && stored.get::<String, _>("status") == "succeeded"
                && stored.get::<String, _>("engine") == "agent_runtime",
            "turn did not close exactly one Agent billing path",
        )?;
        Ok(Receipt {
            label: label.into(),
            text,
            first,
            input: accounting.get("input_tokens"),
            output: accounting.get("output_tokens"),
        })
    }
}
struct Receipt {
    label: String,
    text: String,
    first: f64,
    input: i64,
    output: i64,
}
async fn run(config: Config) -> Result<()> {
    let mut gate = Gate::new(config)?;
    gate.start().await?;
    let simple = gate
        .turn(
            "simple",
            "只回复 EXACT_200，不要调用工具，也不要添加其他内容。".into(),
            256,
        )
        .await?;
    check(simple.text == "EXACT_200", "exact output mismatch")?;
    let long=gate.turn("long","请直接用中文写一篇约800字的说明，向有编程基础但不了解深度学习的人解释Transformer。必须包含注意力、位置编码、训练和推理四部分；不要调用任何工具。".into(),2048).await?;
    let chars = long.text.chars().filter(|c| !c.is_whitespace()).count();
    check(chars >= gate.config.long_min, "long output too short")?;
    let codeword = format!(
        "AZURE-{}",
        uuid::Uuid::new_v4().simple().to_string()[..8].to_uppercase()
    );
    let remember = gate
        .turn(
            "remember",
            format!("请记住口令 {codeword}，只回复“已记住”，不要调用工具。"),
            256,
        )
        .await?;
    check(
        remember.text.contains("已记住"),
        "multi-turn setup not acknowledged",
    )?;
    gate.restart().await?;
    let resume = gate
        .turn(
            "resume",
            "刚才让我记住的口令是什么？只回复口令本身，不要调用工具。".into(),
            256,
        )
        .await?;
    check(
        resume.text.contains(&codeword),
        "resumed Thread lost context",
    )?;
    let receipts = [simple, long, remember, resume];
    let memory = docker(&strings(&[
        "exec",
        &gate.runtime,
        "sh",
        "-c",
        "cat /sys/fs/cgroup/memory.current",
    ]))?
    .parse::<u64>()?;
    println!(
        "AI_PLATFORM_AGENT_RUNTIME_TEXT_GATE_OK\nscenarios=simple,long,remember,resume\nfirst_visible_seconds={}\nlong_output_chars={chars}\nmodel_calls=4\nmodel_tokens={}/{}\nruntime_memory_bytes={memory}",
        receipts
            .iter()
            .map(|r| format!("{}:{:.3}", r.label, r.first))
            .collect::<Vec<_>>()
            .join(","),
        receipts.iter().map(|r| r.input).sum::<i64>(),
        receipts.iter().map(|r| r.output).sum::<i64>()
    );
    if let Some(pool) = gate.pool.take() {
        pool.close().await;
    }
    Ok(())
}
#[tokio::main]
async fn main() {
    let result = match Config::load() {
        Ok(config) => run(config).await,
        Err(error) => Err(error),
    };
    if result.is_err() {
        eprintln!("ERROR: isolated native text gate failed; no acceptance receipt emitted");
        std::process::exit(1);
    }
}
