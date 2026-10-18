//! Agent Runtime v2 conversation and exact-intent approval UI. This is a client,
//! never a second model/Agent loop. Only durable event GETs reconnect.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use ai_platform_client::{Client, Error, events::Event};
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
pub struct TurnOptions {
    pub model_id: String,
    pub reasoning_option: String,
    pub max_tokens: u64,
    pub temperature: f64,
    pub kb_datasets: String,
    pub kb_mode: String,
    pub kb_top_k: u64,
    pub kb_score_threshold: f64,
    pub web_search: bool,
    pub web_max_results: u64,
    pub file_paths: String,
    pub execution_profile: String,
    pub memory_mode: String,
    pub system_prompt: String,
    pub os_agent: bool,
    pub local_device: String,
    pub local_grants: String,
}
impl Default for TurnOptions {
    fn default() -> Self {
        Self {
            model_id: String::new(),
            reasoning_option: String::new(),
            max_tokens: 4096,
            temperature: 0.7,
            kb_datasets: String::new(),
            kb_mode: "off".into(),
            kb_top_k: 5,
            kb_score_threshold: 0.4,
            web_search: false,
            web_max_results: 5,
            file_paths: String::new(),
            execution_profile: "safe".into(),
            memory_mode: "auto".into(),
            system_prompt: String::new(),
            os_agent: false,
            local_device: String::new(),
            local_grants: String::new(),
        }
    }
}
impl TurnOptions {
    pub fn payload(&self, message: &str) -> Value {
        let mut body = json!({"message":message,"max_tokens":self.max_tokens,"temperature":self.temperature,"kb_dataset_ids":lines(&self.kb_datasets),"kb_mode":self.kb_mode,"kb_top_k":self.kb_top_k,"kb_score_threshold":self.kb_score_threshold,"web_search_enabled":self.web_search,"web_search_max_results":self.web_max_results,"file_paths":lines(&self.file_paths),"execution_profile":self.execution_profile,"memory_mode":self.memory_mode,"system_prompt":self.system_prompt,"os_agent_enabled":self.os_agent,"local_node_device_id":self.local_device,"local_node_grant_ids":lines(&self.local_grants)});
        if !self.model_id.is_empty() {
            body["model_id"] = json!(self.model_id);
        }
        if !self.reasoning_option.is_empty() {
            body["reasoning_option"] = json!(self.reasoning_option);
        }
        body
    }
}
struct ChatEntry {
    role: String,
    text: String,
}
enum Message {
    Started {
        generation: u64,
        thread: String,
        turn: String,
        session: String,
        url: String,
    },
    Event(u64, Event),
    Failed(u64, Error),
    AdmissionFailed(u64, ApiError),
    Stopped(u64, Result<Value, ApiError>),
}
pub struct ConversationEditor {
    pub attachments: super::attachments::AttachmentWorkspace,
    pub citation: super::source_preview::SourcePreview,
    citations: Vec<Value>,
    pub options: TurnOptions,
    pub prompt: String,
    pub session_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub status: String,
    pub active: bool,
    pub expired: bool,
    observing: bool,
    stop_pending: bool,
    generation: u64,
    cursor: u64,
    url: Option<String>,
    entries: Vec<ChatEntry>,
    thinking: String,
    tools: BTreeMap<String, Value>,
    artifacts: BTreeMap<String, Value>,
    seen_artifacts: HashSet<String>,
    redacted: bool,
    approval_id: Option<String>,
    preview_attempted_for: Option<String>,
    approval_preview: Value,
    approval_expires: Option<Instant>,
    approval_reviewed: bool,
    pub job: RemoteJob,
    pub history: Value,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    cancel: Option<watch::Sender<bool>>,
}
impl Default for ConversationEditor {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(32);
        Self {
            attachments: Default::default(),
            citation: Default::default(),
            citations: Vec::new(),
            options: TurnOptions::default(),
            prompt: String::new(),
            session_id: String::new(),
            thread_id: String::new(),
            turn_id: String::new(),
            status: String::new(),
            active: false,
            expired: false,
            observing: false,
            stop_pending: false,
            generation: 0,
            cursor: 0,
            url: None,
            entries: Vec::new(),
            thinking: String::new(),
            tools: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            seen_artifacts: HashSet::new(),
            redacted: false,
            approval_id: None,
            preview_attempted_for: None,
            approval_preview: Value::Null,
            approval_expires: None,
            approval_reviewed: false,
            job: RemoteJob::default(),
            history: Value::Null,
            tx,
            rx,
            cancel: None,
        }
    }
}
impl Drop for ConversationEditor {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            let _ = cancel.send(true);
        }
    }
}
impl ConversationEditor {
    fn preview(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let Some(id) = self.approval_id.clone() else {
            return;
        };
        if self.thread_id.is_empty() || self.job.busy {
            return;
        }
        self.approval_reviewed = false;
        self.preview_attempted_for = Some(id.clone());
        self.job.request(
            &format!("preview:{id}"),
            api,
            ApiRequest::get(&[
                "api",
                "v2",
                "agent",
                "threads",
                &self.thread_id,
                "approvals",
                &id,
            ]),
            runtime,
            ctx,
        );
    }
    fn accept_preview(&mut self, value: Value) {
        self.approval_expires = value["preview"]["expires_in_seconds"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 86400.0)
            .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
        self.approval_preview = value;
        self.approval_reviewed = false;
    }
    fn apply_event(&mut self, event: Event) -> bool {
        if event.sequence <= self.cursor {
            return false;
        }
        self.cursor = event.sequence;
        let mut needs_preview = false;
        for (kind, data) in project(&event.payload) {
            if kind == "source_access_revoked" || data["source_access_revoked"] == true {
                self.entries.clear();
                self.attachments.clear();
                self.citation.close();
                self.citations.clear();
                self.thinking.clear();
                self.tools.clear();
                self.artifacts.clear();
                self.approval_preview = Value::Null;
                self.approval_id = None;
                self.redacted = true;
                self.status = "Source access revoked. Start a new conversation.".into();
                continue;
            }
            match kind.as_str() {
                "context_retrieved" if !self.redacted => {
                    let selected = lines(&self.options.kb_datasets);
                    let fallback = data["dataset_id"].as_str().map(str::to_owned).or_else(|| {
                        if selected.len() == 1 {
                            Some(selected[0].clone())
                        } else {
                            None
                        }
                    });
                    if let Some(chunks) = data["chunks"].as_array() {
                        for chunk in chunks {
                            let dataset = chunk["dataset_id"]
                                .as_str()
                                .or_else(|| chunk["metadata"]["dataset_id"].as_str())
                                .map(str::to_owned)
                                .or_else(|| fallback.clone());
                            if let Some(dataset) = dataset {
                                if !selected.is_empty() && !selected.contains(&dataset) {
                                    continue;
                                }
                                let mut chunk = chunk.clone();
                                chunk["dataset_id"] = json!(dataset);
                                if !self.citations.contains(&chunk) {
                                    self.citations.push(chunk);
                                }
                            }
                        }
                    }
                }
                "text_delta" if !self.redacted => {
                    let text = data
                        .as_str()
                        .or_else(|| data["content"].as_str())
                        .unwrap_or("");
                    if let Some(entry) = self.entries.last_mut() {
                        if entry.role == "assistant" && entry.text.len() + text.len() <= 1024 * 1024
                        {
                            entry.text.push_str(text);
                        }
                    }
                }
                "thinking_delta" if !self.redacted => {
                    let text = data
                        .as_str()
                        .or_else(|| data["content"].as_str())
                        .unwrap_or("");
                    if self.thinking.len() + text.len() <= 1024 * 1024 {
                        self.thinking.push_str(text);
                    }
                }
                "approval_required" if !self.redacted => {
                    if let Some(id) = data["approval_id"].as_str().filter(|v| !v.is_empty()) {
                        self.approval_id = Some(id.into());
                        self.preview_attempted_for = None;
                        self.approval_preview = Value::Null;
                        self.approval_reviewed = false;
                        self.status = "Approval required".into();
                        needs_preview = true;
                    }
                }
                "approval_result" => {
                    let matches = data["approval_id"].as_str().is_none_or(|id| {
                        self.approval_id
                            .as_deref()
                            .is_none_or(|current| current == id)
                    });
                    if matches {
                        self.approval_id = None;
                        self.approval_preview = Value::Null;
                        self.status = "Approval resolved; awaiting next event".into();
                    }
                }
                "tool_call_start" | "tool_call_result" if !self.redacted => {
                    let id = ["tool_call_id", "tool_id", "call_id", "id"]
                        .iter()
                        .find_map(|key| data[*key].as_str())
                        .unwrap_or("tool");
                    self.tools.insert(id.into(), data);
                }
                "artifact_created" if !self.redacted => {
                    if let Some(id) = data["artifact_id"].as_str() {
                        if self.seen_artifacts.insert(id.into()) {
                            self.artifacts.insert(id.into(), data);
                        }
                    }
                }
                "run_finished" | "run_error" | "cancelled" => {
                    self.active = false;
                    self.observing = false;
                    self.stop_pending = false;
                    self.approval_id = None;
                    self.approval_preview = Value::Null;
                    self.status = if kind == "run_finished" {
                        "Turn completed"
                    } else if kind == "cancelled"
                        || data["status"] == "cancelled"
                        || data["terminal_envelope"]["status"] == "cancelled"
                    {
                        "Turn cancelled"
                    } else {
                        "Turn failed"
                    }
                    .into();
                }
                _ => {}
            }
        }
        if event.terminal {
            self.active = false;
            self.observing = false;
            self.stop_pending = false;
        }
        needs_preview
    }
    pub fn poll(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let mut refresh = false;
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Started {
                    generation,
                    thread,
                    turn,
                    session,
                    url,
                } if generation == self.generation => {
                    self.thread_id = thread;
                    self.turn_id = turn;
                    self.session_id = session;
                    self.url = Some(url);
                    self.observing = true;
                    self.status = "Streaming".into();
                }
                Message::Event(g, event) if g == self.generation => {
                    refresh |= self.apply_event(event)
                }
                Message::Failed(g, error) if g == self.generation => {
                    self.observing = false;
                    self.expired = error.status() == Some(401);
                    self.active = !self.turn_id.is_empty() && !self.expired;
                    self.status = format!(
                        "{error}. The turn was not resubmitted; reconnect or stop the existing turn."
                    );
                }
                Message::AdmissionFailed(g, error) if g == self.generation => {
                    self.observing = false;
                    self.expired = matches!(error, ApiError::Status(401));
                    self.active = false;
                    self.status = format!("{error}. No automatic resubmission.");
                }
                Message::Stopped(g, result) if g == self.generation => {
                    self.stop_pending = false;
                    self.expired = matches!(result, Err(ApiError::Status(401)));
                    self.status = match result {
                        Ok(_) => "Stop requested; awaiting terminal event".into(),
                        Err(e) => format!("Stop outcome: {e}"),
                    };
                }
                _ => {}
            }
        }
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                if let Some(id) = message.kind.strip_prefix("preview:") {
                    if self.approval_id.as_deref() == Some(id) {
                        self.accept_preview(value);
                    }
                } else if message.kind == "decision" {
                    if let Some(preview) = value.get("latest") {
                        self.accept_preview(preview.clone());
                    }
                    self.status = if value["changed"] == true {
                        "Action details changed. Review the current request before approving."
                    } else {
                        "Decision submitted; awaiting durable approval event."
                    }
                    .into();
                } else if message.kind == "history" {
                    self.history = value;
                }
            }
        }
        self.expired |= self.job.expired;
        if refresh
            || self.approval_id.is_some()
                && self.preview_attempted_for != self.approval_id
                && !self.job.busy
        {
            self.preview(api, runtime, ctx);
        }
    }
    fn send(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if self.active || self.prompt.trim().is_empty() || self.redacted {
            return;
        }
        if self.prompt.chars().count() > 200000 {
            self.status = "Message exceeds the input limit".into();
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.active = true;
        self.observing = true;
        self.cursor = 0;
        self.turn_id.clear();
        self.url = None;
        self.approval_id = None;
        self.approval_preview = Value::Null;
        self.thinking.clear();
        self.tools.clear();
        self.artifacts.clear();
        self.citations.clear();
        self.citation.close();
        self.seen_artifacts.clear();
        let prompt = std::mem::take(&mut self.prompt);
        let body = self.options.payload(&prompt);
        self.entries.push(ChatEntry {
            role: "user".into(),
            text: prompt,
        });
        self.entries.push(ChatEntry {
            role: "assistant".into(),
            text: String::new(),
        });
        self.status = "Submitting once".into();
        let api = api.clone();
        let session = self.session_id.clone();
        let existing = self.thread_id.clone();
        let model = self.options.model_id.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let (cancel, stop) = watch::channel(false);
        self.cancel = Some(cancel);
        runtime.spawn(async move {
            let admitted: Result<(String, String, String, String), ApiError> = async {
                let (thread, session) = if existing.is_empty() {
                    let mut body = json!({});
                    if !session.is_empty() {
                        body["session_id"] = json!(session);
                    }
                    if !model.is_empty() {
                        body["model_id"] = json!(model);
                    }
                    let result = api
                        .execute(ApiRequest::mutation(
                            Method::POST,
                            &["api", "v2", "agent", "threads"],
                            Some(body),
                        ))
                        .await?;
                    (
                        result["thread"]["thread_id"]
                            .as_str()
                            .ok_or(ApiError::Response)?
                            .to_owned(),
                        result["thread"]["session_id"]
                            .as_str()
                            .ok_or(ApiError::Response)?
                            .to_owned(),
                    )
                } else {
                    (existing, session)
                };
                let result = api
                    .execute(ApiRequest::mutation(
                        Method::POST,
                        &["api", "v2", "agent", "threads", &thread, "turns"],
                        Some(body),
                    ))
                    .await?;
                let turn = result["turn"]["id"]
                    .as_str()
                    .ok_or(ApiError::Response)?
                    .to_string();
                let url = result["turn"]["events_url"]
                    .as_str()
                    .ok_or(ApiError::Response)?
                    .to_string();
                Ok((thread, turn, session, url))
            }
            .await;
            match admitted {
                Ok((thread, turn, session, url)) => {
                    let _ = tx
                        .send(Message::Started {
                            generation,
                            thread: thread.clone(),
                            turn: turn.clone(),
                            session,
                            url: url.clone(),
                        })
                        .await;
                    ctx.request_repaint();
                    match api.runtime_client() {
                        Ok(client) => {
                            if let Err(e) =
                                observe(&client, &thread, &turn, &url, generation, &tx, &ctx, stop)
                                    .await
                            {
                                let _ = tx.send(Message::Failed(generation, e)).await;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Message::AdmissionFailed(generation, e)).await;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Message::AdmissionFailed(generation, e)).await;
                }
            }
            ctx.request_repaint();
        });
    }
    fn reconnect(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let (Some(relative), Ok(client)) = (self.url.clone(), api.runtime_client()) else {
            return;
        };
        let Ok(mut url) = client.events_url(&self.thread_id, &relative) else {
            return;
        };
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k != "after_sequence")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(pairs)
            .append_pair("after_sequence", &self.cursor.to_string());
        let relative = format!("{}?{}", url.path(), url.query().unwrap_or(""));
        self.observing = true;
        let thread = self.thread_id.clone();
        let turn = self.turn_id.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let generation = self.generation;
        let (cancel, stop) = watch::channel(false);
        self.cancel = Some(cancel);
        runtime.spawn(async move {
            if let Err(e) = observe(
                &client, &thread, &turn, &relative, generation, &tx, &ctx, stop,
            )
            .await
            {
                let _ = tx.send(Message::Failed(generation, e)).await;
                ctx.request_repaint();
            }
        });
    }
    fn decide(
        &mut self,
        approved: bool,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let Some(id) = self.approval_id.clone() else {
            return;
        };
        if self.job.busy
            || self.approval_preview["approval"]["status"] != "pending"
            || approved
                && (self.redacted
                    || !self.approval_reviewed
                    || self.approval_preview["preview"]["can_approve"] != true)
        {
            return;
        }
        let expected = self.approval_preview["preview"]["arguments_hash"].clone();
        let api = api.clone();
        let thread = self.thread_id.clone();
        self.approval_reviewed = false;
        self.job.start(
            "decision",
            async move {
                let latest = api
                    .execute(ApiRequest::get(&[
                        "api",
                        "v2",
                        "agent",
                        "threads",
                        &thread,
                        "approvals",
                        &id,
                    ]))
                    .await?;
                if latest["approval"]["status"] != "pending"
                    || approved
                        && (latest["preview"]["can_approve"] != true
                            || latest["preview"]["arguments_hash"] != expected)
                {
                    return Ok(json!({"changed":true,"latest":latest}));
                }
                let result = api
                    .execute(ApiRequest::mutation(
                        Method::POST,
                        &[
                            "api",
                            "v2",
                            "agent",
                            "threads",
                            &thread,
                            "approvals",
                            &id,
                            "decision",
                        ],
                        Some(json!({"approved":approved})),
                    ))
                    .await?;
                let latest = api
                    .execute(ApiRequest::get(&[
                        "api",
                        "v2",
                        "agent",
                        "threads",
                        &thread,
                        "approvals",
                        &id,
                    ]))
                    .await?;
                Ok(json!({"changed":false,"latest":latest,"result":result}))
            },
            runtime,
            ctx,
        );
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll(api, runtime, ctx);
        ui.heading("Conversation");
        if !self.redacted {
            if let Some(path) = self
                .attachments
                .ui_upload(ui, api, !self.active, runtime, ctx)
            {
                let mut paths = lines(&self.options.file_paths);
                if !paths.contains(&path) {
                    paths.push(path);
                }
                self.options.file_paths = paths.join("\n");
            }
            self.expired |= self.attachments.expired;
        }
        ui.label(&self.status);
        self.job.status_ui(ui);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.active && !self.job.busy,
                    egui::Button::new("New conversation"),
                )
                .clicked()
            {
                *self = Self::default();
            }
            ui.label(format!(
                "Thread: {} · turn: {} · cursor: {}",
                self.thread_id, self.turn_id, self.cursor
            ));
        });
        ui.add_enabled_ui(!self.active, |ui| {
            text_field(
                ui,
                "Session ID (blank creates a session)",
                &mut self.session_id,
            );
            if ui
                .add_enabled(
                    !self.session_id.is_empty() && !self.job.busy,
                    egui::Button::new("Read saved history"),
                )
                .clicked()
            {
                self.job.request(
                    "history",
                    api,
                    ApiRequest::get(&["api", "v1", "sessions", &self.session_id, "history"]),
                    runtime,
                    ctx,
                );
            }
            text_field(ui, "Model ID", &mut self.options.model_id);
            egui::CollapsingHeader::new("Turn options").show(ui, |ui| {
                text_field(ui, "Reasoning option", &mut self.options.reasoning_option);
                ui.add(
                    egui::DragValue::new(&mut self.options.max_tokens)
                        .range(1..=1_000_000)
                        .prefix("Max tokens "),
                );
                ui.add(
                    egui::Slider::new(&mut self.options.temperature, 0.0..=2.0).text("Temperature"),
                );
                ui.horizontal(|ui| {
                    ui.label("Knowledge mode");
                    for mode in ["off", "auto", "tool"] {
                        ui.selectable_value(&mut self.options.kb_mode, mode.into(), mode);
                    }
                });
                text_field(
                    ui,
                    "Dataset IDs, one per line",
                    &mut self.options.kb_datasets,
                );
                ui.add(
                    egui::DragValue::new(&mut self.options.kb_top_k)
                        .range(1..=100)
                        .prefix("Knowledge Top K "),
                );
                ui.add(
                    egui::Slider::new(&mut self.options.kb_score_threshold, 0.0..=1.0)
                        .text("Knowledge threshold"),
                );
                ui.checkbox(&mut self.options.web_search, "Web search");
                ui.add(
                    egui::DragValue::new(&mut self.options.web_max_results)
                        .range(1..=20)
                        .prefix("Web results "),
                );
                ui.label("Uploaded file paths, one per line");
                ui.text_edit_multiline(&mut self.options.file_paths);
                ui.horizontal(|ui| {
                    ui.label("Execution profile");
                    for profile in ["safe", "balanced", "power"] {
                        ui.selectable_value(
                            &mut self.options.execution_profile,
                            profile.into(),
                            profile,
                        );
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Memory mode");
                    for mode in ["auto", "strict", "off"] {
                        ui.selectable_value(&mut self.options.memory_mode, mode.into(), mode);
                    }
                });
                ui.label("System prompt");
                ui.text_edit_multiline(&mut self.options.system_prompt);
                ui.checkbox(
                    &mut self.options.os_agent,
                    "Enable granted Local Node capabilities",
                );
                text_field(ui, "Local Node device ID", &mut self.options.local_device);
                ui.label("Local grant IDs, one per line");
                ui.text_edit_multiline(&mut self.options.local_grants);
            });
            ui.add(
                egui::TextEdit::multiline(&mut self.prompt)
                    .desired_rows(4)
                    .hint_text("Message"),
            );
            if ui
                .add_enabled(
                    !self.redacted && !self.prompt.trim().is_empty(),
                    egui::Button::new("Send once"),
                )
                .clicked()
            {
                self.send(api, runtime, ctx);
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.active && !self.observing,
                    egui::Button::new("Reconnect observation"),
                )
                .clicked()
            {
                self.reconnect(api, runtime, ctx);
            }
            if ui
                .add_enabled(
                    self.active && !self.stop_pending && !self.turn_id.is_empty(),
                    egui::Button::new("Stop turn"),
                )
                .clicked()
            {
                self.stop_pending = true;
                let api = api.clone();
                let thread = self.thread_id.clone();
                let turn = self.turn_id.clone();
                let tx = self.tx.clone();
                let ctx = ctx.clone();
                let generation = self.generation;
                runtime.spawn(async move {
                    let result = api
                        .execute(ApiRequest::mutation(
                            Method::POST,
                            &[
                                "api",
                                "v2",
                                "agent",
                                "threads",
                                &thread,
                                "turns",
                                &format!("{turn}:interrupt"),
                            ],
                            Some(json!({"reason":"client_interrupt"})),
                        ))
                        .await;
                    let _ = tx.send(Message::Stopped(generation, result)).await;
                    ctx.request_repaint();
                });
            }
        });
        if self.approval_id.is_some() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong("Approval applies to this exact action once");
                if !self.approval_preview.is_null() {
                    json_view(ui, &self.approval_preview);
                }
                let remaining = self
                    .approval_expires
                    .map(|expiry| expiry.saturating_duration_since(Instant::now()).as_secs());
                if let Some(remaining) = remaining {
                    ui.label(format!("Expires in {remaining} seconds"));
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
                ui.checkbox(
                    &mut self.approval_reviewed,
                    "I reviewed the effect, target and parameters",
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.job.busy, egui::Button::new("Refresh action details"))
                        .clicked()
                    {
                        self.preview(api, runtime, ctx);
                    }
                    if ui
                        .add_enabled(
                            !self.job.busy
                                && self.approval_reviewed
                                && !self.redacted
                                && self.approval_preview["approval"]["status"] == "pending"
                                && self.approval_preview["preview"]["can_approve"] == true
                                && remaining != Some(0),
                            egui::Button::new("Approve once"),
                        )
                        .clicked()
                    {
                        self.decide(true, api, runtime, ctx);
                    }
                    if ui
                        .add_enabled(
                            !self.job.busy
                                && self.approval_preview["approval"]["status"] == "pending",
                            egui::Button::new("Reject"),
                        )
                        .clicked()
                    {
                        self.decide(false, api, runtime, ctx);
                    }
                });
            });
        }
        egui::ScrollArea::vertical()
            .id_salt("native_conversation")
            .max_height(420.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &self.entries {
                    ui.strong(&entry.role);
                    ui.label(&entry.text);
                    ui.separator();
                }
            });
        if !self.thinking.is_empty() {
            egui::CollapsingHeader::new("Reasoning").show(ui, |ui| {
                ui.label(&self.thinking);
            });
        }
        if !self.tools.is_empty() {
            egui::CollapsingHeader::new("Tool activity").show(ui, |ui| {
                for (id, value) in &self.tools {
                    ui.strong(id);
                    json_view(ui, value);
                }
            });
        }
        if !self.artifacts.is_empty() {
            egui::CollapsingHeader::new("Artifacts")
                .default_open(true)
                .show(ui, |ui| {
                    for value in self.artifacts.values() {
                        json_view(ui, value);
                    }
                });
        }
        if !self.history.is_null() {
            egui::CollapsingHeader::new("Saved history")
                .show(ui, |ui| json_view(ui, &self.history));
        }
        if !self.redacted {
            let artifacts = self.artifacts.values().cloned().collect::<Vec<_>>();
            self.attachments
                .ui_artifacts(ui, api, &self.session_id, &artifacts, runtime, ctx);
            for chunk in &self.citations {
                let dataset = chunk["dataset_id"].as_str().unwrap_or("");
                if ui
                    .button(format!("Open source {}", chunk["document_id"]))
                    .clicked()
                {
                    self.citation
                        .open(super::source_preview::citation_identity(dataset, chunk));
                }
            }
            self.citation.ui(ui, api, runtime, ctx);
            self.expired |= self.attachments.expired || self.citation.job.expired;
        }
    }
}
async fn observe(
    client: &Client,
    thread: &str,
    turn: &str,
    url: &str,
    generation: u64,
    tx: &mpsc::Sender<Message>,
    ctx: &egui::Context,
    stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let (events, mut receiver) = mpsc::channel(16);
    let stream = client.watch_turn(thread, turn, url, events, stop);
    tokio::pin!(stream);
    loop {
        tokio::select! {result=&mut stream=>{while let Ok(event)=receiver.try_recv(){tx.send(Message::Event(generation,event?)).await.map_err(|_|Error::Transport)?;}ctx.request_repaint();return result;},event=receiver.recv()=>if let Some(event)=event{tx.send(Message::Event(generation,event?)).await.map_err(|_|Error::Transport)?;ctx.request_repaint();}}
    }
}
fn text_content(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.into();
    }
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v["text"].as_str().or_else(|| v["content"].as_str()))
                .collect::<String>()
        })
        .unwrap_or_default()
}
fn find_artifact(value: &Value, depth: usize) -> Option<Value> {
    if depth > 4 {
        return None;
    }
    let decoded = if let Some(s) = value.as_str() {
        serde_json::from_str::<Value>(s).ok()?
    } else {
        value.clone()
    };
    let map = decoded.as_object()?;
    if map
        .get("artifact_id")
        .or_else(|| map.get("artifactId"))
        .and_then(Value::as_str)
        .is_some()
    {
        let mut artifact = decoded.clone();
        if artifact["artifact_id"].is_null() {
            artifact["artifact_id"] = artifact["artifactId"].clone();
        }
        return Some(artifact);
    }
    for key in [
        "artifact",
        "result",
        "output",
        "broker_response",
        "data",
        "payload",
    ] {
        if let Some(artifact) = find_artifact(&decoded[key], depth + 1) {
            return Some(artifact);
        }
    }
    for key in ["content", "content_items", "contentItems"] {
        if let Some(items) = decoded[key].as_array() {
            for item in items {
                if let Some(artifact) = find_artifact(
                    item.get("text")
                        .or_else(|| item.get("content"))
                        .unwrap_or(item),
                    depth + 1,
                ) {
                    return Some(artifact);
                }
            }
        }
    }
    None
}
fn project(value: &Value) -> Vec<(String, Value)> {
    let kind = value["event_type"].as_str().unwrap_or("");
    let item = if ["rollout/item", "item"].contains(&kind) {
        &value["data"]
    } else {
        value
    };
    let mut result = Vec::new();
    if !kind.is_empty() && !matches!(kind, "rollout/item" | "item") {
        result.push((kind.into(), value["data"].clone()));
    } else {
        let payload = item
            .get("payload")
            .filter(|v| v.is_object())
            .unwrap_or(item);
        let text = {
            let content = text_content(&payload["content"]);
            if !content.is_empty() {
                content
            } else {
                payload["message"]
                    .as_str()
                    .or_else(|| payload["text"].as_str())
                    .unwrap_or("")
                    .into()
            }
        };
        let projected = if !text.is_empty()
            && (payload["role"] == "assistant" || payload["type"] == "agent_message")
        {
            Some(("text_delta".into(), json!(text)))
        } else if !text.is_empty()
            && (payload["role"] == "reasoning" || payload["type"] == "reasoning")
        {
            Some(("thinking_delta".into(), json!(text)))
        } else if payload
            .get("approval_id")
            .or_else(|| payload.get("approvalId"))
            .is_some()
        {
            let mut data = payload.clone();
            if data["approval_id"].is_null() {
                data["approval_id"] = data["approvalId"].clone();
            }
            Some(("approval_required".into(), data))
        } else if let Some(artifact) = find_artifact(payload, 0) {
            Some(("artifact_created".into(), artifact))
        } else {
            let tool = payload["name"]
                .as_str()
                .or_else(|| payload["tool"].as_str());
            if tool.is_some()
                || [
                    "function_call",
                    "tool_use",
                    "command_execution",
                    "mcp_tool_call",
                ]
                .contains(&item["type"].as_str().unwrap_or(""))
            {
                let status = item["status"]
                    .as_str()
                    .or_else(|| payload["status"].as_str())
                    .unwrap_or("");
                Some((
                    if ["completed", "succeeded", "failed", "error", "cancelled"].contains(&status)
                    {
                        "tool_call_result"
                    } else {
                        "tool_call_start"
                    }
                    .into(),
                    json!({"tool_call_id":item["id"].as_str().or_else(||payload["id"].as_str()),"tool_name":tool.unwrap_or("tool"),"arguments":payload.get("arguments").or_else(||payload.get("input")),"result":payload.get("result").or_else(||payload.get("output")),"status":status}),
                ))
            } else {
                None
            }
        };
        if let Some(projected) = projected {
            result.push(projected);
        }
    }
    if !result.iter().any(|(k, _)| k == "artifact_created") {
        if let Some(artifact) = find_artifact(value, 0) {
            result.push(("artifact_created".into(), artifact));
        }
    }
    result
}

