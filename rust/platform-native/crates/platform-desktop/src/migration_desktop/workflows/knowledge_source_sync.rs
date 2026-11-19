//! Session-scoped request recovery; Knowledge remains the execution authority.
use super::super::api::{ApiError, DesktopApi};
use super::common::RemoteJob;
use ai_platform_client::knowledge_sources as contract;
use eframe::egui;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Intent {
    request: Option<String>,
    receipt: Option<Value>,
    uncertain: bool,
    review: bool,
    confirmed: bool,
    recovery_input: String,
}
impl Intent {
    fn can_start(&self) -> bool {
        self.request.is_none()
            || (!self.uncertain && self.receipt.as_ref().is_some_and(contract::terminal))
    }
    fn restore(&mut self, id: &str) -> bool {
        if !contract::request_id_valid(id) || !self.can_start() {
            return false;
        }
        self.request = Some(id.into());
        self.receipt = None;
        self.uncertain = true;
        self.close_review();
        true
    }
    fn close_review(&mut self) {
        self.review = false;
        self.confirmed = false;
    }
    fn submit(&mut self, id: &str) -> bool {
        if !self.review || !self.confirmed || !self.can_start() {
            return false;
        }
        self.restore(id)
    }
    fn receive(&mut self, binding: &str, value: Value) -> Result<(), ApiError> {
        let id = self.request.as_deref().ok_or(ApiError::Response)?;
        match contract::validate_receipt(value, binding, id) {
            Ok(value) => {
                self.receipt = Some(value);
                self.uncertain = false;
                Ok(())
            }
            Err(error) => {
                self.uncertain = true;
                Err(error.into())
            }
        }
    }
}
#[derive(Default)]
struct Entry {
    intent: Intent,
    job: RemoteJob,
    next_read: Option<Instant>,
}
#[derive(Default)]
pub struct SourceSync {
    session: Option<DesktopApi>,
    dataset: String,
    entries: BTreeMap<(String, String), Entry>,
}
impl SourceSync {
    pub fn close_reviews(&mut self) {
        for entry in self.entries.values_mut() {
            entry.intent.close_review();
        }
    }
    /// Missing bindings never hide retained request recovery or grant new writes.
    pub fn visible_bindings(&self, dataset: &str, sources: &Value) -> Vec<Value> {
        let mut bindings = sources["confluence_bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for ((d, binding), entry) in &self.entries {
            if d == dataset
                && entry.intent.request.is_some()
                && !bindings
                    .iter()
                    .any(|v| v["binding_id"].as_str() == Some(binding))
            {
                bindings.push(serde_json::json!({"binding_id":binding,"space_name":"Retained request recovery","manual_sync_available":false}));
            }
        }
        bindings
    }

