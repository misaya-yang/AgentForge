//! Provider-free bounded fixed-GET measurement. Caller owns service lifecycle.
//! The CLI exposes only the offline plan; measure_case requires an owned PID verifier.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAX_BODY: usize = 64 * 1024;
const MAX_HEADER: usize = 8 * 1024;
fn check(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
#[derive(Clone)]
pub struct ReadProfile {
    pub name: String,
    pub run_id: Option<String>,
    pub framework: Option<String>,
    token: Option<String>,
}
impl ReadProfile {
    pub fn new(
        name: String,
        run_id: Option<String>,
        framework: Option<String>,
        token: Option<String>,
    ) -> Result<Self> {
        match name.as_str() {
            "public_liveness" => check(
                run_id.is_none() && framework.is_none() && token.is_none(),
                "liveness_has_no_identity",
            )?,
            "owner_terminal_sdk_read" => {
                check(
                    run_id.as_deref().is_some_and(|id| {
                        uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id)
                    }) && matches!(
                        framework.as_deref(),
                        Some("openai_agents" | "claude_agent_sdk")
                    ),
                    "invalid_owned_sdk_profile",
                )?;
                check(
                    token.as_ref().is_some_and(|v| {
                        !v.is_empty()
                            && v.len() <= 16 * 1024
                            && v.is_ascii()
                            && v.bytes().all(|b| b >= 33 && b != 127)
                    }),
                    "invalid_in_memory_token",
                )?;
            }
            _ => return Err("unsupported_read_profile".into()),
        }
        Ok(Self {
            name,
            run_id,
            framework,
            token,
        })
    }
    fn path(&self) -> String {
        if self.name == "public_liveness" {
            "/health/live".into()
        } else {
            format!(
                "/api/v1/assistant/runs/{}",
                self.run_id.as_deref().unwrap_or("")
            )
        }
    }
    fn valid_body(&self, bytes: &[u8]) -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return false;
        };
        if self.name == "public_liveness" {
            value == json!({"status":"alive"})
        } else {
            let r = &value["run"];
            r.is_object()
                && r["run_id"].as_str() == self.run_id.as_deref()
                && r["engine"].as_str() == self.framework.as_deref()
                && r["status"] == "cancelled"
                && r.get("harness_thread_id") == Some(&Value::Null)
                && r.get("harness_turn_id") == Some(&Value::Null)
                && r.get("request_id").is_none()
                && r.get("service_id").is_none()
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Budget {
    pub concurrency: usize,
    pub measured_requests: usize,
    pub request_timeout_seconds: f64,
    pub case_timeout_seconds: f64,
    pub sample_interval_seconds: f64,
    pub max_gateway_rss_bytes: Option<u64>,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            concurrency: 1,
            measured_requests: 12,
            request_timeout_seconds: 2.0,
            case_timeout_seconds: 15.0,
            sample_interval_seconds: 0.05,
            max_gateway_rss_bytes: None,
        }
    }
}
impl Budget {
    pub fn validate(&self) -> Result<()> {
        check(
            (1..=8).contains(&self.concurrency)
                && (self.concurrency..=200).contains(&self.measured_requests)
                && [
                    self.request_timeout_seconds,
                    self.case_timeout_seconds,
                    self.sample_interval_seconds,
                ]
                .iter()
                .all(|n| n.is_finite())
                && (0.05..=5.0).contains(&self.request_timeout_seconds)
                && (self.request_timeout_seconds..=30.0).contains(&self.case_timeout_seconds)
                && (0.02..=1.0).contains(&self.sample_interval_seconds)
                && self.max_gateway_rss_bytes.is_none_or(|n| n > 0),
            "invalid_safety_budget",
        )
    }
}
#[derive(Clone, Default, Serialize)]
pub struct Observation {
    pub sequence: usize,
    pub status: Option<u16>,
    pub outcome: String,
    pub client_queue_ms: Option<f64>,
    pub elapsed_ms: Option<f64>,
    pub first_response_byte_ms: Option<f64>,
    pub response_headers_ms: Option<f64>,
    pub first_body_byte_ms: Option<f64>,
    pub body_bytes: usize,
    pub reused_connection: bool,
    pub connection_ms: Option<f64>,
}
impl Observation {
    fn new(sequence: usize) -> Self {
        Self {
            sequence,
            outcome: "not_started".into(),
            ..Default::default()
        }
    }
}
#[derive(Default)]
pub struct HttpConnection {
    stream: Option<BufReader<tokio::net::TcpStream>>,
}
impl HttpConnection {
    pub async fn close(&mut self) {
        self.stream = None;
    }
    pub async fn request(&mut self, profile: &ReadProfile, result: &mut Observation, timeout: f64) {
        let start = Instant::now();
        match tokio::time::timeout(
            Duration::from_secs_f64(timeout),
            self.inner(profile, result, start),
        )
        .await
        {
            Err(_) => {
                result.outcome = "timeout".into();
                self.close().await;
            }
            Ok(Err(_)) => {
                result.outcome = "transport_or_protocol_error".into();
                self.close().await;
            }
            Ok(Ok(())) => {}
        }
        result.elapsed_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
    }
    async fn inner(
        &mut self,
        profile: &ReadProfile,
        result: &mut Observation,
        start: Instant,
    ) -> Result<()> {
        result.reused_connection = self.stream.is_some();
        if self.stream.is_none() {
            let connect = Instant::now();
            self.stream = Some(BufReader::new(
                tokio::net::TcpStream::connect(("127.0.0.1", 18080)).await?,
            ));
            result.connection_ms = Some(connect.elapsed().as_secs_f64() * 1000.0);
        }
        let stream = self.stream.as_mut().ok_or("missing_connection")?;
        let mut request = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1:18080\r\nAccept: application/json\r\nConnection: keep-alive\r\n",
            profile.path()
        );
        if let Some(token) = &profile.token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        request.push_str("\r\n");
        stream.get_mut().write_all(request.as_bytes()).await?;
        stream.get_mut().flush().await?;
        let first = stream.read_u8().await?;
        result.first_response_byte_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
        let mut raw = vec![first];
        while !raw.ends_with(b"\r\n\r\n") {
            check(raw.len() < MAX_HEADER, "response_header_limit")?;
            raw.push(stream.read_u8().await?);
        }
        result.response_headers_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
        let (status, headers) = parse_headers(&raw)?;
        result.status = Some(status);
        let payload = body(stream, &headers, result, start).await?;
        result.body_bytes = payload.len();
        result.outcome = match status {
            200 => {
                if headers
                    .get("content-type")
                    .map(|s| s.split(';').next().unwrap_or(""))
                    == Some("application/json")
                    && profile.valid_body(&payload)
                {
                    "success"
                } else {
                    "invalid_success_body"
                }
            }
            429 => "rate_limited",
            503 => "unavailable",
            _ => "unexpected_http_status",
        }
        .into();
        if headers
            .get("connection")
            .is_some_and(|s| s.eq_ignore_ascii_case("close"))
        {
            self.close().await;
        }
        Ok(())
    }
}
async fn line(
    stream: &mut BufReader<tokio::net::TcpStream>,
    budget: &mut usize,
) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    while !line.ends_with(b"\r\n") {
        check(*budget < MAX_HEADER, "chunk_framing_limit")?;
        line.push(stream.read_u8().await?);
        *budget += 1;
    }
    Ok(line)
}
async fn take(
    stream: &mut BufReader<tokio::net::TcpStream>,
    payload: &mut Vec<u8>,
    mut count: usize,
    result: &mut Observation,
    start: Instant,
) -> Result<()> {
    check(
        count <= MAX_BODY.saturating_sub(payload.len()),
        "response_body_limit",
    )?;
    if count > 0 {
        if result.first_body_byte_ms.is_none() {
            payload.push(stream.read_u8().await?);
            result.first_body_byte_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
            count -= 1;
        }
        let offset = payload.len();
        payload.resize(offset + count, 0);
        stream.read_exact(&mut payload[offset..]).await?;
    }
    Ok(())
}
async fn body(
    stream: &mut BufReader<tokio::net::TcpStream>,
    headers: &BTreeMap<String, String>,
    result: &mut Observation,
    start: Instant,
) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    if let Some(encoding) = headers.get("transfer-encoding") {
        check(
            encoding.eq_ignore_ascii_case("chunked") && !headers.contains_key("content-length"),
            "ambiguous_response_framing",
        )?;
        let mut framing = 0;
        loop {
            let raw = line(stream, &mut framing).await?;
            let size = raw[..raw.len() - 2]
                .split(|b| *b == b';')
                .next()
                .unwrap_or(&[]);
            check(
                !size.is_empty() && size.iter().all(u8::is_ascii_hexdigit),
                "invalid_chunk_size",
            )?;
            let size = usize::from_str_radix(std::str::from_utf8(size)?, 16)?;
            if size == 0 {
                loop {
                    if line(stream, &mut framing).await? == b"\r\n" {
                        return Ok(payload);
                    }
                }
            }
            take(stream, &mut payload, size, result, start).await?;
            let mut delimiter = [0; 2];
            stream.read_exact(&mut delimiter).await?;
            check(delimiter == *b"\r\n", "invalid_chunk_delimiter")?;
        }
    }
    let length = headers
        .get("content-length")
        .ok_or("missing_or_invalid_response_length")?;
    check(
        !length.is_empty() && length.bytes().all(|b| b.is_ascii_digit()),
        "missing_or_invalid_response_length",
    )?;
    take(stream, &mut payload, length.parse()?, result, start).await?;
    Ok(payload)
}
pub fn parse_headers(raw: &[u8]) -> Result<(u16, BTreeMap<String, String>)> {
    check(
        raw.len() <= MAX_HEADER && raw.ends_with(b"\r\n\r\n"),
        "response_header_limit",
    )?;
    let text: String = raw[..raw.len() - 4]
        .iter()
        .map(|b| char::from(*b))
        .collect();
    let mut lines = text.split("\r\n");
    let parts: Vec<_> = lines.next().unwrap_or("").splitn(3, ' ').collect();
    check(
        parts.len() >= 2
            && parts[0] == "HTTP/1.1"
            && parts[1].len() == 3
            && parts[1].bytes().all(|b| b.is_ascii_digit()),
        "invalid_response_status",
    )?;
    let status = parts[1].parse()?;
    check((200..=599).contains(&status), "unsupported_response_status")?;
    let mut headers = BTreeMap::new();
    for line in lines {
        let (key, value) = line.split_once(':').ok_or("invalid_response_header")?;
        let key = key.to_lowercase();
        check(
            !key.is_empty() && key.trim() == key,
            "invalid_response_header",
        )?;
        if [
            "content-length",
            "transfer-encoding",
            "content-type",
            "connection",
        ]
        .contains(&key.as_str())
        {
            check(
                headers.insert(key, value.trim().to_owned()).is_none(),
                "duplicate_response_framing",
            )?;
        }
    }
    Ok((status, headers))
}
#[derive(Clone, Copy)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
}
fn monotonic() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}
impl ProcessIdentity {
    pub fn sample(&self) -> Result<Value> {
        let root = std::path::PathBuf::from(format!("/proc/{}", self.pid));
        let stat = std::fs::read_to_string(root.join("stat"))?;
        let fields: Vec<_> = stat
            .rsplit_once(") ")
            .ok_or("invalid_process_stat")?
            .1
            .split_whitespace()
            .collect();
        check(
            fields.get(19).and_then(|s| s.parse::<u64>().ok()) == Some(self.start_ticks),
            "gateway_process_identity_changed",
        )?;
        let fds = std::fs::read_dir(root.join("fd"))?.collect::<std::io::Result<Vec<_>>>()?;
        let mut sockets = BTreeSet::new();
        for fd in &fds {
            match std::fs::read_link(fd.path()) {
                Ok(target) => {
                    let target = target.to_string_lossy();
                    if target.starts_with("socket:[") {
                        sockets.insert(target.into_owned());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        check(ticks > 0 && page > 0, "invalid_process_clock")?;
        let number =
            |n: usize| -> Result<u64> { Ok(fields.get(n).ok_or("invalid_process_stat")?.parse()?) };
        Ok(
            json!({"monotonic_seconds":monotonic(),"cpu_seconds":(number(11)?+number(12)?)as f64/ticks as f64,"rss_bytes":number(21)?*page as u64,"open_fd_count":fds.len(),"open_socket_count":sockets.len()}),
        )
    }
}
pub fn distribution(mut values: Vec<f64>) -> Result<Value> {
    check(
        values.iter().all(|v| v.is_finite()),
        "nonfinite_measurement",
    )?;
    values.sort_by(f64::total_cmp);
    let q = |p: f64| {
        if values.is_empty() {
            Value::Null
        } else {
            json!(values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)])
        }
    };
    Ok(
        json!({"count":values.len(),"method":"nearest_rank","p50":q(0.50),"p95":q(0.95),"p99":q(0.99),"min":values.first(),"max":values.last(),"mean":if values.is_empty(){None}else{Some(values.iter().sum::<f64>()/values.len()as f64)},"tail_sample_warning":values.len()<100}),
    )
}
pub fn summarize(rows: &[Observation], elapsed: f64) -> Result<Value> {
    check(
        elapsed.is_finite() && elapsed > 0.0,
        "invalid_measurement_window",
    )?;
    let started: Vec<_> = rows.iter().filter(|r| r.elapsed_ms.is_some()).collect();
    let success: Vec<_> = rows.iter().filter(|r| r.outcome == "success").collect();
    let responses: Vec<_> = rows.iter().filter(|r| r.status.is_some()).collect();
    let rejected: Vec<_> = rows
        .iter()
        .filter(|r| matches!(r.outcome.as_str(), "rate_limited" | "unavailable"))
        .collect();
    let all: Vec<_> = rows.iter().collect();
    let mut outcomes = BTreeMap::new();
    let mut statuses = BTreeMap::new();
    for row in rows {
        *outcomes.entry(&row.outcome).or_insert(0) += 1;
        if let Some(status) = row.status {
            *statuses.entry(status.to_string()).or_insert(0) += 1;
        }
    }
    let latency = |rows: &[&Observation], field: fn(&Observation) -> Option<f64>| {
        distribution(rows.iter().filter_map(|r| field(r)).collect())
    };
    Ok(
        json!({"scheduled_requests":rows.len(),"started_requests":started.len(),"http_responses":responses.len(),"successful_requests":success.len(),"non_success_requests":rows.len()-success.len(),"outcomes":outcomes,"http_statuses":statuses,"wall_seconds":elapsed,"scheduled_per_second":rows.len()as f64/elapsed,"finished_attempts_per_second":started.len()as f64/elapsed,"http_responses_per_second":responses.len()as f64/elapsed,"success_per_second":success.len()as f64/elapsed,"non_success_fraction":if rows.is_empty(){None}else{Some((rows.len()-success.len())as f64/rows.len()as f64)},"all_attempt_elapsed_ms":latency(&started,|r|r.elapsed_ms)?,"success_elapsed_ms":latency(&success,|r|r.elapsed_ms)?,"rejection_elapsed_ms":latency(&rejected,|r|r.elapsed_ms)?,"first_response_byte_ms":latency(&success,|r|r.first_response_byte_ms)?,"response_headers_ms":latency(&success,|r|r.response_headers_ms)?,"first_body_byte_ms":latency(&success,|r|r.first_body_byte_ms)?,"client_batch_queue_ms":latency(&all,|r|r.client_queue_ms)?,"new_connection_ms":latency(&all,|r|r.connection_ms)?,"reused_connections":rows.iter().filter(|r|r.reused_connection).count(),"response_body_bytes":rows.iter().map(|r|r.body_bytes).sum::<usize>()}),
    )
}
pub fn process_summary(samples: &[Value]) -> Result<Value> {
    if samples.len() < 2 {
        return Ok(json!({"status":"unavailable","reason":"fewer_than_two_process_samples"}));
    }
    let first = &samples[0];
    let last = &samples[samples.len() - 1];
    let n = |v: &Value, k: &str| -> Result<f64> {
        v[k].as_f64().ok_or_else(|| "invalid_process_sample".into())
    };
    let elapsed = n(last, "monotonic_seconds")? - n(first, "monotonic_seconds")?;
    let cpu = n(last, "cpu_seconds")? - n(first, "cpu_seconds")?;
    check(elapsed > 0.0 && cpu >= 0.0, "invalid_process_sample_window")?;
    let max = |key: &str| samples.iter().filter_map(|v| v[key].as_u64()).max();
    Ok(
        json!({"status":"measured","sample_count":samples.len(),"window_seconds":elapsed,"gateway_cpu_seconds":cpu,"gateway_cpu_percent_of_one_core":100.0*cpu/elapsed,"cpu_tick_resolution_seconds":1.0/unsafe{libc::sysconf(libc::_SC_CLK_TCK)}as f64,"sampled_peak_rss_bytes":max("rss_bytes"),"rss_start_bytes":first["rss_bytes"],"rss_end_bytes":last["rss_bytes"],"sampled_peak_open_fd_count":max("open_fd_count"),"sampled_peak_open_socket_count":max("open_socket_count"),"scope":"Gateway PID only, including its background work; sampled socket count is not a DB pool gauge"}),
    )
}
fn cpu() -> Result<f64> {
    let mut value = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) } != 0 {
        return Err("load_generator_cpu_measurement_failed".into());
    }
    let value = unsafe { value.assume_init() };
    Ok(value.ru_utime.tv_sec as f64
        + value.ru_utime.tv_usec as f64 / 1e6
        + value.ru_stime.tv_sec as f64
        + value.ru_stime.tv_usec as f64 / 1e6)
}
struct RowGuard {
    rows: Arc<Mutex<Vec<Observation>>>,
    row: Observation,
    start: Instant,
    complete: bool,
}
impl Drop for RowGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.row.outcome = "client_cancelled_by_case_deadline".into();
            self.row.elapsed_ms = Some(self.start.elapsed().as_secs_f64() * 1000.0);
        }
        if let Ok(mut rows) = self.rows.lock() {
            let sequence = self.row.sequence;
            rows[sequence] = self.row.clone();
        }
    }
}
#[derive(Debug)]
pub struct BaselineCancelled {
    pub partial_case: Value,
}
impl std::fmt::Display for BaselineCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("baseline_cancelled")
    }
}
impl std::error::Error for BaselineCancelled {}
pub async fn measure_case<F>(
    profile: ReadProfile,
    budget: Budget,
    process: ProcessIdentity,
    verify_owned: F,
) -> Result<Value>
where
    F: Fn() -> Result<()>,
{
    measure_case_cancellable(
        profile,
        budget,
        process,
        verify_owned,
        Arc::new(AtomicBool::new(false)),
    )
    .await
}
/// Explicit cancellation returns bounded partial observations, never a pass receipt.
/// Dropping this future aborts all owned workers via JoinSet and closes sockets.
pub async fn measure_case_cancellable<F>(
    profile: ReadProfile,
    budget: Budget,
    process: ProcessIdentity,
    verify_owned: F,
    cancel: Arc<AtomicBool>,
) -> Result<Value>
where
    F: Fn() -> Result<()>,
{
    budget.validate()?;
    let profile = ReadProfile::new(
        profile.name,
        profile.run_id,
        profile.framework,
        profile.token,
    )?;
    verify_owned()?;
    process.sample()?;
    let warmups = Arc::new(Mutex::new(
        (0..budget.concurrency)
            .map(Observation::new)
            .collect::<Vec<_>>(),
    ));
    let observations = Arc::new(Mutex::new(
        (0..budget.measured_requests)
            .map(Observation::new)
            .collect::<Vec<_>>(),
    ));
    let mut samples = Vec::new();
    let mut errors = Vec::new();
    let mut status = "MEASURED_EXPLORATORY";
    let mut generator_start = cpu()?;
    let mut started = Instant::now();
    let mut warm = tokio::task::JoinSet::new();
    for index in 0..budget.concurrency {
        let profile = profile.clone();
        let rows = warmups.clone();
        let timeout = budget.request_timeout_seconds;
        warm.spawn(async move {
            let mut connection = HttpConnection::default();
            let mut guard = RowGuard {
                rows,
                row: Observation::new(index),
                start: Instant::now(),
                complete: false,
            };
            connection.request(&profile, &mut guard.row, timeout).await;
            guard.complete = true;
            drop(guard);
            connection
        });
    }
    let mut connections = Vec::new();
    let mut cancelled = false;
    while !warm.is_empty() {
        tokio::select! {result=warm.join_next()=>{connections.push(result.ok_or("warmup_task_missing")??);},_=tokio::time::sleep(Duration::from_millis(10))=>{if cancel.load(Ordering::Acquire){cancelled=true;warm.abort_all();while warm.join_next().await.is_some(){}break;}}}
    }
    if !cancelled
        && warmups
            .lock()
            .map_err(|_| "observation_lock")?
            .iter()
            .any(|r| r.outcome != "success")
    {
        status = "WARMUP_FAILED";
    } else if !cancelled {
        generator_start = cpu()?;
        started = Instant::now();
        samples.push(process.sample()?);
        let next = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = tokio::task::JoinSet::new();
        for mut connection in connections {
            let next = next.clone();
            let stop = stop.clone();
            let rows = observations.clone();
            let profile = profile.clone();
            let count = budget.measured_requests;
            let timeout = budget.request_timeout_seconds;
            workers.spawn(async move {
                loop {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let sequence = next.fetch_add(1, Ordering::AcqRel);
                    if sequence >= count {
                        break;
                    }
                    let mut row = Observation::new(sequence);
                    row.client_queue_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                    let mut guard = RowGuard {
                        rows: rows.clone(),
                        row,
                        start: Instant::now(),
                        complete: false,
                    };
                    connection.request(&profile, &mut guard.row, timeout).await;
                    guard.complete = true;
                }
                connection.close().await;
            });
        }
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs_f64(budget.case_timeout_seconds);
        let mut sample_tick =
            tokio::time::interval(Duration::from_secs_f64(budget.sample_interval_seconds));
        sample_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !workers.is_empty() {
            tokio::select! {result=workers.join_next()=>{if result.is_some_and(|r|r.is_err()){errors.push("load_generator_worker_failed");status="RESOURCE_OBSERVATION_FAILED";}},_=tokio::time::sleep_until(deadline)=>{status="CASE_DEADLINE_EXCEEDED";stop.store(true,Ordering::Release);workers.abort_all();while workers.join_next().await.is_some(){}break;},_=sample_tick.tick()=>{if cancel.load(Ordering::Acquire){cancelled=true;stop.store(true,Ordering::Release);workers.abort_all();while workers.join_next().await.is_some(){}break;}match process.sample(){Ok(point)=>{let exceeded=budget.max_gateway_rss_bytes.is_some_and(|limit|point["rss_bytes"].as_u64().is_some_and(|rss|rss>limit));samples.push(point);if exceeded{errors.push("gateway_rss_safety_budget_exceeded");stop.store(true,Ordering::Release);status="RESOURCE_OBSERVATION_FAILED";}},Err(_)=>{errors.push("gateway_process_sampling_failed");stop.store(true,Ordering::Release);status="RESOURCE_OBSERVATION_FAILED";}}}}
        }
        match process.sample() {
            Ok(point) => samples.push(point),
            Err(_) => {
                errors.push("final_gateway_process_sample_failed");
                status = "RESOURCE_OBSERVATION_FAILED";
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64().max(1e-9);
    let rows = observations.lock().map_err(|_| "observation_lock")?.clone();
    let warmups = warmups.lock().map_err(|_| "observation_lock")?.clone();
    let summary = summarize(&rows, elapsed)?;
    if cancelled {
        return Err(Box::new(BaselineCancelled {
            partial_case: json!({"profile":profile.name,"budget":budget,"status":"CANCELLED_WITH_PARTIAL_OBSERVATIONS","warmup":{"requests":warmups.len(),"observations":warmups},"summary":summary,"observations":rows}),
        }));
    }
    if verify_owned().is_err() {
        errors.push("final_gateway_ownership_verification_failed");
        status = "IDENTITY_VERIFICATION_FAILED";
    }
    Ok(
        json!({"profile":profile.name,"budget":budget,"status":status,"load_model":"finite_closed_loop_batch_no_retries","warmup":{"requests":warmups.len(),"observations":warmups},"summary":summary,"process":process_summary(&samples)?,"resource_errors":errors,"load_generator_cpu_window":if status=="WARMUP_FAILED"{"warmup_failed_no_measured_batch"}else{"measured_batch_excluding_warmup"},"load_generator_cpu_seconds":cpu()?-generator_start,"observations":rows}),
    )
}
pub fn measurement_scope() -> Value {
    json!({"measured":["client_observed_fixed_GET_latency","first_response_and_body_byte","empirical_p50_p95_p99","closed_loop_throughput_and_all_errors","in_client_batch_queue_delay","Gateway_PID_CPU_RSS_FD_socket_samples"],"unavailable":{"gateway_exclusive_overhead":"No per-request provider or internal-stage server timing on these reads; client/network costs included.","provider_latency":"No model request is made; not applicable, never synthesized as zero.","streaming_first_token_inter_chunk":"JSON GET profiles do not exercise SSE; no streaming acceptance.","server_queue_delay":"Client batch queue is not server admission or execution queue.","knowledge_retrieval":"No Knowledge query, embedding, retrieval or relevance fixture is exercised.","database_pool_pressure":"FD/socket samples cannot distinguish pool occupancy, waiters or acquire duration.","server_saturation_capacity":"Bounded concurrency is not a saturation search; 429 establishes rate policy rejection only.","execution_cancellation":"Reading a cancelled inert SDK run does not execute or stop an SDK/provider/tool loop."},"enterprise_slo":"UNSET_REQUIRES_OWNER_REVIEWED_WORKLOAD_AND_ENVIRONMENT","acceptance":"Exploratory measurement, not release readiness, scale capacity or an enterprise SLO pass."})
}
pub fn plan() -> Value {
    json!({"schema":"gateway-performance-baseline/v1","status":"PREPARED_NOT_EXECUTED","origin":"http://127.0.0.1:18080","profiles":["public_liveness","owner_terminal_sdk_read"],"concurrency":[1,2,4],"business_measured_requests_per_case":12,"business_quiet_windows":"Controller preserves existing rate limits between normal cases","service_lifecycle":"Existing owned-fixture controller only; this module starts/stops nothing","scope":measurement_scope()})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_budgets_only() {
        let mut b = Budget::default();
        assert!(b.validate().is_ok());
        b.concurrency = 9;
        assert!(b.validate().is_err());
        b.concurrency = 1;
        b.case_timeout_seconds = f64::INFINITY;
        assert!(b.validate().is_err());
    }
    #[test]
    fn profile_rejects_token_in_liveness_and_forged_read_shapes() {
        assert!(
            ReadProfile::new("public_liveness".into(), None, None, Some("secret".into())).is_err()
        );
        let p = ReadProfile::new("public_liveness".into(), None, None, None).unwrap();
        assert!(p.valid_body(br#"{"status":"alive"}"#));
        assert!(!p.valid_body(br#"{"status":"alive","fake":true}"#));
    }
    #[test]
    fn framing_rejects_ambiguity_and_unsupported_status() {
        assert!(parse_headers(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n"
        )
        .is_err());
        assert!(parse_headers(b"HTTP/1.0 200 OK\r\n\r\n").is_err());
        assert!(parse_headers(b"HTTP/1.1 100 Continue\r\n\r\n").is_err());
        assert_eq!(
            parse_headers(b"HTTP/1.1 429 Slow\r\nContent-Length: 0\r\n\r\n")
                .unwrap()
                .0,
            429
        );
    }
    #[test]
    fn absent_measurements_are_null_not_zero() {
        let d = distribution(vec![]).unwrap();
        assert!(d["p95"].is_null());
        assert_eq!(distribution(vec![1.0, 2.0, 3.0, 4.0]).unwrap()["p50"], 2.0);
        assert!(distribution(vec![f64::NAN]).is_err());
        let summary = summarize(&[Observation::new(0)], 1.0).unwrap();
        assert_eq!(summary["non_success_requests"], 1);
        assert_eq!(summary["started_requests"], 0);
    }
    #[test]
    fn stale_pid_fails_before_http() {
        assert!(ProcessIdentity {
            pid: std::process::id(),
            start_ticks: 0
        }
        .sample()
        .is_err());
    }
}
