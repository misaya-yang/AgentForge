//! Native official-ingress parity adapter. Requests are never replayed on transport failure.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct AdapterConfig {
    pub gateway_base_url: String,
    pub email: String,
    pub password: String,
    pub model_id: String,
    pub temperature: f64,
    pub max_tokens: u64,
    pub thinking_level: String,
    pub execution_profile: String,
    pub max_approval_rounds: usize,
    pub task_output_formats: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnResult {
    pub text: String,
    pub duration_seconds: f64,
    pub terminal_status: String,
    pub metadata: Value,
}
#[derive(Clone)]
pub struct AIPlatformAdapter {
    config: AdapterConfig,
    token: String,
    sessions: BTreeMap<String, String>,
    client: reqwest::Client,
    strict_runtime_model_echo: bool,
}
const TOOL: &str = "execute_python_code";
const REASON: &str = "native-agent-parity benchmark execute_python_code verification";
const RESUME: &str = "Continue the exact approved execute_python_code tool call.";
fn need(b: bool, code: &str) -> Result<()> {
    if b {
        Ok(())
    } else {
        Err(code.to_string().into())
    }
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn round(n: f64) -> f64 {
    (n * 1e6).round() / 1e6
}
fn identifier(v: &Value, key: &str) -> Result<String> {
    let s = v[key]
        .as_str()
        .ok_or_else(|| format!("gateway_approval_{key}_missing"))?;
    need(
        regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$")?.is_match(s),
        &format!("gateway_approval_{key}_missing"),
    )?;
    Ok(s.into())
}
#[derive(Default)]
struct Stream {
    events: Vec<String>,
    chunks: Vec<String>,
    terminal: String,
    runtime: Value,
    failover: Vec<Value>,
    usage: Option<Value>,
    approvals: Vec<Value>,
    results: Vec<Value>,
    started: Vec<Value>,
    lifecycle: Vec<Value>,
    first_text: Option<f64>,
    first_thinking: Option<f64>,
    finished: f64,
    timing: Value,
}
impl Stream {
    fn count(&self, s: &str) -> usize {
        self.events.iter().filter(|e| e.as_str() == s).count()
    }
    fn has(&self, s: &str) -> bool {
        self.events.iter().any(|e| e == s)
    }
    fn event(&mut self, e: Value, phase: &str, at: f64) -> Result<()> {
        need(e.is_object(), "gateway_invalid_sse_json")?;
        let kind = e["event_type"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| e["type"].as_str())
            .unwrap_or("");
        if !kind.is_empty() {
            self.events.push(kind.into());
        }
        if ["thinking_start", "thinking_delta"].contains(&kind) && self.first_thinking.is_none() {
            self.first_thinking = Some(at);
        }
        let p = &e["data"];
        if kind == "run_started" && p.is_object() {
            self.started.push(p.clone());
            let s = &p["context_snapshot"];
            if s.is_object() {
                self.runtime = json!({"model_id":s["model_id"],"provider":s["provider"],"context_snapshot_hash":s["snapshot_hash"],"startup_config_fingerprint":s["bootstrap"]["startup_config_fingerprint"]});
            }
        }
        if kind == "gateway_decision" && p.is_object() && p["decision_type"] == "model_failover" {
            let mut v = serde_json::Map::new();
            for k in [
                "requested_model",
                "failed_model",
                "served_model",
                "failure_class",
                "attempt",
            ] {
                v.insert(k.into(), p[k].clone());
            }
            self.failover.push(v.into());
        }
        if kind == "usage" && p.is_object() {
            self.usage = Some(p.clone());
        }
        if kind == "text_delta" {
            if let Some(s) = p.as_str().or_else(|| p["content"].as_str()) {
                self.chunks.push(s.into());
                if !s.is_empty() && self.first_text.is_none() {
                    self.first_text = Some(at);
                }
            }
        }
        if kind == "approval_required" && p.is_object() {
            self.approvals.push(p.clone());
        }
        if kind == "approval_result" && p.is_object() {
            self.results.push(p.clone());
        }
        if [
            "run_started",
            "approval_required",
            "approval_result",
            "run_finished",
            "run_error",
        ]
        .contains(&kind)
            && p.is_object()
        {
            let state = match kind {
                "run_started" if phase == "resumed" => "resumed",
                "run_started" => "started",
                "approval_required" => "paused",
                "approval_result" => "approved",
                "run_finished" => "finished",
                _ => "failed",
            };
            let mut r =
                json!({"phase":phase,"event_type":kind,"state":state,"elapsed_seconds":round(at)});
            for k in ["run_id", "approval_id", "attempt_id"] {
                if let Some(s) = p[k].as_str().filter(|s| !s.is_empty()) {
                    r[format!("{k}_sha256")] = hash(s).into();
                }
            }
            if let Some(s) = p["tool_name"].as_str().filter(|s| !s.is_empty()) {
                r["tool_name"] = s.into();
            }
            if p["attempt_number"].is_i64() || p["attempt_number"].is_u64() {
                r["attempt_number"] = p["attempt_number"].clone();
            }
            self.lifecycle.push(r);
        }
        if ["approval_required", "run_finished", "run_error"].contains(&kind) && p.is_object() {
            if let Some(s) = p["terminal_envelope"]["status"].as_str() {
                self.terminal = s.into();
            }
        }
        Ok(())
    }
    fn approval_boundary(&self) -> Result<()> {
        need(
            self.count("approval_required") == 1,
            "gateway_approval_limit_exceeded",
        )?;
        need(
            self.approvals.len() == 1,
            "gateway_approval_event_malformed",
        )?;
        need(
            !self.has("run_finished") && !self.has("run_error") && !self.has("side_effect_unknown"),
            "gateway_terminal_contract",
        )?;
        need(
            self.terminal == "blocked",
            "gateway_approval_pause_contract",
        )?;
        let p = &self.approvals[0];
        let tool = identifier(p, "tool_name")?;
        need(
            tool == TOOL,
            &format!("gateway_approval_tool_not_allowed:{tool}"),
        )?;
        need(
            p["terminal_envelope"]["resume_ready"] == true,
            "gateway_approval_resume_not_ready",
        )?;
        let run = identifier(p, "run_id")?;
        need(self.started.len() == 1, "gateway_run_started_contract")?;
        need(
            self.started[0]["run_id"] == run,
            "gateway_approval_run_mismatch",
        )
    }
    fn resume(&self, run: &str, approval: &str) -> Result<()> {
        need(self.started.len() == 1, "gateway_resume_started_contract")?;
        need(
            self.started[0]["run_id"] == run,
            "gateway_resume_run_mismatch",
        )?;
        need(self.results.len() == 1, "gateway_approval_result_contract")?;
        let r = &self.results[0];
        need(
            r["run_id"] == run
                && r["approval_id"] == approval
                && r["tool_name"] == TOOL
                && r["approved"] == true,
            "gateway_approval_result_mismatch",
        )
    }
    fn success(&self) -> Result<()> {
        need(
            self.count("run_finished") == 1
                && !self.has("run_error")
                && !self.has("approval_required")
                && !self.has("side_effect_unknown"),
            "gateway_terminal_contract",
        )?;
        need(
            self.terminal == "succeeded",
            &format!("gateway_terminal_status:{}", self.terminal),
        )
    }
}
impl AIPlatformAdapter {
    pub fn with_runtime_model_echo(mut self, enabled: bool) -> Self {
        self.strict_runtime_model_echo = enabled;
        self
    }

    pub async fn new(mut config: AdapterConfig) -> Result<Self> {
        config.gateway_base_url = config.gateway_base_url.trim_end_matches('/').into();
        let mut builder = reqwest::Client::builder().retry(reqwest::retry::never());
        if reqwest::Url::parse(&config.gateway_base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .is_some_and(|h| ["127.0.0.1", "localhost", "[::1]", "::1"].contains(&h.as_str()))
        {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        let mut out = Self {
            config,
            token: String::new(),
            sessions: BTreeMap::new(),
            client,
            strict_runtime_model_echo: true,
        };
        let login = out
            .request(
                "/auth/login",
                &json!({"email":out.config.email,"password":out.config.password}),
                false,
            )
            .await?;
        out.token = login["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("gateway_login_missing_token")?
            .into();
        out.config.password.clear();
        Ok(out)
    }
    async fn request(&self, path: &str, body: &Value, auth: bool) -> Result<Value> {
        let mut q = self
            .client
            .post(format!("{}{path}", self.config.gateway_base_url))
            .timeout(Duration::from_secs(30))
            .json(body);
        if auth {
            q = q.bearer_auth(&self.token);
        }
        let r = q
            .send()
            .await
            .map_err(|_| "http_transport:request_failed")?;
        if !r.status().is_success() {
            return Err(format!(
                "http_{}:{}",
                r.status().as_u16(),
                path.rsplit('/').next().unwrap_or("")
            )
            .into());
        }
        let p: Value = r.json().await.map_err(|_| "http_transport:invalid_json")?;
        need(p.is_object(), "http_response_not_object")?;
        Ok(p)
    }
    pub async fn start_task(&mut self, id: &str) -> Result<()> {
        let s = self
            .request(
                "/assistant/sessions",
                &json!({"metadata":{"benchmark_suite":"native-agent-parity/v1","task_id":id}}),
                true,
            )
            .await?;
        let sid = s["session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("gateway_session_missing_id")?;
        self.sessions.insert(id.into(), sid.into());
        Ok(())
    }
    pub fn turn_request_body(&self, task: &str, session: &str, prompt: &str) -> Result<Value> {
        Ok(
            json!({"message":prompt,"session_id":session,"model_id":self.config.model_id,"temperature":self.config.temperature,"max_tokens":self.config.max_tokens,"kb_mode":"off","web_search_enabled":false,"enable_task_planning":false,"thinking_level":self.config.thinking_level,"execution_profile":self.config.execution_profile,"memory_mode":"off","skills_enabled":false,"output_format":self.config.task_output_formats.get(task).ok_or("missing_task_output_format")?}),
        )
    }
    fn runtime(&self, s: &Stream) -> Result<()> {
        if !self.strict_runtime_model_echo {
            return need(
                s.failover.is_empty(),
                "gateway_model_failover_invalidates_comparison",
            );
        }
        need(
            s.runtime["model_id"] == self.config.model_id,
            "gateway_runtime_model_mismatch",
        )?;
        need(
            s.runtime["provider"].is_string(),
            "gateway_runtime_provider_missing",
        )?;
        need(
            s.failover.is_empty(),
            "gateway_model_failover_invalidates_comparison",
        )
    }
    async fn stream(&self, body: &Value, phase: &str, start: Instant) -> Result<Stream> {
        let phase_start = start.elapsed().as_secs_f64();
        let mut r = self
            .client
            .post(format!(
                "{}/assistant/chat/stream",
                self.config.gateway_base_url
            ))
            .bearer_auth(&self.token)
            .json(body)
            .timeout(Duration::from_secs(240))
            .send()
            .await
            .map_err(|_| "gateway_chat_transport")?;
        need(
            r.status().as_u16() == 200,
            &format!("gateway_chat_http_{}", r.status().as_u16()),
        )?;
        let mut s = Stream {
            terminal: "missing".into(),
            runtime: json!({}),
            ..Default::default()
        };
        let mut first = None;
        let mut buffer = Vec::new();
        while let Some(chunk) = r.chunk().await.map_err(|_| "gateway_chat_transport")? {
            buffer.extend_from_slice(&chunk);
            while let Some(i) = buffer.iter().position(|b| *b == b'\n') {
                let bytes = buffer.drain(..=i).collect::<Vec<_>>();
                self.parse_line(&bytes, phase, start, &mut first, &mut s)?;
            }
        }
        if !buffer.is_empty() {
            self.parse_line(&buffer, phase, start, &mut first, &mut s)?;
        }
        s.finished = start.elapsed().as_secs_f64();
        let visible = s
            .first_text
            .into_iter()
            .chain(s.first_thinking)
            .reduce(f64::min);
        let delta = |v: Option<f64>| v.map(|v| round(v - phase_start));
        let thinking_to_text = s
            .first_text
            .zip(s.first_thinking)
            .map(|(a, b)| round(a - b));
        s.timing = json!({"phase":phase,"started_offset_seconds":round(phase_start),"first_event_seconds":delta(first),"first_thinking_seconds":delta(s.first_thinking),"ttft_seconds":delta(visible),"first_text_seconds":delta(s.first_text),"thinking_to_visible_seconds":thinking_to_text,"thinking_to_text_seconds":thinking_to_text,"duration_seconds":round(s.finished-phase_start)});
        Ok(s)
    }
    fn parse_line(
        &self,
        bytes: &[u8],
        phase: &str,
        start: Instant,
        first: &mut Option<f64>,
        s: &mut Stream,
    ) -> Result<()> {
        let line = String::from_utf8_lossy(bytes);
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(());
        };
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }
        let event: Value = serde_json::from_str(data).map_err(|_| "gateway_invalid_sse_json")?;
        let at = start.elapsed().as_secs_f64();
        if first.is_none() {
            *first = Some(at);
        }
        s.event(event, phase, at)
    }
    pub async fn run_turn(&self, task: &str, prompt: &str) -> Result<TurnResult> {
        let session = self.sessions.get(task).ok_or("task_not_started")?;
        let body = self.turn_request_body(task, session, prompt)?;
        let start = Instant::now();
        let initial = self.stream(&body, "initial", start).await?;
        self.runtime(&initial)?;
        let mut streams = vec![initial];
        let mut approvals = vec![];
        let mut active = None;
        while streams.last().unwrap().has("approval_required") {
            need(
                approvals.len() < self.config.max_approval_rounds,
                "gateway_approval_round_limit_exceeded",
            )?;
            let current = streams.last().unwrap();
            current.approval_boundary()?;
            let p = &current.approvals[0];
            let run = identifier(p, "run_id")?;
            if let Some(ref old) = active {
                need(old == &run, "gateway_approval_run_changed")?;
            } else {
                active = Some(run.clone());
            }
            let approval = identifier(p, "approval_id")?;
            let at = Instant::now();
            let decision = self
                .request(
                    &format!("/assistant/approvals/{approval}"),
                    &json!({"approved":true,"reason":REASON}),
                    true,
                )
                .await?;
            let decision_duration = at.elapsed().as_secs_f64();
            let d = &decision["approval"];
            need(d.is_object(), "gateway_approval_receipt_missing")?;
            need(
                d["approval_id"] == approval
                    && d["run_id"] == run
                    && d["tool_name"] == TOOL
                    && d["status"] == "approved",
                "gateway_approval_receipt_mismatch",
            )?;
            let mut body = self.turn_request_body(task, session, RESUME)?;
            body["resume_run_id"] = run.clone().into();
            body["resume_approval_id"] = approval.clone().into();
            let resumed = self.stream(&body, "resumed", start).await?;
            resumed.resume(&run, &approval)?;
            self.runtime(&resumed)?;
            streams.push(resumed);
            approvals.push(json!({"tool_name":TOOL,"run_id_sha256":hash(&run),"approval_id_sha256":hash(&approval),"decision":"approved","decision_duration_seconds":round(decision_duration)}));
        }
        let last = streams.last().unwrap();
        last.success()?;
        let first_text = streams.iter().find_map(|s| s.first_text);
        let first_thinking = streams.iter().find_map(|s| s.first_thinking);
        let visible = first_text
            .into_iter()
            .chain(first_thinking)
            .reduce(f64::min);
        let events = streams
            .iter()
            .flat_map(|s| s.events.iter())
            .collect::<Vec<_>>();
        let text = streams
            .iter()
            .flat_map(|s| s.chunks.iter())
            .cloned()
            .collect::<String>()
            .trim()
            .to_string();
        let metadata = json!({"event_types":events,"lifecycle_events":streams.iter().flat_map(|s|s.lifecycle.iter()).collect::<Vec<_>>(),"timing":{"ttft_seconds":visible.map(round),"first_text_seconds":first_text.map(round),"first_thinking_seconds":first_thinking.map(round),"total_duration_seconds":round(last.finished),"phases":streams.iter().map(|s|&s.timing).collect::<Vec<_>>()},"approval":approvals.first(),"approvals":approvals,"session_id_sha256":hash(session),"runtime":last.runtime,"usage":streams.iter().rev().find_map(|s|s.usage.as_ref()),"failover_decisions":streams.iter().flat_map(|s|s.failover.iter()).collect::<Vec<_>>(),"thinking_level":self.config.thinking_level,"execution_profile":self.config.execution_profile,"memory_mode":"off","skills_enabled":false});
        Ok(TurnResult {
            text,
            duration_seconds: last.finished,
            terminal_status: last.terminal.clone(),
            metadata,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_and_receipt_contracts() {
        let mut s = Stream {
            terminal: "succeeded".into(),
            events: vec!["run_finished".into()],
            ..Default::default()
        };
        assert!(s.success().is_ok());
        s.events.push("run_finished".into());
        assert!(s.success().is_err());
    }
    #[test]
    fn lifecycle_ids_are_hashed() {
        let mut s = Stream::default();
        s.event(json!({"event_type":"approval_required","data":{"run_id":"r","approval_id":"a","tool_name":"execute_python_code","terminal_envelope":{"status":"blocked","resume_ready":true}}}),"initial",1.25).unwrap();
        assert_eq!(s.terminal, "blocked");
        assert_eq!(s.lifecycle[0]["run_id_sha256"], hash("r"));
        assert!(s.lifecycle[0].get("run_id").is_none());
        assert!(s.approval_boundary().is_err());
    }
    #[test]
    fn split_text_shapes() {
        let mut s = Stream::default();
        s.event(json!({"type":"text_delta","data":"a"}), "initial", 1.)
            .unwrap();
        s.event(
            json!({"type":"text_delta","data":{"content":"b"}}),
            "initial",
            2.,
        )
        .unwrap();
        assert_eq!(s.chunks, vec!["a", "b"]);
        assert_eq!(s.first_text, Some(1.));
    }
}