    /// Run before source polling/rendering so old-session snapshots cannot leak.
    pub fn bind(&mut self, api: &DesktopApi, dataset: &str) -> bool {
        let changed = self
            .session
            .as_ref()
            .is_some_and(|old| !api.same_session(old));
        if changed {
            for entry in self.entries.values_mut() {
                entry.job.cancel_observation();
            }
            *self = Self::default();
        }
        self.session = Some(api.clone());
        if self.dataset != dataset {
            for entry in self.entries.values_mut() {
                entry.intent.close_review();
            }
            self.dataset = dataset.into();
        }
        changed
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        dataset: &str,
        binding: &Value,
        enabled: bool,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) -> bool {
        let Some(binding_id) = binding["binding_id"].as_str().filter(|id| !id.is_empty()) else {
            return false;
        };
        let key = (dataset.to_owned(), binding_id.to_owned());
        let entry = self.entries.entry(key).or_default();
        let current = binding.get("current_sync_task").filter(|v| !v.is_null());
        let invalid_current = current.is_some_and(|v| {
            v["task_id"]
                .as_str()
                .is_none_or(|id| !contract::request_id_valid(id))
        });
        if entry.intent.request.is_none() {
            if let Some(id) = current.and_then(|v| v["task_id"].as_str()) {
                entry.intent.restore(id);
            }
        }
        if let Some(message) = entry.job.poll() {
            match message.result {
                Ok(value) => {
                    if let Err(error) = entry.intent.receive(binding_id, value) {
                        entry.job.status = error.to_string();
                    }
                }
                Err(_) => {
                    entry.intent.uncertain = true;
                }
            }
            entry.next_read = Some(Instant::now() + Duration::from_secs(2));
        }
        let different_current = current
            .and_then(|v| v["task_id"].as_str())
            .is_some_and(|id| entry.intent.request.as_deref() != Some(id));
        let available = enabled
            && binding["manual_sync_available"] == true
            && !invalid_current
            && !different_current;
        if !available {
            entry.intent.close_review();
        }
        ui.push_id((dataset, binding_id, "manual_sync"), |ui| {
            ui.strong(format!("{} · binding {binding_id}", binding["space_name"].as_str().unwrap_or("Saved source")));
            ui.label("Manual full reconciliation uses this saved binding. Source removals may remove documents; documents still referenced by another binding are retained.");
            if invalid_current { ui.colored_label(egui::Color32::YELLOW, "Invalid current task identity. Refresh source state; no new sync can be submitted."); }
            if different_current { ui.label("The source reports a different task. Finish checking this exact request, then recover the current task ID before starting another sync."); }
            entry.job.status_ui(ui);
            if let Some(id) = entry.intent.request.clone() {
                ui.label(format!("Exact request ID: {id}"));
                if ui.button("Copy recovery request ID").clicked() { ctx.copy_text(id.clone()); }
                ui.weak("Keep this ID to recover after closing the app. Local retention lasts only for this signed-in session; source state may provide the last server task.");
                if entry.intent.uncertain { ui.colored_label(egui::Color32::YELLOW, "Submission or status unconfirmed. Recheck this exact ID. Missing/error responses do not prove rejection; do not submit again."); }
                if !entry.intent.uncertain {
                    if let Some(task) = &entry.intent.receipt {
                        ui.label(format!("Server status: {}", task["status"].as_str().unwrap_or("unknown")));
                        if contract::active(task) {
                            match (task["processed_items"].as_u64(), task["total_items"].as_u64()) {
                                (Some(done), Some(total)) => { ui.label(format!("Attempted: {done} / {total} (not successful updates)")); }
                                _ => { ui.label("Progress total unknown"); }
                            }
                        }
                        for (field, label) in [("synced_items", "Synced items"), ("failed_items", "Failed items"), ("error_count", "Errors")] {
                            if let Some(count) = task[field].as_u64() { ui.label(format!("{label}: {count}")); }
                        }
                        if task["status"] == "failed" {
                            ui.label(if task["synced_items"].as_u64().is_some_and(|n| n > 0) { "Sync failed after partial updates. Review the source before a new full reconciliation." } else { "Sync failed. This is not a successful zero-update result." });
                        }
                    }
                }
                let auto = !entry.intent.uncertain && entry.intent.receipt.as_ref().is_some_and(contract::active)
                    && entry.next_read.is_some_and(|at| Instant::now() >= at);
                let recheck = ui.add_enabled(!entry.job.busy, egui::Button::new("Recheck exact sync request")).clicked();
                if !entry.job.busy && (recheck || auto) {
                    if let Ok(request) = contract::read_request(dataset, binding_id, &id) {
                        entry.job.request("source_sync_read", api, request, runtime, ctx);
                    }
                }
                if !entry.intent.uncertain && entry.intent.receipt.as_ref().is_some_and(contract::active) { ctx.request_repaint_after(Duration::from_secs(2)); }
            }
            ui.horizontal(|ui| {
                ui.label("Recover request ID");
                ui.add_enabled(entry.intent.can_start() && !entry.job.busy, egui::TextEdit::singleline(&mut entry.intent.recovery_input));
                if ui.add_enabled(entry.intent.can_start() && !entry.job.busy && contract::request_id_valid(entry.intent.recovery_input.trim()), egui::Button::new("Use exact ID (read only)")).clicked() {
                    let id = entry.intent.recovery_input.trim().to_owned(); entry.intent.restore(&id);
                }
            });
            if let Some(id) = current.and_then(|v| v["task_id"].as_str()) {
                if different_current && ui.add_enabled(entry.intent.can_start() && !entry.job.busy, egui::Button::new("Recover source's current task (read only)")).clicked() { entry.intent.restore(id); }
            }
            if ui.add_enabled(available && !entry.job.busy && entry.intent.can_start(), egui::Button::new("Review full source reconciliation…")).clicked() { entry.intent.review = true; entry.intent.confirmed = false; }
            if entry.intent.review {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.strong("Review one full reconciliation");
                    ui.label(format!("Dataset: {dataset}\nBinding: {binding_id}"));
                    ui.checkbox(&mut entry.intent.confirmed, "I reviewed this saved binding and the removal/partial-failure impact");
                    if ui.add_enabled(available && !entry.job.busy && entry.intent.confirmed && entry.intent.can_start(), egui::Button::new("Submit full reconciliation once")).clicked() {
                        let id = uuid::Uuid::new_v4().to_string();
                        if let Ok(request) = contract::start_request(dataset, binding_id, &id) {
                            // Retain the original identity BEFORE dispatch, never automatically POST again.
                            if entry.intent.submit(&id) { entry.job.request("source_sync_submit", api, request, runtime, ctx); }
                        }
                    }
                    if ui.button("Cancel reconciliation review").clicked() { entry.intent.close_review(); }
                });
            }
        });
        entry.job.expired
    }
}

