use super::super::api::{ApiError, ApiRequest, DesktopApi};
use eframe::egui;
use serde_json::Value;
use std::{
    future::Future,
    sync::mpsc::{self, Receiver, Sender},
};
pub struct JobResult {
    pub kind: String,
    pub result: Result<Value, ApiError>,
}
pub struct RemoteJob {
    pub busy: bool,
    pub status: String,
    pub expired: bool,
    pub outcome_unknown: bool,
    pub current_kind: String,
    abort: Option<tokio::task::AbortHandle>,
    sender: Sender<JobResult>,
    receiver: Receiver<JobResult>,
}
impl Default for RemoteJob {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            busy: false,
            status: String::new(),
            expired: false,
            outcome_unknown: false,
            current_kind: String::new(),
            abort: None,
            sender,
            receiver,
        }
    }
}
impl RemoteJob {
    pub fn start<F>(
        &mut self,
        kind: &str,
        future: F,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) where
        F: Future<Output = Result<Value, ApiError>> + Send + 'static,
    {
        if self.busy {
            return;
        }
        self.busy = true;
        self.current_kind = kind.into();
        self.status = "Working…".into();
        let sender = self.sender.clone();
        let kind = kind.into();
        let ctx = ctx.clone();
        let task = runtime.spawn(async move {
            let _ = sender.send(JobResult {
                kind,
                result: future.await,
            });
            ctx.request_repaint();
        });
        self.abort = Some(task.abort_handle());
    }
    pub fn request(
        &mut self,
        kind: &str,
        api: &DesktopApi,
        request: ApiRequest,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let api = api.clone();
        self.start(
            kind,
            async move { api.execute(request).await },
            runtime,
            ctx,
        );
    }
    pub fn poll(&mut self) -> Option<JobResult> {
        let message = self.receiver.try_recv().ok()?;
        self.abort = None;
        self.busy = false;
        match &message.result {
            Ok(_) => {
                self.status = "Loaded".into();
                if message.kind.starts_with("load") || message.kind == "preview" {
                    self.outcome_unknown = false;
                }
            }
            Err(ApiError::Status(401)) => {
                self.expired = true;
                self.status = "Session expired".into();
            }
            Err(ApiError::Status(409 | 412)) => {
                self.status = "The server record changed. Reload before saving again.".into()
            }
            Err(error) => {
                self.outcome_unknown = matches!(
                    error,
                    ApiError::Transport | ApiError::Response | ApiError::Status(408 | 500..=599)
                );
                self.status = if self.outcome_unknown {
                    "Outcome unknown. Reload the record before repeating a change.".into()
                } else {
                    error.to_string()
                };
            }
        }
        Some(message)
    }
    pub fn cancel_observation(&mut self) {
        if let Some(task) = self.abort.take() {
            task.abort();
        }
        let expired = self.expired;
        *self = Self::default();
        self.expired = expired;
        self.status = "Request observation closed; completed server work is not undone".into();
    }
    pub fn status_ui(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.busy {
                ui.spinner();
            }
            ui.label(&self.status);
        });
    }
}
pub fn json_view(ui: &mut egui::Ui, value: &Value) {
    ui.add(
        egui::TextEdit::multiline(&mut serde_json::to_string_pretty(value).unwrap_or_default())
            .code_editor()
            .interactive(false)
            .desired_rows(12),
    );
}
pub fn text_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
    });
}
pub fn json_object(text: &str) -> Result<Value, ApiError> {
    serde_json::from_str::<Value>(text)
        .map_err(|_| ApiError::Request)
        .and_then(|v| {
            if v.is_object() {
                Ok(v)
            } else {
                Err(ApiError::Request)
            }
        })
}
pub fn lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A frozen request reviewed independently from editable form fields.
#[derive(Default)]
pub struct ReviewedAction {
    request: Option<ApiRequest>,
    kind: String,
    label: String,
    checked: bool,
    sensitive: bool,
}
impl ReviewedAction {
    pub fn prepare(&mut self, kind: &str, label: &str, request: ApiRequest) {
        self.prepare_sensitive(kind, label, request, false);
    }
    pub fn prepare_sensitive(
        &mut self,
        kind: &str,
        label: &str,
        request: ApiRequest,
        sensitive: bool,
    ) {
        self.request = Some(request);
        self.kind = kind.into();
        self.label = label.into();
        self.checked = false;
        self.sensitive = sensitive;
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        job: &mut RemoteJob,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let mut submit = false;
        let mut cancel = false;
        if let Some(request) = &self.request {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong(&self.label); ui.label(format!("{} /{}", request.method, request.segments.join("/")));
                if !self.sensitive { if let Some(body) = &request.body { json_view(ui, body); } } else { ui.label("Credential input is hidden and will be sent only with this reviewed request."); }
                if !request.query.is_empty() { for (key, value) in &request.query { ui.label(format!("{key}: {value}")); } }
                ui.checkbox(&mut self.checked, "I confirm this exact operation and target");
                submit = ui.add_enabled(self.checked && !job.busy && !job.outcome_unknown, egui::Button::new("Submit once")).clicked();
                cancel = ui.button("Cancel review").clicked();
            });
        }
        if cancel {
            self.clear();
        }
        if submit {
            if let Some(request) = self.request.take() {
                job.request(&self.kind, api, request, runtime, ctx);
            }
            self.clear();
        }
    }
}
pub fn select_text(ui: &mut egui::Ui, id: &str, value: &mut String, choices: &[&str]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.as_str())
        .show_ui(ui, |ui| {
            for choice in choices {
                ui.selectable_value(value, (*choice).into(), *choice);
            }
        });
}
pub fn optional_text(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        Value::String(value.trim().into())
    }
}
pub fn string_list(value: &Value) -> String {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
pub fn json_array(text: &str) -> Result<Value, ApiError> {
    serde_json::from_str::<Value>(text)
        .map_err(|_| ApiError::Request)
        .and_then(|v| {
            if v.is_array() {
                Ok(v)
            } else {
                Err(ApiError::Request)
            }
        })
}
/// Issued secrets stay in ephemeral UI state; no generic result viewer or disk persistence.
#[derive(Default)]
pub struct IssuedSecret {
    value: String,
    reveal: bool,
}
impl IssuedSecret {
    pub fn set(&mut self, value: String) {
        self.value = value;
        self.reveal = false;
    }
    pub fn clear(&mut self) {
        self.value.clear();
        self.reveal = false;
    }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if self.value.is_empty() {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("Copy this credential now. It will not be shown again after dismissal.");
            ui.checkbox(&mut self.reveal, "Show credential");
            ui.add(
                egui::TextEdit::singleline(&mut self.value)
                    .password(!self.reveal)
                    .interactive(false),
            );
            if ui.button("Copy credential").clicked() {
                ui.ctx().copy_text(self.value.clone());
            }
            if ui.button("I saved it; dismiss").clicked() {
                self.clear();
            }
        });
    }
}
