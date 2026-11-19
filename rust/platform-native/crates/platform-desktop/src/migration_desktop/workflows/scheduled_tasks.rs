//! Schedule management projects Gateway-owned state and never dispatches work locally.
use super::{super::api::DesktopApi, common::*};
use ai_platform_client::scheduled_tasks::{self, CreateScheduledTask, ScheduleAction, ScheduleCursor, SchedulePage, ScheduleQuery};
use eframe::egui;
use serde_json::Value;

pub struct ScheduledTaskEditor {
    pub job: RemoteJob,
    action: ReviewedAction,
    rows: Vec<Value>,
    selected: String,
    selected_row: Value,
    tasks_cursor: Option<ScheduleCursor>,
    executions_cursor: Option<ScheduleCursor>,
    pending_read: Option<HistoryRead>,
    exact_task_id: String,
    exact_execution_id: String,
    exact_execution: Value,
    recovery_inspected: bool,
    executions: Value,
    mutation: Value,
    title: String,
    message: String,
    model_id: String,
    run_at: String,
    recurring: bool,
    interval: u32,
    retries: u8,
}
#[derive(Clone)]
enum HistoryRead {
    Tasks(ScheduleQuery),
    Executions(String, ScheduleQuery),
    Task(String),
    Execution(String, String),
}
impl HistoryRead {
    fn request(&self) -> Result<ai_platform_client::ApiRequest, ai_platform_client::Error> {
        match self {
            Self::Tasks(query) => query.tasks_request(),
            Self::Executions(id, query) => query.executions_request(id),
            Self::Task(id) => scheduled_tasks::get_request(id),
            Self::Execution(id, execution) => scheduled_tasks::execution_request(id, execution),
        }
    }
}
impl Default for ScheduledTaskEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            action: ReviewedAction::default(),
            rows: Vec::new(),
            selected: String::new(),
            selected_row: Value::Null,
            tasks_cursor: None,
            executions_cursor: None,
            pending_read: None,
            exact_task_id: String::new(),
            exact_execution_id: String::new(),
            exact_execution: Value::Null,
            recovery_inspected: false,
            executions: Value::Null,
            mutation: Value::Null,
            title: String::new(),
            message: String::new(),
            model_id: String::new(),
            run_at: String::new(),
            recurring: false,
            interval: 3600,
            retries: 0,
        }
    }
}
impl ScheduledTaskEditor {
    pub fn refresh(
        &mut self,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.read(HistoryRead::Tasks(ScheduleQuery::default()), api, runtime, ctx);
    }
    fn read(&mut self, read: HistoryRead, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        if self.job.busy { return; }
        match read.request() {
            Ok(request) => {
                self.pending_read = Some(read);
                self.job.request("load schedule history", api, request, runtime, ctx);
            }
            Err(_) => self.job.status = "Use valid UUIDs and a complete server cursor.".into(),
        }
    }
    fn select(&mut self, row: Value) {
        let id = row["task_id"].as_str().unwrap_or_default().to_owned();
        if self.selected != id {
            self.selected = id;
            self.executions = Value::Null;
            self.executions_cursor = None;
            self.mutation = Value::Null;
            self.action.clear();
        }
        self.selected_row = row;
    }
    fn accept_read(&mut self, read: HistoryRead, value: Value) -> Result<(), ai_platform_client::Error> {
        match read {
            HistoryRead::Tasks(query) => {
                let page = SchedulePage::tasks(&value, &query)?;
                if let Some(row) = page.rows.iter().find(|row| row["task_id"].as_str() == Some(self.selected.as_str())) {
                    self.selected_row = row.clone();
                    self.action.clear();
                }
                self.rows = page.rows;
                self.tasks_cursor = page.next_cursor;
            }
            HistoryRead::Executions(id, query) => {
                if self.selected != id { return Err(ai_platform_client::Error::Response); }
                let page = SchedulePage::executions(&value, &id, &query)?;
                self.executions_cursor = page.next_cursor;
                self.executions = value;
            }
            HistoryRead::Task(id) => {
                let row = scheduled_tasks::parse_task(&value, &id)?;
                self.select(row);
                self.action.clear();
            }
            HistoryRead::Execution(id, execution) => {
                scheduled_tasks::parse_execution(&value, &id, &execution)?;
                self.exact_execution = value;
            }
        }
        Ok(())
    }
    /// Abandon only reads/reviews on navigation; submitted writes keep their channel.
    pub fn close_observation(&mut self) {
        self.action.clear();
        self.recovery_inspected = false;
        if self.pending_read.take().is_some() {
            let unknown = self.job.outcome_unknown;
            self.job.cancel_observation();
            self.job.outcome_unknown = unknown;
        }
        self.rows.clear();
        self.tasks_cursor = None;
        self.selected.clear();
        self.selected_row = Value::Null;
        self.executions = Value::Null;
        self.executions_cursor = None;
        self.exact_execution = Value::Null;
        self.exact_task_id.clear();
        self.exact_execution_id.clear();
    }
    fn poll(&mut self) {
        // Read success/failure must not clear or invent uncertainty about a write.
        let write_unknown = self.job.outcome_unknown;
        if let Some(message) = self.job.poll() {
            if let Some(read) = self.pending_read.take() {
                self.job.outcome_unknown = write_unknown;
                if let Ok(value) = message.result {
                    if self.accept_read(read, value).is_err() {
                        self.job.status = "Invalid or mismatched schedule history response; previous view retained.".into();
                    } else {
                        self.recovery_inspected = true;
                    }
                } else if !self.job.expired {
                    self.job.status = "Could not load schedule history. Previous view retained; this read made no change.".into();
                }
            } else if let Ok(value) = message.result {
                self.mutation = value;
                self.recovery_inspected = false;
                self.job.status = "Change accepted. Refresh schedules to inspect current state.".into();
            } else {
                self.recovery_inspected = false;
            }
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll();
        self.job.status_ui(ui);
        ui.label("Schedules run on the Gateway under your current permissions. This screen does not start a local scheduler.");
        if self.job.outcome_unknown {
            ui.colored_label(egui::Color32::YELLOW, "Outcome unknown. Reload schedules and executions before repeating a change; a previous submission may already exist.");
        }
        ui.add_enabled_ui(!self.job.busy, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Latest schedules").clicked() { self.refresh(api, runtime, ctx); }
                if ui.add_enabled(self.tasks_cursor.is_some(), egui::Button::new("Older schedules")).clicked() {
                    self.read(HistoryRead::Tasks(ScheduleQuery { cursor: self.tasks_cursor.clone(), ..Default::default() }), api, runtime, ctx);
                }
            });
            ui.weak("20 records per page. Opening older pages preserves the selected schedule and receipt. Reads never dispatch work.");
            ui.collapsing("Recover by exact ID", |ui| {
                text_field(ui, "Schedule task ID", &mut self.exact_task_id);
                text_field(ui, "Execution ID (optional)", &mut self.exact_execution_id);
                ui.horizontal(|ui| {
                    if ui.button("Open exact schedule").clicked() {
                        self.read(HistoryRead::Task(self.exact_task_id.trim().into()), api, runtime, ctx);
                    }
                    if ui.button("Open exact execution").clicked() {
                        self.read(HistoryRead::Execution(self.exact_task_id.trim().into(), self.exact_execution_id.trim().into()), api, runtime, ctx);
                    }
                });
                ui.weak("Copy both IDs from the receipt. Access is checked by the Gateway for the current owner and tenant.");
            });
            if self.job.outcome_unknown && self.recovery_inspected && ui.button("I reviewed the prior submission outcome").clicked() {
                self.job.outcome_unknown = false;
                self.recovery_inspected = false;
            }
            let mut chosen = None;
            egui::ScrollArea::vertical().id_salt("schedule_list").max_height(240.0).show(ui, |ui| {
                for row in &self.rows {
                    let Some(id) = row["task_id"].as_str() else { continue; };
                    ui.horizontal_wrapped(|ui| {
                        if ui.selectable_label(self.selected == id, row["title"].as_str().unwrap_or(id)).clicked() { chosen = Some(row.clone()); }
                        ui.label(row["status"].as_str().unwrap_or("unknown"));
                        ui.weak(row["next_run_at"].as_str().unwrap_or(""));
                    });
                }
            });
            if let Some(row) = chosen { self.select(row); }
            if !self.selected_row.is_null() {
                let row = self.selected_row.clone();
                ui.label(format!("Selected schedule: {}", self.selected));
                let status = row["status"].as_str().unwrap_or("");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Latest executions").clicked() {
                        self.read(HistoryRead::Executions(self.selected.clone(), ScheduleQuery::default()), api, runtime, ctx);
                    }
                    if ui.add_enabled(self.executions_cursor.is_some(), egui::Button::new("Older executions")).clicked() {
                        self.read(HistoryRead::Executions(self.selected.clone(), ScheduleQuery { cursor: self.executions_cursor.clone(), ..Default::default() }), api, runtime, ctx);
                    }
                    if ui.button("Refresh selected schedule").clicked() {
                        self.read(HistoryRead::Task(self.selected.clone()), api, runtime, ctx);
                    }
                    if ui.add_enabled(status == "active" && !self.job.outcome_unknown, egui::Button::new("Pause…")).clicked() {
                        self.action.prepare("pause schedule", "Pause future dispatch; an active execution may continue", scheduled_tasks::action_request(&self.selected, ScheduleAction::Pause));
                    }
                    if ui.add_enabled(status == "paused" && !self.job.outcome_unknown, egui::Button::new("Resume…")).clicked() {
                        self.action.prepare("resume schedule", "Resume this schedule and reset its pre-dispatch retry count", scheduled_tasks::action_request(&self.selected, ScheduleAction::Resume));
                    }
                    if ui.add_enabled(matches!(status, "active" | "paused") && !self.job.outcome_unknown, egui::Button::new("Cancel schedule…")).clicked() {
                        self.action.prepare("cancel schedule", "Cancel future runs and request cancellation of active execution", scheduled_tasks::action_request(&self.selected, ScheduleAction::Cancel));
                    }
                });
                egui::CollapsingHeader::new("Selected schedule").show(ui, |ui| json_view(ui, &row));
            }
            egui::CollapsingHeader::new("Create schedule").show(ui, |ui| {
                text_field(ui, "Title", &mut self.title);
                text_field(ui, "Model ID", &mut self.model_id);
                text_field(ui, "Run at (RFC3339 with offset)", &mut self.run_at);
                ui.weak("Use an explicit future timestamp, such as 2030-01-15T09:00:00+08:00. The server validates the time.");
                ui.label("Message");
                ui.add(egui::TextEdit::multiline(&mut self.message).desired_rows(5));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.recurring, "Repeat at a fixed interval");
                    if self.recurring { ui.label("Seconds"); ui.add(egui::DragValue::new(&mut self.interval).range(60..=31_536_000)); }
                });
                ui.horizontal(|ui| {
                    ui.label("Pre-dispatch retries"); ui.add(egui::DragValue::new(&mut self.retries).range(0..=3));
                });
                ui.weak("Only pre-dispatch failures are eligible for bounded retries. Uncertain dispatch is never automatically replayed. Recurring runs may consume model quota until paused or cancelled.");
                if ui.add_enabled(!self.job.outcome_unknown, egui::Button::new("Review schedule…")).clicked() {
                    let input = CreateScheduledTask { title: self.title.clone(), message: self.message.clone(), model_id: self.model_id.clone(), run_at: self.run_at.clone(), interval_seconds: self.recurring.then_some(self.interval), max_retries: self.retries };
                    match input.request() {
                        Ok(request) => self.action.prepare("create schedule", "Create this schedule and authorize its future Gateway executions", request),
                        Err(_) => self.job.status = "Complete all fields within the title/message/model and interval limits.".into(),
                    }
                }
            });
        });
        self.action.ui(ui, &mut self.job, api, runtime, ctx);
        if !self.executions.is_null() {
            ui.label("Execution status 'uncertain' means dispatch outcome is unknown. Inspect the execution/session instead of resubmitting it.");
            egui::CollapsingHeader::new("Executions")
                .default_open(true)
                .show(ui, |ui| json_view(ui, &self.executions));
        }
        if !self.exact_execution.is_null() {
            egui::CollapsingHeader::new("Exact execution receipt (IDs shown below)")
                .default_open(true)
                .show(ui, |ui| json_view(ui, &self.exact_execution));
        }
        if !self.mutation.is_null() {
            egui::CollapsingHeader::new("Submitted change result")
                .default_open(true)
                .show(ui, |ui| json_view(ui, &self.mutation));
        }
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use serde_json::json;
    const TASK: &str="00000000-0000-0000-0000-000000000003";
    const OTHER: &str="00000000-0000-0000-0000-000000000002";
    const EXEC: &str="00000000-0000-0000-0000-000000000001";
    const AT: &str="2026-10-13T00:00:00Z";
    fn task(id: &str)->Value {json!({"task_id":id,"created_at":AT,"status":"active"})}
    fn execution(id: &str)->Value {json!({"task_id":id,"execution_id":EXEC,"created_at":AT,"status":"uncertain"})}
    #[test]
    fn workspace_navigation_and_disconnect_clear_recovery_state() {
        use super::super::super::{state::DesktopWorkspace,catalog::Section};
        let mut workspace=DesktopWorkspace::default();
        workspace.select(Section::ScheduledTasks);
        workspace.workflows.scheduled_tasks.select(task(TASK));
        workspace.select(Section::Tasks);
        assert!(workspace.workflows.scheduled_tasks.selected.is_empty());
        workspace.select(Section::ScheduledTasks);
        workspace.workflows.scheduled_tasks.select(task(TASK));
        workspace.disconnect();
        assert!(workspace.workflows.scheduled_tasks.selected.is_empty());
    }
    #[test]
    fn fresh_matching_page_updates_selected_status() {
        let mut editor=ScheduledTaskEditor::default(); editor.select(task(TASK));
        let mut fresh=task(TASK);fresh["status"]=json!("paused");
        editor.accept_read(HistoryRead::Tasks(ScheduleQuery::default()),json!({"tasks":[fresh],"next_cursor":null})).unwrap();
        assert_eq!(editor.selected_row["status"],"paused");
    }
    #[test]
    fn closing_drops_reads_and_receipts_but_retains_submitted_write() {
        let mut editor=ScheduledTaskEditor::default();editor.select(task(TASK));
        editor.pending_read=Some(HistoryRead::Task(TASK.into()));editor.job.busy=true;editor.job.outcome_unknown=true;
        editor.close_observation();
        assert!(!editor.job.busy);assert!(editor.job.outcome_unknown);assert!(editor.pending_read.is_none());
        assert!(editor.selected_row.is_null());assert!(editor.rows.is_empty());
        editor.job.busy=true;editor.job.current_kind="pause schedule".into();
        editor.close_observation();assert!(editor.job.busy);assert_eq!(editor.job.current_kind,"pause schedule");
    }
    #[test]
    fn read_completion_never_changes_write_uncertainty() {
        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ctx=egui::Context::default();
        for unknown in [false,true] {
            for success in [false,true] {
                let mut editor=ScheduledTaskEditor::default();editor.job.outcome_unknown=unknown;
                editor.pending_read=Some(HistoryRead::Task(TASK.into()));
                editor.job.start("load schedule history",async move {
                    if success { Ok(json!({"task":task(TASK)})) } else { Err(super::super::super::api::ApiError::Transport) }
                },&runtime,&ctx);
                runtime.block_on(async {tokio::task::yield_now().await;});editor.poll();
                assert!(!editor.job.busy);assert_eq!(editor.job.outcome_unknown,unknown);
                if !success { assert!(!editor.job.status.contains("Outcome unknown")); }
            }
        }
    }
    #[test]
    fn closed_read_completion_cannot_repopulate_view() {
        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ctx=egui::Context::default();let mut editor=ScheduledTaskEditor::default();
        editor.pending_read=Some(HistoryRead::Task(TASK.into()));
        editor.job.start("load schedule history",async { Ok(json!({"task":task(TASK)})) },&runtime,&ctx);
        runtime.block_on(async {tokio::task::yield_now().await;});
        editor.close_observation();editor.poll();assert!(editor.selected_row.is_null());assert!(editor.selected.is_empty());
    }
    #[test]
    fn older_pages_preserve_off_page_selection_and_open_receipt() {
        let mut editor=ScheduledTaskEditor::default();
        editor.select(task(TASK));
        editor.accept_read(HistoryRead::Executions(TASK.into(),ScheduleQuery::default()),json!({"executions":[execution(TASK)],"next_cursor":null})).unwrap();
        editor.accept_read(HistoryRead::Execution(TASK.into(),EXEC.into()),json!({"execution":execution(TASK)})).unwrap();
        let before=editor.executions.clone();
        editor.accept_read(HistoryRead::Tasks(ScheduleQuery::default()),json!({"tasks":[task(OTHER)],"next_cursor":null})).unwrap();
        assert_eq!(editor.selected,TASK);assert_eq!(editor.selected_row,task(TASK));
        assert_eq!(editor.executions,before);assert!(!editor.exact_execution.is_null());
        editor.select(task(OTHER));
        assert!(editor.executions.is_null());assert!(editor.executions_cursor.is_none());
    }
    #[test]
    fn exact_recovery_works_without_any_list_rows() {
        let mut editor=ScheduledTaskEditor::default();
        editor.accept_read(HistoryRead::Task(TASK.into()),json!({"task":task(TASK)})).unwrap();
        assert!(editor.rows.is_empty());assert_eq!(editor.selected,TASK);
        editor.accept_read(HistoryRead::Execution(TASK.into(),EXEC.into()),json!({"execution":execution(TASK)})).unwrap();
        assert_eq!(editor.exact_execution["execution"]["status"],"uncertain");
    }
    #[test]
    fn malformed_or_mismatched_read_retains_previous_view() {
        let mut editor=ScheduledTaskEditor::default();editor.select(task(TASK));
        assert!(editor.accept_read(HistoryRead::Task(TASK.into()),json!({"task":task(OTHER)})).is_err());
        assert_eq!(editor.selected,TASK);
        assert!(editor.accept_read(HistoryRead::Execution(TASK.into(),EXEC.into()),json!({"execution":execution(OTHER)})).is_err());
        assert!(editor.exact_execution.is_null());
        assert!(editor.accept_read(HistoryRead::Executions(OTHER.into(),ScheduleQuery::default()),json!({"executions":[],"next_cursor":null})).is_err());
        assert!(editor.accept_read(HistoryRead::Tasks(ScheduleQuery::default()),json!({"tasks":[]})).is_err());
    }
    #[test]
    fn page_cursor_changes_only_after_valid_response() {
        let mut editor=ScheduledTaskEditor::default();
        let query=ScheduleQuery{limit:1,cursor:None};
        editor.accept_read(HistoryRead::Tasks(query.clone()),json!({"tasks":[task(TASK)],"next_cursor":{"before_at":AT,"before_id":TASK}})).unwrap();
        let cursor=editor.tasks_cursor.clone();
        assert!(editor.accept_read(HistoryRead::Tasks(query),json!({"tasks":[]})).is_err());
        assert_eq!(editor.tasks_cursor,cursor);
        editor.accept_read(HistoryRead::Tasks(ScheduleQuery{limit:1,cursor}),json!({"tasks":[task(OTHER)],"next_cursor":null})).unwrap();
        assert!(editor.tasks_cursor.is_none());assert_eq!(editor.rows[0]["task_id"],OTHER);
        editor.accept_read(HistoryRead::Tasks(ScheduleQuery::default()),json!({"tasks":[task(TASK)],"next_cursor":null})).unwrap();
        assert_eq!(editor.rows[0]["task_id"],TASK);
    }
}
