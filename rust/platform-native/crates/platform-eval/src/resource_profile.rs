//! Named-concurrency resource collector. No implicit invocation during migration.
use crate::{
    Result,
    parity_benchmark::{AIPlatformAdapter, AdapterConfig},
    resource_stats::*,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const PROMPT: &str = "只回答数字：2+2等于多少？";
fn need(b: bool, s: &str) -> Result<()> {
    if b { Ok(()) } else { Err(s.to_string().into()) }
}
#[derive(Clone)]
pub struct Config {
    pub root: PathBuf,
    pub env_path: PathBuf,
    pub base: String,
    pub output: PathBuf,
    pub levels: Vec<usize>,
    pub max_calls: usize,
    pub wall_minutes: f64,
    pub health: String,
    pub stagger: f64,
    pub settle: f64,
    pub quiet: f64,
    pub idle: f64,
    pub interval: f64,
    pub prompt: String,
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
async fn docker(args: &[String]) -> Result<String> {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(args).kill_on_drop(true);
    let o = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| "docker_cli_timeout")?
        .map_err(|_| "docker_cli_failed")?;
    need(o.status.success(), "docker_cli_failed")?;
    Ok(String::from_utf8(o.stdout)?)
}
async fn inspect() -> Result<Value> {
    let args = std::iter::once("inspect".to_string())
        .chain(CONTAINERS.iter().map(|s| s.to_string()))
        .collect::<Vec<_>>();
    let v: Value = serde_json::from_str(&docker(&args).await?)?;
    let mut result = json!({});
    for e in v.as_array().ok_or("docker_inspect_unexpected_shape")? {
        if !e.is_object() {
            continue;
        }
        let name = e["Name"].as_str().unwrap_or("").trim_start_matches('/');
        let memory = finite(&e["HostConfig"]["Memory"]);
        let restart = e["RestartCount"]
            .as_i64()
            .or_else(|| e["State"]["RestartCount"].as_i64());
        result[name] = json!({"container_id":e["Id"],"image_tag":e["Config"]["Image"],"image_id":e["Image"],"created":e["Created"],"mem_limit_bytes":e["HostConfig"]["Memory"],"mem_limit_mib":memory.filter(|n|*n!=0.).map(|n|round(n/1048576.,4)),"restart_count":restart,"compose_working_dir":e["Config"]["Labels"][OWNERSHIP]});
    }
    need(
        CONTAINERS.iter().all(|n| result.get(*n).is_some()),
        "docker_inspect_missing_containers",
    )?;
    Ok(result)
}
async fn stats() -> Result<Value> {
    let args = ["stats", "--no-stream", "--format", "json"]
        .into_iter()
        .chain(CONTAINERS.iter().copied())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let raw = docker(&args).await?;
    let mut out = json!({});
    for line in raw.lines().filter(|s| !s.trim().is_empty()) {
        let v: Value = serde_json::from_str(line)?;
        let name = v["Name"].as_str().unwrap_or("").trim_start_matches('/');
        let mem = v["MemUsage"].as_str().unwrap_or("");
        let net = v["NetIO"].as_str().unwrap_or("");
        let (m, l) = mem.split_once('/').unwrap_or((mem, ""));
        let (rx, tx) = net.split_once('/').unwrap_or((net, ""));
        out[name] = json!({"cpu_pct":percent(&v["CPUPerc"]),"mem_used_mib":human_mib(m),"mem_limit_mib_stats":human_mib(l),"mem_pct":percent(&v["MemPerc"]),"net_in_mib":human_mib(rx),"net_out_mib":human_mib(tx),"pids":v["PIDs"]});
    }
    Ok(out)
}
async fn health(url: &str, runtime: bool) -> bool {
    let c = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    for i in 0..5 {
        let okay = match c.get(url).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        };
        if okay {
            return true;
        }
        if runtime {
            let args = [
                "exec",
                "ai-gateway-agent-runtime",
                "curl",
                "-f",
                "http://localhost:8094/health/ready",
            ]
            .map(str::to_string);
            if docker(&args).await.is_ok() {
                return true;
            }
        }
        if i < 4 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    false
}
#[derive(Default)]
struct Samples {
    phase: String,
    samples: Vec<Value>,
    violation: Option<Value>,
    error: Option<String>,
}
struct State {
    start: Instant,
    calls: usize,
    max: usize,
    wall: f64,
    incomplete: Vec<String>,
    abort: Option<String>,
    interrupted: Arc<AtomicBool>,
    samples: Arc<Mutex<Samples>>,
}
impl State {
    fn check(&mut self) -> bool {
        let s = self.samples.lock().unwrap();
        if s.violation.is_some() {
            self.abort = Some("mem_limit_90pct".into());
        }
        if s.error.is_some() && self.abort.is_none() {
            self.abort = Some("sampler_docker_error".into());
        }
        drop(s);
        if self.interrupted.load(Ordering::Relaxed) {
            self.abort.get_or_insert("interrupted".into());
        }
        if self.start.elapsed().as_secs_f64() > self.wall
            && !self
                .incomplete
                .contains(&"wall_clock_cap_exceeded".to_string())
        {
            self.incomplete.push("wall_clock_cap_exceeded".into());
        }
        self.abort.is_none() && self.incomplete.is_empty()
    }
    fn issue(&mut self) -> bool {
        if !self.check() {
            return false;
        }
        if self.calls >= self.max {
            if !self.incomplete.contains(&"max_calls_exhausted".to_string()) {
                self.incomplete.push("max_calls_exhausted".into());
            }
            return false;
        }
        self.calls += 1;
        true
    }
    async fn sleep(&mut self, secs: f64) {
        let end = Instant::now() + Duration::from_secs_f64(secs.max(0.));
        while Instant::now() < end && self.check() {
            tokio::time::sleep(
                Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }
    fn phase(&self, p: &str) {
        self.samples.lock().unwrap().phase = p.into();
    }
}
async fn collect(
    samples: Arc<Mutex<Samples>>,
    stop: Arc<AtomicBool>,
    containers: Value,
    path: PathBuf,
    start: Instant,
    interval: f64,
) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path)?;
    while !stop.load(Ordering::Relaxed) {
        let cycle = Instant::now();
        let rows = match stats().await {
            Ok(v) => v,
            Err(e) => {
                samples.lock().unwrap().error = Some(e.to_string());
                return Ok(());
            }
        };
        if !CONTAINERS.iter().all(|n| rows.get(*n).is_some()) {
            samples.lock().unwrap().error = Some("docker_stats_missing_containers".into());
            return Ok(());
        }
        {
            let mut s = samples.lock().unwrap();
            let record = json!({"ts":now(),"elapsed_seconds":round(start.elapsed().as_secs_f64(),3),"phase":s.phase,"containers":rows});
            s.samples.push(record.clone());
            writeln!(file, "{}", serde_json::to_string(&record)?)?;
            file.flush()?;
            for n in CONTAINERS {
                if let Some((limit, used)) =
                    finite(&containers[*n]["mem_limit_mib"]).zip(finite(&rows[*n]["mem_used_mib"]))
                {
                    if limit != 0. && used > limit * 0.9 {
                        s.violation = Some(
                            json!({"container":n,"mem_used_mib":used,"mem_limit_mib":limit,"threshold_fraction":0.9,"phase":s.phase,"elapsed_seconds":record["elapsed_seconds"],"ts":record["ts"]}),
                        );
                        return Ok(());
                    }
                }
            }
        }
        let wait = (interval - cycle.elapsed().as_secs_f64()).max(0.);
        let end = Instant::now() + Duration::from_secs_f64(wait);
        while !stop.load(Ordering::Relaxed) && Instant::now() < end {
            tokio::time::sleep(
                Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }
    Ok(())
}
async fn stream(
    mut adapter: AIPlatformAdapter,
    task: String,
    prompt: String,
    level: Value,
    ordinal: usize,
) -> Value {
    let result = async {
        adapter.start_task(&task).await?;
        adapter.run_turn(&task, &prompt).await
    }
    .await;
    match result {
        Ok(r) => {
            let success = r.terminal_status == "succeeded" && !r.text.is_empty();
            let mut v = json!({"level":level,"ordinal":ordinal,"classification":if success{"success"}else{"stream_error"},"terminal_status":r.terminal_status,"total_seconds":round(r.duration_seconds,6),"first_event_seconds":r.metadata["timing"]["phases"][0]["first_event_seconds"]});
            if !success {
                v["reason"] = format!("gateway_terminal_status:{}", r.terminal_status).into();
            }
            v
        }
        Err(e) => {
            json!({"level":level,"ordinal":ordinal,"classification":classify(&e.to_string()),"total_seconds":null,"first_event_seconds":null,"reason":e.to_string().chars().take(300).collect::<String>()})
        }
    }
}
async fn level(
    state: &mut State,
    adapter: &AIPlatformAdapter,
    k: usize,
    ids: &mut VecDeque<String>,
    config: &Config,
) -> Value {
    let start = Instant::now();
    let target = (2 * k).max(10);
    let mut issued = 0;
    let mut maximum = 0;
    let mut records = vec![];
    let mut tasks = tokio::task::JoinSet::new();
    state.phase(&format!("ramp:{k}"));
    for position in 0..k.min(target) {
        if !state.issue() {
            break;
        }
        issued += 1;
        tasks.spawn(stream(
            adapter.clone(),
            ids.pop_front().unwrap(),
            config.prompt.clone(),
            json!(k),
            issued,
        ));
        maximum = maximum.max(tasks.len());
        if position + 1 < k.min(target) {
            state.sleep(config.stagger).await;
        }
    }
    state.phase(&format!("steady:{k}"));
    let mut first = None;
    while !tasks.is_empty() {
        state.check();
        match tokio::time::timeout(Duration::from_millis(250), tasks.join_next()).await {
            Ok(Some(Ok(r))) => {
                if first.is_none() {
                    first = Some(round(start.elapsed().as_secs_f64(), 3));
                }
                records.push(r);
                if issued < target && state.issue() {
                    issued += 1;
                    tasks.spawn(stream(
                        adapter.clone(),
                        ids.pop_front().unwrap(),
                        config.prompt.clone(),
                        json!(k),
                        issued,
                    ));
                    maximum = maximum.max(tasks.len());
                }
            }
            Ok(Some(Err(_))) => {
                state.abort = Some("stream_task_failed".into());
            }
            _ => {}
        }
    }
    let mut classes: serde_json::Map<String, Value> =
        CLASSES.iter().map(|s| (s.to_string(), json!(0))).collect();
    for r in &records {
        let key = r["classification"]
            .as_str()
            .unwrap_or("infrastructure_error");
        classes[key] = json!(classes[key].as_u64().unwrap_or(0) + 1);
    }
    let totals = records
        .iter()
        .filter_map(|r| finite(&r["total_seconds"]))
        .collect::<Vec<_>>();
    let firsts = records
        .iter()
        .filter_map(|r| finite(&r["first_event_seconds"]))
        .collect::<Vec<_>>();
    let seconds = start.elapsed().as_secs_f64();
    json!({"k":k,"target_streams":target,"issued":issued,"returned_streams":records.len(),"successful_streams":records.iter().filter(|r|r["classification"]=="success").count(),"max_concurrent_in_flight":maximum,"in_flight_hard_cap":3*k,"level_seconds":round(seconds,3),"steady_started_after_seconds":first,"completed_streams_per_second":if seconds>0.{Some(round(records.len()as f64/seconds,4))}else{None},"error_classes":classes,"total_seconds":{"p50":percentile(&totals,0.5),"p95":percentile(&totals,0.95)},"time_to_first_event_seconds":{"p50":percentile(&firsts,0.5),"p95":percentile(&firsts,0.95)},"streams":records})
}
pub async fn run(config: Config) -> Result<Value> {
    need(!config.output.exists(), "ppr00_output_exists")?;
    need(
        !config.levels.is_empty() && config.levels.iter().all(|k| *k > 0),
        "ppr00_levels_invalid",
    )?;
    need(config.max_calls > 0, "ppr00_max_calls_invalid")?;
    need(
        config.wall_minutes.is_finite() && config.wall_minutes > 0.,
        "ppr00_wall_clock_cap_invalid",
    )?;
    let samples_path = config.output.with_file_name(format!(
        "{}.samples.jsonl",
        config
            .output
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    let start = Instant::now();
    let shared = Arc::new(Mutex::new(Samples {
        phase: "preflight".into(),
        ..Default::default()
    }));
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal = interrupted.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.store(true, Ordering::Relaxed);
        }
    });
    let mut state = State {
        start,
        calls: 0,
        max: config.max_calls,
        wall: config.wall_minutes * 60.,
        incomplete: vec![],
        abort: None,
        interrupted,
        samples: shared.clone(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut sampler = None;
    let mut containers = json!({});
    let mut ownership = None;
    let mut warmups = vec![];
    let mut blocks = vec![];
    let mut restarts = vec![];
    let mut aborted_at = None;
    let mut model = Value::Null;
    let outcome:Result<bool>=async{
 containers=match inspect().await{Ok(c)=>c,Err(_)=>{state.abort=Some("preflight_docker_failed".into());return Ok(false)}};let root=config.root.to_string_lossy();ownership=Some(CONTAINERS.iter().all(|n|containers[*n]["compose_working_dir"]==root.as_ref()));if ownership!=Some(true){state.abort=Some("ownership_mismatch".into());return Ok(false)}if CONTAINERS.iter().any(|n|finite(&containers[*n]["mem_limit_mib"]).is_none()){state.abort=Some("mem_limit_missing".into());return Ok(false)}let gateway=format!("{}/health",config.base.trim_end_matches("/api/v1").trim_end_matches('/'));if !health(&gateway,false).await{state.abort=Some("gateway_health_check_failed".into());return Ok(false)}if !health(&config.health,true).await{state.abort=Some("agent_runtime_health_check_failed".into());return Ok(false)}
 if let Some(p)=samples_path.parent(){std::fs::create_dir_all(p)?;}let shared=shared.clone();let stopped=stop.clone();let cs=containers.clone();let path=samples_path.clone();let interval=config.interval;sampler=Some(tokio::spawn(async move{collect(shared,stopped,cs,path,start,interval).await}));state.phase("warmup_streams");
 let runtime=match crate::ttft::runtime_inputs(&config.env_path){Ok(r)=>r,Err(_)=>{state.abort=Some("adapter_login_failed".into());return Ok(false)}};model=json!(runtime["model_id"]);let mut ids=VecDeque::new();for n in 1..=2{ids.push_back(format!("ppr00.warmup.{n}.{}",uuid::Uuid::new_v4().simple()));}for k in &config.levels{for n in 1..=(2*k).max(10){ids.push_back(format!("ppr00.l{k}.{n}.{}",uuid::Uuid::new_v4().simple()));}}
 let adapter=AIPlatformAdapter::new(AdapterConfig{gateway_base_url:config.base.clone(),email:runtime["email"].clone(),password:runtime["password"].clone(),model_id:runtime["model_id"].clone(),temperature:0.,max_tokens:256,thinking_level:"low".into(),execution_profile:"safe".into(),max_approval_rounds:1,task_output_formats:ids.iter().map(|id|(id.clone(),"text".into())).collect()}).await;let adapter=match adapter{Ok(a)=>a.with_runtime_model_echo(false),Err(e)=>{state.abort=Some("adapter_login_failed".into());warmups.push(json!({"level":"warmup","ordinal":0,"classification":classify(&e.to_string()),"total_seconds":null,"first_event_seconds":null,"reason":e.to_string().chars().take(300).collect::<String>()}));return Ok(false)}};
 for n in 1..=2{if !state.issue(){return Ok(false)}let r=stream(adapter.clone(),ids.pop_front().unwrap(),config.prompt.clone(),json!("warmup"),n).await;let pass=r["classification"]=="success";warmups.push(r);if !pass{state.abort=Some("warmup_stream_failed".into());return Ok(false)}}
 state.phase("quiet");state.sleep(config.quiet).await;if !state.check(){return Ok(false)}state.phase("warmed_idle");state.sleep(config.idle).await;if !state.check(){return Ok(false)}
 for(index,k)in config.levels.iter().enumerate(){blocks.push(level(&mut state,&adapter,*k,&mut ids,&config).await);if !state.check(){return Ok(false)}let recheck=match inspect().await{Ok(c)=>c,Err(_)=>{state.abort=Some("post_level_inspect_failed".into());return Ok(false)}};let mut recreated=vec![];let mut increments=vec![];for name in CONTAINERS{let before=&containers[*name];let after=&recheck[*name];if before["container_id"]!=after["container_id"]{recreated.push(json!({"container":name,"container_id_before":before["container_id"],"container_id_after":after["container_id"]}));}if let Some((b,a))=before["restart_count"].as_i64().zip(after["restart_count"].as_i64()){if a>b{increments.push(json!({"container":name,"restart_before":b,"restart_after":a}));}}}if !recreated.is_empty()||!increments.is_empty(){aborted_at=Some(k);state.abort=Some(if recreated.is_empty(){"container_restart"}else{"container_recreated"}.into());restarts.extend(if recreated.is_empty(){increments}else{recreated});return Ok(false)}if index+1<config.levels.len(){state.phase(&format!("settle:{k}"));state.sleep(config.settle).await;if !state.check(){return Ok(false)}}}Ok(true)
 }.await;
    let completed = match outcome {
        Ok(v) => v,
        Err(_) => {
            state.abort.get_or_insert("infrastructure_error".into());
            false
        }
    };
    stop.store(true, Ordering::Relaxed);
    if let Some(task) = sampler {
        match task.await {
            Ok(Ok(())) => {}
            _ => shared.lock().unwrap().error = Some("sampler_failed".into()),
        }
    }
    signal_task.abort();
    state.check();
    let (samples, violation, error) = {
        let s = shared.lock().unwrap();
        (s.samples.clone(), s.violation.clone(), s.error.clone())
    };
    if samples.is_empty() {
        state.incomplete.push("no_samples".into());
    }
    let mut warmed = json!({});
    for n in CONTAINERS {
        warmed[*n] = window(
            &samples,
            "warmed_idle",
            n,
            finite(&containers[*n]["mem_limit_mib"]),
        );
    }
    let mut steady = BTreeMap::new();
    for block in &mut blocks {
        let k = block["k"].as_u64().unwrap() as usize;
        let mut services = json!({});
        let mut cooldown = json!({});
        for n in CONTAINERS {
            let limit = finite(&containers[*n]["mem_limit_mib"]);
            services[*n] = window(&samples, &format!("steady:{k}"), n, limit);
            cooldown[*n] = window(&samples, &format!("settle:{k}"), n, limit);
        }
        steady.insert(k, services.clone());
        block["services"] = services;
        block["cooldown_services"] = cooldown;
    }
    let interrupted = state.interrupted.load(Ordering::Relaxed);
    let code = if completed && state.incomplete.is_empty() && !interrupted && state.abort.is_none()
    {
        0
    } else {
        2
    };
    let mut identities = json!({});
    let mut owners = json!({});
    for n in CONTAINERS {
        owners[*n] = containers[*n]["compose_working_dir"].clone();
        identities[*n] = json!({});
        for key in [
            "container_id",
            "image_tag",
            "image_id",
            "created",
            "mem_limit_bytes",
            "mem_limit_mib",
            "restart_count",
        ] {
            identities[*n][key] = containers[*n][key].clone();
        }
    }
    use sha2::{Digest, Sha256};
    let report = json!({"schema_version":"ppr00-stream-load/v1","profile":"ppr00-stream-load-v1","generated_at":now(),"status":if code==0{"completed"}else{"aborted_or_incomplete"},"configuration_fingerprint":{"git_commit":crate::ttft::configuration_fingerprint(),"git_commit_scope":"client-side operator checkout only; running identity is pinned by container/image identity","model_id":model,"prompt_sha256":format!("{:x}",Sha256::digest(config.prompt.as_bytes())),"prompt_chars":config.prompt.chars().count(),"thinking_level":"low","temperature":0.,"max_tokens":256,"execution_profile":"safe","memory_mode":"off","skills_enabled":false,"gateway_base_url":config.base},"ownership_check":{"label":OWNERSHIP,"expected_root":config.root,"checked_containers":CONTAINERS,"observed":owners,"ownership_ok":ownership},"container_identity":identities,"levels_requested":config.levels,"schedule_policy":{"per_level_streams":"max(2*K, 10) issued per level","launch_stagger_seconds":config.stagger,"in_flight_hard_cap":"concurrent outstanding streams never exceed 3*K (fixed pool of K workers)","settle_gap_seconds":config.settle,"sample_interval_seconds":config.interval,"stream_mix":"byte-identical TTFT request: thinking low, temperature 0.0, max_tokens 256, execution_profile safe, memory off, skills off"},"warmup_policy":{"health_checks":[format!("{}/health",config.base.trim_end_matches("/api/v1").trim_end_matches('/')),config.health],"warmup_streams":2,"quiet_seconds":config.quiet,"idle_baseline_window_seconds":config.idle,"warmed_rss_definition":"mean RSS over the idle baseline sample window","first_two_ordinals_excluded":"not applicable — warm-up streams excluded from per-level error stats"},"warmup":{"streams":warmups,"excluded_from_level_error_stats":true},"warmed_idle":warmed,"levels":blocks,"derivations":derive(&warmed,&steady),"raw_samples_file":samples_path,"raw_sample_count":samples.len(),"exit":{"code":code,"completed_all_levels":completed,"incomplete":!state.incomplete.is_empty(),"incomplete_reasons":state.incomplete,"interrupted":interrupted,"aborted_reason":state.abort,"aborted_at_level":aborted_at,"mem_guard_violation":violation,"sampler_docker_error":error,"restart_evidence":restarts,"calls_issued":state.calls,"max_calls":config.max_calls,"wall_clock_cap_minutes":config.wall_minutes,"elapsed_seconds":round(start.elapsed().as_secs_f64(),3)}});
    crate::ttft::atomic_report(&config.output, &report)?;
    Ok(report)
}
pub async fn main() -> Result<i32> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = BTreeMap::new();
    need(a.len() % 2 == 0, "option_value_missing")?;
    for p in a.chunks_exact(2) {
        need(
            [
                "--env-file",
                "--gateway-base-url",
                "--levels",
                "--output",
                "--max-calls",
                "--wall-clock-cap-minutes",
                "--agent-runtime-health-url",
                "--repo-root",
            ]
            .contains(&p[0].as_str()),
            "unknown_option",
        )?;
        opts.insert(p[0].clone(), p[1].clone());
    }
    let get = |k: &str, d: &str| opts.get(k).cloned().unwrap_or(d.into());
    let levels = get("--levels", "1,5,10,20")
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse::<usize>())
        .collect::<std::result::Result<BTreeSet<_>, _>>()?
        .into_iter()
        .collect();
    let config = Config {
        root: PathBuf::from(get("--repo-root", ".")).canonicalize()?,
        env_path: opts.get("--env-file").ok_or("--env-file required")?.into(),
        base: get("--gateway-base-url", "http://127.0.0.1:8080/api/v1"),
        output: opts.get("--output").ok_or("--output required")?.into(),
        levels,
        max_calls: get("--max-calls", "90").parse()?,
        wall_minutes: get("--wall-clock-cap-minutes", "30").parse()?,
        health: get(
            "--agent-runtime-health-url",
            "http://127.0.0.1:8094/health/ready",
        ),
        stagger: 0.5,
        settle: 60.,
        quiet: 60.,
        idle: 60.,
        interval: 1.,
        prompt: PROMPT.into(),
    };
    let output = config.output.clone();
    let report = run(config).await?;
    println!(
        "{}",
        json!({"output":output,"exit_code":report["exit"]["code"],"completed_all_levels":report["exit"]["completed_all_levels"],"incomplete":report["exit"]["incomplete"],"aborted_reason":report["exit"]["aborted_reason"],"aborted_at_level":report["exit"]["aborted_at_level"],"calls_issued":report["exit"]["calls_issued"],"levels":report["levels"].as_array().unwrap().iter().map(|v|&v["k"]).collect::<Vec<_>>()})
    );
    Ok(report["exit"]["code"].as_i64().unwrap_or(2) as i32)
}
