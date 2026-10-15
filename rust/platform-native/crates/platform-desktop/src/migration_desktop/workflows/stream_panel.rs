use super::super::{
    api::{ApiError, ApiRequest, DesktopApi},
    stream::StreamEvent,
};
use super::common::{RemoteJob, json_view};
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
enum Message {
    Event(StreamEvent),
    Ended(Result<(), ApiError>),
}
pub struct StreamPanel {
    pub source_dataset: String,
    artifacts: super::attachments::AttachmentWorkspace,
    citation: super::source_preview::SourcePreview,
    pub active: bool,
    pub expired: bool,
    pub terminal: bool,
    pub status: String,
    pub text: String,
    pub events: Vec<Value>,
    pub last_event_id: Option<String>,
    pub run_id: Option<String>,
    pub thread_id: Option<String>,
    pub approval_id: Option<String>,
    approval_preview: Value,
    approval_expires: Option<Instant>,
    reviewed: bool,
    pub job: RemoteJob,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    cancel: Option<watch::Sender<bool>>,
    stop_on_terminal: bool,
}
impl Default for StreamPanel {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(32);
        Self {
            source_dataset: String::new(),
            artifacts: Default::default(),
            citation: Default::default(),
            active: false,
            expired: false,
            terminal: false,
            status: String::new(),
            text: String::new(),
            events: Vec::new(),
            last_event_id: None,
            run_id: None,
            thread_id: None,
            approval_id: None,
            approval_preview: Value::Null,
            approval_expires: None,
            reviewed: false,
            job: RemoteJob::default(),
            tx,
            rx,
            cancel: None,
            stop_on_terminal: true,
        }
    }
}
impl Drop for StreamPanel {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            let _ = cancel.send(true);
        }
    }
}
impl StreamPanel {
    pub fn start(
        &mut self,
        api: &DesktopApi,
        request: ApiRequest,
        stop_on_terminal: bool,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if self.active {
            return;
        }
        *self = Self::default();
        self.active = true;
        self.stop_on_terminal = stop_on_terminal;
        self.status = "Streaming".into();
        let api = api.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let (cancel, stop) = watch::channel(false);
        self.cancel = Some(cancel);
        runtime.spawn(async move{let(sender,mut receiver)=mpsc::channel(16);let future=api.stream_once(request,sender,stop);tokio::pin!(future);let result=loop{tokio::select!{result=&mut future=>{while let Ok(event)=receiver.try_recv(){match event{Ok(event)=>{let _=tx.send(Message::Event(event)).await;},Err(error)=>{let _=tx.send(Message::Ended(Err(error))).await;}}}break result;},event=receiver.recv()=>if let Some(event)=event{match event{Ok(event)=>{let _=tx.send(Message::Event(event)).await;},Err(error)=>{let _=tx.send(Message::Ended(Err(error))).await;}}ctx.request_repaint();}}};let _=tx.send(Message::Ended(result)).await;ctx.request_repaint();});
    }
    pub fn detach(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(true);
        }
        self.active = false;
        self.status = "Observation closed. Check the saved run to establish its outcome.".into();
    }
    fn load_approval(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let (Some(thread), Some(id)) = (&self.thread_id, &self.approval_id) {
            self.job.request(
                "preview",
                api,
                ApiRequest::get(&["api", "v2", "agent", "threads", thread, "approvals", id]),
                runtime,
                ctx,
            );
        }
    }
    fn accept_preview(&mut self, value: Value) {
        self.approval_expires = value["preview"]["expires_in_seconds"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 86400.0)
            .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
        self.approval_preview = value;
        self.reviewed = false;
    }
    pub fn poll(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let mut approval = false;
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Event(event) => {
                    self.last_event_id = event.id;
                    let kind = event.data["event_type"]
                        .as_str()
                        .or_else(|| event.data["event"].as_str())
                        .unwrap_or(&event.event)
                        .to_string();
                    let data = event.data.get("data").unwrap_or(&event.data);
                    if matches!(kind.as_str(), "delta" | "text_delta") {
                        let text = data
                            .as_str()
                            .or_else(|| data["content"].as_str())
                            .or_else(|| event.data["content"].as_str())
                            .unwrap_or("");
                        if self.text.len() + text.len() <= 1024 * 1024 {
                            self.text.push_str(text);
                        }
                    }
                    if kind == "run_started" {
                        self.run_id = data["run_id"]
                            .as_str()
                            .or_else(|| data["task_id"].as_str())
                            .or_else(|| event.data["run_id"].as_str())
                            .map(str::to_owned);
                    }
                    if kind == "approval_required" {
                        self.approval_id = data["approval_id"].as_str().map(str::to_owned);
                        self.thread_id = data["thread_id"].as_str().map(str::to_owned);
                        approval = true;
                    }
                    if kind == "approval_result" {
                        self.approval_id = None;
                        self.approval_preview = Value::Null;
                    }
                    if kind == "source_access_revoked" || data["source_access_revoked"] == true {
                        self.text.clear();
                        self.events.clear();
                        self.artifacts.clear();
                        self.citation.close();
                        self.approval_id = None;
                        self.approval_preview = Value::Null;
                        self.status = "Source access revoked; content hidden".into();
                        self.detach();
                        return;
                    }
                    if matches!(
                        kind.as_str(),
                        "done" | "error" | "run_finished" | "run_error" | "cancelled"
                    ) {
                        self.terminal = true;
                        self.approval_id = None;
                        if self.stop_on_terminal {
                            if let Some(cancel) = self.cancel.take() {
                                let _ = cancel.send(true);
                            }
                            self.active = false;
                        }
                        self.status = format!("Stream ended: {kind}");
                    }
                    if self.events.len() < 1000 {
                        self.events.push(event.data);
                    }
                }
                Message::Ended(result) => {
                    self.active = false;
                    match result{Ok(())if self.terminal=>{},Ok(())=>self.status=if self.approval_id.is_some(){"Waiting for approval"}else{"Stream closed before a terminal event. Inspect the saved run before resubmitting."}.into(),Err(error)=>{self.expired=matches!(error,ApiError::Status(401));self.status=error.to_string();}}
                }
            }
        }
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                if message.kind == "preview" {
                    self.accept_preview(value);
                } else if message.kind == "decision" {
                    self.accept_preview(value["latest"].clone());
                    if value["changed"] == true {
                        self.status =
                            "Action details changed. Review them before another decision.".into();
                    }
                } else if message.kind == "cancel_run" {
                    self.detach();
                    self.status =
                        "Cancellation requested. Refresh the saved run to confirm the outcome."
                            .into();
                } else if message.kind == "load_run" {
                    self.events.push(value);
                }
            }
        }
        self.expired |= self.job.expired;
        if approval && !self.job.busy {
            self.load_approval(api, runtime, ctx);
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll(api, runtime, ctx);
        ui.label(&self.status);
        self.job.status_ui(ui);
        ui.horizontal(|ui| {
            if self.active && ui.button("Close observation").clicked() {
                self.detach();
            }
            if let Some(run) = &self.run_id {
                if ui
                    .add_enabled(!self.job.busy, egui::Button::new("Refresh saved run"))
                    .clicked()
                {
                    self.job.request(
                        "load_run",
                        api,
                        ApiRequest::get(&["api", "v1", "assistant", "runs", run]),
                        runtime,
                        ctx,
                    );
                }
                if ui
                    .add_enabled(
                        !self.job.busy && !self.terminal,
                        egui::Button::new("Stop saved run"),
                    )
                    .clicked()
                {
                    self.job.request(
                        "cancel_run",
                        api,
                        ApiRequest::mutation(
                            Method::POST,
                            &["api", "v1", "assistant", "tasks", run, "cancel"],
                            Some(json!({"reason":"user_requested_stop"})),
                        ),
                        runtime,
                        ctx,
                    );
                }
            }
        });
        if self.approval_id.is_some() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong("Review this exact action once");
                json_view(ui, &self.approval_preview);
                ui.checkbox(
                    &mut self.reviewed,
                    "I reviewed effect, target and parameters",
                );
                let expired = self
                    .approval_expires
                    .is_some_and(|expiry| Instant::now() >= expiry);
                ui.horizontal(|ui| {
                    if ui.button("Refresh approval").clicked() && !self.job.busy {
                        self.load_approval(api, runtime, ctx);
                    }
                    for (approved, label) in [(true, "Approve once"), (false, "Reject")] {
                        let allowed = !self.job.busy
                            && self.approval_preview["approval"]["status"] == "pending"
                            && (!approved
                                || self.reviewed
                                    && !expired
                                    && self.approval_preview["preview"]["can_approve"] == true);
                        if ui.add_enabled(allowed, egui::Button::new(label)).clicked() {
                            if let (Some(thread), Some(id)) =
                                (self.thread_id.clone(), self.approval_id.clone())
                            {
                                let api = api.clone();
                                let expected =
                                    self.approval_preview["preview"]["arguments_hash"].clone();
                                self.reviewed = false;
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
                                                    || latest["preview"]["arguments_hash"]
                                                        != expected)
                                        {
                                            return Ok(json!({"changed":true,"latest":latest}));
                                        }
                                        api.execute(ApiRequest::mutation(
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
                                        Ok(json!({"changed":false,"latest":latest}))
                                    },
                                    runtime,
                                    ctx,
                                );
                            }
                        }
                    }
                });
            });
        }
        if !self.text.is_empty() {
            ui.label(&self.text);
        }
        if !self.events.is_empty() {
            egui::CollapsingHeader::new("Stream events, citations and artifacts").show(ui, |ui| {
                for event in &self.events {
                    json_view(ui, event);
                }
            });
        }
        let mut artifacts = Vec::new();
        let mut citations = Vec::new();
        for event in &self.events {
            collect_preview_items(
                event,
                &self.source_dataset,
                0,
                &mut artifacts,
                &mut citations,
            );
        }
        self.artifacts
            .ui_artifacts(ui, api, "", &artifacts, runtime, ctx);
        for (dataset, chunk) in citations {
            if ui
                .button(format!("Open saved source {}", chunk["document_id"]))
                .clicked()
            {
                self.citation
                    .open(super::source_preview::citation_identity(&dataset, &chunk));
            }
        }
        self.citation.ui(ui, api, runtime, ctx);
        self.expired |= self.artifacts.expired || self.citation.job.expired;
    }
}

fn collect_preview_items(
    value: &Value,
    fallback: &str,
    depth: usize,
    artifacts: &mut Vec<Value>,
    citations: &mut Vec<(String, Value)>,
) {
    if depth > 5 {
        return;
    }
    if value["artifact_id"].is_string() {
        artifacts.push(value.clone());
    }
    let dataset = value["dataset_id"]
        .as_str()
        .or_else(|| value["metadata"]["dataset_id"].as_str())
        .unwrap_or(fallback);
    if value["document_id"].is_string()
        && (value["text"].is_string() || value["content"].is_string())
        && !dataset.is_empty()
    {
        citations.push((dataset.into(), value.clone()));
    }
    for key in ["data", "result", "artifact", "output", "response"] {
        if value[key].is_object() {
            collect_preview_items(&value[key], dataset, depth + 1, artifacts, citations);
        }
    }
    for key in [
        "results",
        "chunks",
        "retrieved_chunks",
        "citations",
        "artifacts",
    ] {
        if let Some(rows) = value[key].as_array() {
            for row in rows {
                collect_preview_items(row, dataset, depth + 1, artifacts, citations);
            }
        }
    }
}
