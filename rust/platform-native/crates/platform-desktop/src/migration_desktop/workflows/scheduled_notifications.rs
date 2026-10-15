//! Owner-scoped outcome inbox. Reads never acknowledge or relaunch an execution.
use super::{super::api::{ApiError, ApiRequest, DesktopApi}, common::*};
use ai_platform_client::scheduled_notifications::{
    self, NotificationAcknowledgements, NotificationCursor, NotificationFilter,
    NotificationQuery, ScheduledNotification, ScheduledNotificationPage,
};
use eframe::egui;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const PAGE_SIZE: u16 = 50;
const MAX_PAGE_HISTORY: usize = 128;

struct ReadChange {
    ids: Vec<String>,
    read: bool,
}
#[derive(Default)]
struct ExecutionOutput {
    text: Vec<String>,
    revoked: bool,
    limited: bool,
}
impl ExecutionOutput {
    fn parse(value: Value, session: &str, run: &str) -> Result<Self, ApiError> {
        if value["session_id"].as_str() != Some(session) {
            return Err(ApiError::Response);
        }
        let messages = value["messages"].as_array().ok_or(ApiError::Response)?;
        let total = value["total"].as_u64().ok_or(ApiError::Response)?;
        if messages.len() > 200 || total < messages.len() as u64 {
            return Err(ApiError::Response);
        }
        let mut result = Self { limited: total > messages.len() as u64, ..Self::default() };
        for message in messages {
            if message["metadata"]["runtime_run_id"].as_str() != Some(run) {
                continue;
            }
            if message["metadata"]["source_access_revoked"].as_bool() == Some(true) {
                result.revoked = true;
                continue;
            }
            if message["role"].as_str() == Some("assistant") {
                let content = message["content"].as_str().ok_or(ApiError::Response)?;
                if !content.trim().is_empty() {
                    result.text.push(content.into());
                }
            }
        }
        Ok(result)
    }
}