#[cfg(test)]
mod client_problem_tests {
    use super::*;
    fn problem(status: u16) -> Error {
        let bytes=serde_json::to_vec(&json!({"type":"about:blank","title":"private-marker","status":status,"detail":"private-marker"})).unwrap();
        Error::Problem(Box::new(
            ai_platform_client::problem::RemoteProblem::decode(status, &bytes).unwrap(),
        ))
    }
    #[test]
    fn direct_client_problem_follows_real_generation_and_session_fences() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let api = DesktopApi::new("https://gateway.example", "synthetic-token".into()).unwrap();
        let ctx = egui::Context::default();
        for status in [401, 404, 408, 500, 503] {
            let mut editor = ConversationEditor::default();
            editor.generation = 7;
            editor.turn_id = "existing-turn".into();
            editor.thread_id = "existing-thread".into();
            editor.active = true;
            editor.observing = true;
            assert!(editor.tx.try_send(Message::Failed(6, problem(401))).is_ok());
            editor.poll(&api, &runtime, &ctx);
            assert!(!editor.expired && editor.active && editor.observing);
            assert!(
                editor
                    .tx
                    .try_send(Message::Failed(7, problem(status)))
                    .is_ok()
            );
            editor.poll(&api, &runtime, &ctx);
            assert_eq!(editor.expired, status == 401);
            assert_eq!(editor.active, status != 401);
            assert!(!editor.observing);
            assert_eq!(editor.turn_id, "existing-turn");
            assert!(editor.status.contains("turn was not resubmitted"));
            assert!(!editor.status.contains("private-marker"));
            assert!(editor.rx.try_recv().is_err());
        }
    }
}
