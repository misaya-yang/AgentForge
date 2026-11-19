//! Task operations use the shared client contract and never resubmit uncertain mutations.
use super::{super::api::DesktopApi, common::*};
use ai_platform_client::management::{
    TaskQuery, TaskStatus, cancel_task_request, task_request, task_result_request,
};
use eframe::egui;
use serde_json::Value;
use std::time::{Duration, Instant};

pub struct TaskExplorer {
    pub list_job: RemoteJob,
    pub job: RemoteJob,
    action: ReviewedAction,
    filter: String,
    limit: u16,
    offset: u64,
    rows: Vec<Value>,
    selected: String,
    detail: Value,
    result: Value,
    watching: bool,
    next_poll: Instant,
}
impl Default for TaskExplorer {
    fn default() -> Self {
        Self {
            list_job: RemoteJob::default(),
            job: RemoteJob::default(),
            action: ReviewedAction::default(),
            filter: String::new(),
            limit: 50,
            offset: 0,
            rows: Vec::new(),
            selected: String::new(),
            detail: Value::Null,
            result: Value::Null,
            watching: false,
            next_poll: Instant::now(),
        }
    }
}
impl TaskExplorer {
    pub fn refresh(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let status = if self.filter.is_empty() {
            None
        } else {
            TaskStatus::parse(&self.filter).ok()
        };
        if let Ok(request) = (TaskQuery {
            status,
            limit: self.limit,
            offset: self.offset,
        })
        .request()
        {
            self.list_job
                .request("load tasks", api, request, runtime, ctx);
        }
    }
    fn load_selected(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.job
            .request("load task", api, task_request(&self.selected), runtime, ctx);
        self.next_poll = Instant::now() + Duration::from_secs(2);
    }
    fn select(
        &mut self,
        id: String,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if id == self.selected {
            return;
        }
        self.job.cancel_observation();
        self.action.clear();
        self.selected = id;
        self.detail = Value::Null;
        self.result = Value::Null;
        self.watching = false;
        if !self.selected.is_empty() {
            self.load_selected(api, runtime, ctx);
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.list_job.poll() {
            if let Ok(value) = message.result {
                match value.as_array() {
                    Some(rows) => self.rows = rows.clone(),
                    None => self.list_job.status = "Invalid task list response".into(),
                }
            }
        }
        if let Some(message) = self.job.poll() {
            match message.result {
                Ok(value) if message.kind == "load task" => {
                    if value["task_id"].as_str() != Some(self.selected.as_str()) {
                        self.job.status = "Invalid task response".into();
                        self.watching = false;
                    } else {
                        self.watching &= TaskStatus::parse(value["status"].as_str().unwrap_or(""))
                            .is_ok_and(|s| !s.terminal());
                        self.detail = value;
                    }
                }
                Ok(value) if message.kind == "load result" => self.result = value,
                Ok(_) if message.kind == "cancel task" => {
                    self.watching = true;
                    self.next_poll = Instant::now();
                }
                Err(_) => self.watching = false,
                _ => {}
            }
        }
        self.list_job.status_ui(ui);
        ui.add_enabled_ui(!self.list_job.busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("Status");
                let before = self.filter.clone();
                select_text(
                    ui,
                    "task_status_filter",
                    &mut self.filter,
                    &[
                        "",
                        "pending",
                        "processing",
                        "completed",
                        "failed",
                        "cancelled",
                    ],
                );
                if before != self.filter {
                    self.offset = 0;
                }
                ui.label("Page size");
                ui.add(egui::DragValue::new(&mut self.limit).range(1..=100));
                if ui.button("Load tasks").clicked() {
                    self.refresh(api, runtime, ctx);
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.offset > 0, egui::Button::new("Previous page"))
                    .clicked()
                {
                    self.offset = self.offset.saturating_sub(self.limit as u64);
                    self.refresh(api, runtime, ctx);
                }
                if ui
                    .add_enabled(
                        self.rows.len() >= self.limit as usize,
                        egui::Button::new("Next page"),
                    )
                    .clicked()
                {
                    self.offset = self.offset.saturating_add(self.limit as u64);
                    self.refresh(api, runtime, ctx);
                }
                ui.label(format!(
                    "Offset {} · {} tasks",
                    self.offset,
                    self.rows.len()
                ));
            });
        });
        let mut selected = self.selected.clone();
        // Keep the reviewed target stable until an in-flight mutation is observed.
        ui.add_enabled_ui(!self.job.busy, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("task_list")
                .max_height(240.0)
                .show(ui, |ui| {
                    for row in &self.rows {
                        let Some(id) = row["task_id"].as_str() else {
                            continue;
                        };
                        ui.horizontal(|ui| {
                            if ui.selectable_label(id == selected, id).clicked() {
                                selected = id.into();
                            }
                            ui.label(row["status"].as_str().unwrap_or("unknown"));
                            ui.weak(row["service_id"].as_str().unwrap_or(""));
                            if let Some(progress) = row["progress"].as_f64() {
                                ui.label(format!("Progress: {progress}"));
                            }
                        });
                    }
                });
            text_field(ui, "Task ID", &mut selected);
        });
        self.select(selected, api, runtime, ctx);
        self.job.status_ui(ui);
        let status = TaskStatus::parse(self.detail["status"].as_str().unwrap_or("")).ok();
        ui.add_enabled_ui(!self.selected.is_empty() && !self.job.busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Refresh selected").clicked() {
                    self.load_selected(api, runtime, ctx);
                }
                if ui
                    .add_enabled(
                        status == Some(TaskStatus::Completed),
                        egui::Button::new("Load result"),
                    )
                    .clicked()
                {
                    self.watching = false;
                    self.job.request(
                        "load result",
                        api,
                        task_result_request(&self.selected),
                        runtime,
                        ctx,
                    );
                }
                if ui
                    .add_enabled(
                        status.is_some_and(|s| !s.terminal()) && !self.job.outcome_unknown,
                        egui::Button::new("Cancel task…"),
                    )
                    .clicked()
                {
                    self.watching = false;
                    self.action.prepare(
                        "cancel task",
                        "Cancel this task",
                        cancel_task_request(&self.selected),
                    );
                }
                if ui
                    .checkbox(&mut self.watching, "Watch selected task")
                    .changed()
                {
                    self.next_poll = Instant::now();
                }
            });
        });
        if self.watching && !self.selected.is_empty() {
            if !self.job.busy && Instant::now() >= self.next_poll {
                self.load_selected(api, runtime, ctx);
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        self.action.ui(ui, &mut self.job, api, runtime, ctx);
        if !self.detail.is_null() {
            egui::CollapsingHeader::new("Task details")
                .default_open(true)
                .show(ui, |ui| json_view(ui, &self.detail));
        }
        if !self.result.is_null() {
            egui::CollapsingHeader::new("Task result")
                .default_open(true)
                .show(ui, |ui| json_view(ui, &self.result));
        }
    }
}