pub struct ScheduledNotifications {
    page_job: RemoteJob,
    detail_job: RemoteJob,
    change_job: RemoteJob,
    output_job: RemoteJob,
    filter: NotificationFilter,
    cursors: Vec<Option<NotificationCursor>>,
    loading_query: Option<NotificationQuery>,
    page: Option<ScheduledNotificationPage>,
    selected: String,
    open_id: String,
    loading_id: String,
    detail: Option<ScheduledNotification>,
    pending_change: Option<ReadChange>,
    output_target: Option<(String, String, String)>,
    output: Option<ExecutionOutput>,
    active: bool,
    next_refresh: Instant,
}
impl Default for ScheduledNotifications {
    fn default() -> Self {
        Self {
            page_job: RemoteJob::default(),
            detail_job: RemoteJob::default(),
            change_job: RemoteJob::default(),
            output_job: RemoteJob::default(),
            filter: NotificationFilter::All,
            cursors: vec![None],
            loading_query: None,
            page: None,
            selected: String::new(),
            open_id: String::new(),
            loading_id: String::new(),
            detail: None,
            pending_change: None,
            output_target: None,
            output: None,
            active: false,
            next_refresh: Instant::now(),
        }
    }
}
impl Drop for ScheduledNotifications {
    fn drop(&mut self) {
        self.page_job.cancel_observation();
        self.detail_job.cancel_observation();
        self.output_job.cancel_observation();
        self.cancel_reconciliation();
        // A submitted change is not cancelled or retried. Its old receiver is
        // dropped with this account's workspace; it cannot update a new account.
    }
}
impl ScheduledNotifications {
    fn cancel_reconciliation(&mut self) {
        if self.change_job.busy && self.change_job.current_kind == "load affected read states" {
            self.change_job.cancel_observation();
            self.change_job.outcome_unknown = self.pending_change.is_some();
            self.change_job.status = "Read-state check closed. Check affected receipts before another change.".into();
        }
    }
    pub fn expired(&self) -> bool {
        self.page_job.expired || self.detail_job.expired
            || self.change_job.expired || self.output_job.expired
    }
    pub fn close_observation(&mut self) {
        if !self.active { return; }
        self.active = false;
        self.cancel_reconciliation();
        self.page_job.cancel_observation();
        self.detail_job.cancel_observation();
        self.output_job.cancel_observation();
        self.loading_query = None;
        self.loading_id.clear();
        self.page = None;
        self.detail = None;
        self.output = None;
        self.output_target = None;
    }
    pub fn refresh(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if self.change_job.busy { return; }
        self.page_job.cancel_observation();
        self.page = None;
        let query = NotificationQuery {
            filter: self.filter,
            limit: PAGE_SIZE,
            cursor: self.cursors.last().cloned().flatten(),
        };
        match query.request() {
            Ok(request) => {
                self.loading_query = Some(query);
                self.page_job.request("load updates", api, request, runtime, ctx);
            }
            Err(_) => self.page_job.status = "Invalid update page. Return to Latest.".into(),
        }
        self.next_refresh = Instant::now() + Duration::from_secs(15);
    }
    fn load_selected(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        self.detail_job.cancel_observation();
        self.output_job.cancel_observation();
        self.detail_job.status.clear();
        self.output_job.status.clear();
        self.detail = None;
        self.output = None;
        self.output_target = None;
        self.loading_id = self.selected.clone();
        if self.selected.is_empty() { return; }
        match scheduled_notifications::get_request(&self.selected) {
            Ok(request) => self.detail_job.request("load selected update", api, request, runtime, ctx),
            Err(_) => self.detail_job.status = "Enter a valid notification ID.".into(),
        }
    }
    fn select(&mut self, id: String, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if self.change_job.busy { return; }
        self.selected = id;
        self.open_id = self.selected.clone();
        self.load_selected(api, runtime, ctx);
    }
    fn change(&mut self, ids: Vec<String>, read: bool, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if self.change_job.busy || self.pending_change.is_some() { return; }
        let request = if ids.len() == 1 {
            scheduled_notifications::mark_request(&ids[0], read)
        } else if read {
            scheduled_notifications::mark_read_request(&ids)
        } else { return; };
        match request {
            Ok(request) => {
                // Freeze only explicit visible/selected IDs. Never mark unseen arrivals.
                self.pending_change = Some(ReadChange { ids, read });
                self.change_job.request("change read state", api, request, runtime, ctx);
            }
            Err(_) => self.change_job.status = "Invalid read-state selection.".into(),
        }
    }
    fn reconcile(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        let Some(change) = &self.pending_change else { return; };
        if self.change_job.busy { return; }
        let ids = change.ids.clone();
        let api = api.clone();
        self.change_job.start("load affected read states", async move {
            // Explicit recovery is read-only. Every affected receipt must be
            // reobserved before another write; an empty unread page is insufficient.
            for id in &ids {
                let request = scheduled_notifications::get_request(id).map_err(ApiError::from)?;
                let value = api.execute(request).await?;
                ScheduledNotification::parse(value, id).map_err(ApiError::from)?;
            }
            Ok(json!({"checked_ids": ids}))
        }, runtime, ctx);
    }
    pub fn poll(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if let Some(message) = self.page_job.poll() {
            self.page = None;
            if let (Ok(value), Some(query)) = (message.result, self.loading_query.take()) {
                match ScheduledNotificationPage::parse(value, &query) {
                    Ok(page) => self.page = Some(page),
                    Err(_) => self.page_job.status = "Invalid update page response; no receipts or count were retained.".into(),
                }
            }
        }
        if let Some(message) = self.detail_job.poll() {
            self.detail = None;
            if self.loading_id == self.selected {
                if let Ok(value) = message.result {
                    match ScheduledNotification::parse(value, &self.selected) {
                        Ok(detail) => self.detail = Some(detail),
                        Err(_) => self.detail_job.status = "Selected update response did not match this receipt.".into(),
                    }
                }
            }
        }
        if let Some(message) = self.output_job.poll() {
            self.output = None;
            if let (Ok(value), Some((id, session, run))) = (message.result, &self.output_target) {
                if id == &self.selected {
                    match ExecutionOutput::parse(value, session, run) {
                        Ok(output) => self.output = Some(output),
                        Err(_) => self.output_job.status = "Invalid or mismatched execution history response.".into(),
                    }
                }
            }
        }
        if let Some(message) = self.change_job.poll() {
            let Some(change) = &self.pending_change else { return; };
            let accepted = match message.result {
                Ok(value) if message.kind == "change read state" => {
                    NotificationAcknowledgements::parse(value, &change.ids, change.read).is_ok()
                }
                Ok(value) if message.kind == "load affected read states" => {
                    value["checked_ids"] == json!(change.ids)
                }
                Err(error) => {
                    let unknown = message.kind == "load affected read states" || matches!(error,
                        ApiError::Transport | ApiError::Response | ApiError::Status(408 | 500..=599));
                    if !unknown { self.pending_change = None; }
                    false
                }
                _ => false,
            };
            if accepted {
                self.pending_change = None;
                self.change_job.outcome_unknown = false;
                self.change_job.status = if message.kind == "change read state" {
                    "Read state saved. Refreshing the current receipts."
                } else { "Affected receipts checked. No change was repeated." }.into();
                if self.active {
                    self.refresh(api, runtime, ctx);
                    self.load_selected(api, runtime, ctx);
                }
            } else if self.pending_change.is_some() {
                self.change_job.outcome_unknown = true;
                self.change_job.status = "Read-state change is unconfirmed. Check affected receipts before another change.".into();
            }
        }
    }
    fn load_output(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        let Some(detail) = &self.detail else { return; };
        let Some(run) = &detail.execution.run_id else { return; };
        if self.output_job.busy { return; }
        let session = detail.execution.session_id.clone();
        self.output = None;
        self.output_target = Some((detail.notification_id.clone(), session.clone(), run.clone()));
        self.output_job.request("load execution output", api,
            ApiRequest::get(&["api", "v1", "assistant", "sessions", &session, "history"]).query("limit", 200),
            runtime, ctx);
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if !self.active {
            self.active = true;
            self.refresh(api, runtime, ctx);
            self.load_selected(api, runtime, ctx);
        }
        ui.label("Completed, failed and cancelled scheduled attempts. Read state is saved to your account across devices; opening an update does not mark it read.");
        ui.weak("In-app execution receipts only. No email, system push or external message is sent. Unresolved launches remain in Scheduled tasks.");
        self.page_job.status_ui(ui);
        self.change_job.status_ui(ui);
        if self.pending_change.is_some() && !self.change_job.busy {
            if ui.button("Check affected read states (read only)").clicked() { self.reconcile(api, runtime, ctx); }
        }
        ui.add_enabled_ui(!self.change_job.busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                let before = self.filter;
                ui.selectable_value(&mut self.filter, NotificationFilter::All, "All");
                ui.selectable_value(&mut self.filter, NotificationFilter::Unread, "Unread");
                if before != self.filter {
                    self.cursors = vec![None];
                    self.refresh(api, runtime, ctx);
                }
                if let Some(page) = &self.page { ui.label(format!("{} unread across your inbox", page.unread_count)); }
                if ui.button("Refresh updates").clicked() {
                    self.refresh(api, runtime, ctx);
                    self.load_selected(api, runtime, ctx);
                }
            });
            let unread_ids = self.page.as_ref().map(|page| page.notifications.iter()
                .filter(|row| row.read_at.is_none()).map(|row| row.notification_id.clone()).collect::<Vec<_>>()).unwrap_or_default();
            if ui.add_enabled(!unread_ids.is_empty() && self.pending_change.is_none(), egui::Button::new("Mark this page read")).clicked() {
                self.change(unread_ids, true, api, runtime, ctx);
            }
            let rows = self.page.as_ref().map(|page| page.notifications.clone()).unwrap_or_default();
            if self.page.is_some() && rows.is_empty() { ui.label("No matching outcomes on this page."); }
            egui::ScrollArea::vertical().id_salt("scheduled_updates").max_height(260.0).show(ui, |ui| {
                for row in rows {
                    ui.group(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            if ui.selectable_label(self.selected == row.notification_id, &row.task.title).clicked() {
                                self.select(row.notification_id.clone(), api, runtime, ctx);
                            }
                            ui.label(row.execution.status.as_str());
                            ui.strong(if row.read_at.is_none() { "Unread" } else { "Read" });
                            if ui.add_enabled(self.pending_change.is_none(), egui::Button::new(if row.read_at.is_none() { "Mark read" } else { "Mark unread" })).clicked() {
                                self.change(vec![row.notification_id.clone()], row.read_at.is_none(), api, runtime, ctx);
                            }
                        });
                        ui.weak(&row.occurred_at);
                        ui.monospace(&row.notification_id);
                        if let Some(error) = &row.execution.error_code { ui.label(error); }
                    });
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(self.cursors.last().is_some_and(Option::is_some), egui::Button::new("Latest")).clicked() {
                    self.cursors = vec![None]; self.refresh(api, runtime, ctx);
                }
                if ui.add_enabled(self.cursors.len() > 1, egui::Button::new("Newer")).clicked() {
                    self.cursors.pop(); self.refresh(api, runtime, ctx);
                }
                let next = self.page.as_ref().and_then(|page| page.next_cursor.clone());
                if ui.add_enabled(next.is_some(), egui::Button::new("Older")).clicked() {
                    if self.cursors.len() == MAX_PAGE_HISTORY { self.cursors.remove(0); }
                    self.cursors.push(next); self.refresh(api, runtime, ctx);
                }
                ui.weak(format!("Up to {PAGE_SIZE} receipts per page"));
            });
            ui.weak("Newer keeps the last 127 page transitions; Latest is always available from an older page.");
            ui.weak("New outcomes appear on Latest. The current page refreshes every 15 seconds while this view is open.");
            ui.horizontal(|ui| {
                ui.label("Notification ID");
                ui.text_edit_singleline(&mut self.open_id);
                if ui.button("Open update").clicked() { self.select(self.open_id.trim().to_owned(), api, runtime, ctx); }
                if !self.selected.is_empty() && ui.button("Close update").clicked() { self.select(String::new(), api, runtime, ctx); }
            });
        });
        self.detail_job.status_ui(ui);
        if let Some(detail) = self.detail.clone() {
            ui.separator();
            ui.heading(&detail.task.title);
            ui.label(format!("{} · {}", detail.execution.status.as_str(), detail.occurred_at));
            ui.label(format!("Schedule {} · {}", detail.task.task_id, detail.task.status));
            ui.label(format!("Execution {}", detail.execution.execution_id));
            if let Some(run) = &detail.execution.run_id { ui.label(format!("Run {run}")); }
            if let Some(error) = &detail.execution.error_code { ui.colored_label(egui::Color32::LIGHT_RED, error); }
            if detail.execution.status.as_str() == "failed" {
                ui.weak(if detail.execution.run_id.is_some() {
                    "This run failed after launch and may have performed actions. Review its output before creating another schedule. Recurring schedules may still launch again."
                } else { "This attempt failed before launch. Check Scheduled tasks for its current status and any bounded pre-launch retry." });
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.change_job.busy && self.pending_change.is_none(), egui::Button::new(if detail.read_at.is_none() { "Mark selected read" } else { "Mark selected unread" })).clicked() {
                    self.change(vec![detail.notification_id.clone()], detail.read_at.is_none(), api, runtime, ctx);
                }
                if ui.button("Copy Web update path").clicked() {
                    ctx.copy_text(format!("/tasks?tab=notifications&notification={}", detail.notification_id));
                }
                if ui.add_enabled(!self.change_job.busy, egui::Button::new("Refresh selected update")).clicked() { self.load_selected(api, runtime, ctx); }
                if ui.add_enabled(detail.execution.run_id.is_some() && !self.output_job.busy, egui::Button::new("Load execution output")).clicked() { self.load_output(api, runtime, ctx); }
            });
            ui.weak("The copied path opens this receipt in your Web console. Gateway and Web origins may differ.");
            egui::CollapsingHeader::new("Receipt details").show(ui, |ui| {
                if let Ok(value) = serde_json::to_value(&detail) { json_view(ui, &value); }
            });
        }
        self.output_job.status_ui(ui);
        if let Some(output) = &self.output {
            ui.weak("Read-only assistant text for the selected execution. Output can be partial; the execution status determines whether it succeeded.");
            if output.revoked { ui.label("Some output is hidden because access to its sources was revoked."); }
            if output.limited { ui.label("Only the latest 200 session history messages were loaded."); }
            if output.text.is_empty() { ui.label("No readable assistant text is available. This does not mean the run succeeded or had no effects."); }
            for content in &output.text { ui.label(content); }
        }
        if !self.page_job.busy && !self.change_job.busy && Instant::now() >= self.next_refresh {
            self.refresh(api, runtime, ctx);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}