#[cfg(test)]
mod source_sync_tests {
    use super::*;
    use serde_json::json;
    const ID: &str = "7d73497c-7f68-4c13-a5d4-4681c3f7aaae";
    fn receipt(status: &str) -> Value {
        json!({"task_id":ID,"binding_id":"b","status":status})
    }
    #[test]
    fn one_review_one_post_unknown_cannot_replay() {
        let mut i = Intent::default();
        assert!(!i.submit(ID));
        i.review = true;
        assert!(!i.submit(ID));
        i.confirmed = true;
        assert!(i.submit(ID));
        assert!(!i.submit(ID));
        assert!(!i.can_start());
        assert!(i.uncertain);
        assert_eq!(i.request.as_deref(), Some(ID));
    }
    #[test]
    fn restored_terminal_requires_exact_read() {
        let mut i = Intent::default();
        assert!(i.restore(ID));
        assert!(!i.can_start());
        i.receive("b", receipt("completed")).unwrap();
        assert!(i.can_start());
    }
    #[test]
    fn mismatched_status_and_failed_reads_cannot_release_identity() {
        let mut i = Intent::default();
        i.restore(ID);
        assert!(i.receive("other", receipt("completed")).is_err());
        assert!(!i.can_start());
        assert!(i.receive("b", receipt("succeeded")).is_err());
        assert_eq!(i.request.as_deref(), Some(ID));
        i.receive("b", receipt("processing")).unwrap();
        assert!(!i.can_start());
        i.uncertain = true;
        assert!(!i.can_start());
        i.receive("b", receipt("failed")).unwrap();
        assert!(i.can_start());
    }
    #[test]
    fn dataset_navigation_keeps_request_but_cancels_review() {
        let api = DesktopApi::new("https://example.test", "synthetic".into()).unwrap();
        let mut s = SourceSync::default();
        s.bind(&api, "d");
        let mut e = Entry::default();
        e.intent.restore(ID);
        e.intent.review = true;
        e.intent.confirmed = true;
        s.entries.insert(("d".into(), "b".into()), e);
        s.bind(&api, "other");
        let i = &s.entries[&("d".into(), "b".into())].intent;
        assert_eq!(i.request.as_deref(), Some(ID));
        assert!(!i.review);
        assert!(!i.confirmed);
    }
    #[test]
    fn missing_binding_still_exposes_read_only_recovery() {
        let mut s = SourceSync::default();
        let mut entry = Entry::default();
        entry.intent.restore(ID);
        s.entries.insert(("d".into(), "b".into()), entry);
        let visible = s.visible_bindings("d", &Value::Null);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["binding_id"], "b");
        assert_eq!(visible[0]["manual_sync_available"], false);
        assert!(s.visible_bindings("other", &Value::Null).is_empty());
        let listed = serde_json::json!({"confluence_bindings":[{"binding_id":"b"}]});
        assert_eq!(s.visible_bindings("d", &listed).len(), 1);
    }
    #[test]
    fn account_and_origin_changes_clear_private_state() {
        let a = DesktopApi::new("https://example.test", "synthetic-a".into()).unwrap();
        for b in [
            DesktopApi::new("https://example.test", "synthetic-b".into()).unwrap(),
            DesktopApi::new("https://other.test", "synthetic-a".into()).unwrap(),
        ] {
            let mut s = SourceSync::default();
            s.bind(&a, "d");
            s.entries.insert(("d".into(), "b".into()), Entry::default());
            assert!(s.bind(&b, "d"));
            assert!(s.entries.is_empty());
        }
    }
}
