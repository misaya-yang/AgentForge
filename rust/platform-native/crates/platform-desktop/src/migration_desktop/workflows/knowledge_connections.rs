//! Reviewed saved-connection maintenance. Knowledge owns authorization and execution.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::RemoteJob;
use ai_platform_client::knowledge_connections as contract;
use eframe::egui;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
struct State {
    connections: Option<contract::Managed>,
    impact: Option<contract::Impact>,
    confirmed: bool,
    pending: Option<contract::Change>,
    acknowledged: bool,
    recovery: String,
    message: String,
    refresh_sources: bool,
}
impl State {
    fn close_review(&mut self) {
        self.connections = None;
        self.impact = None;
        self.confirmed = false;
    }
    fn submit(&mut self, dataset: &str, enabled: bool) -> Result<ApiRequest, ApiError> {
        if !enabled || !self.confirmed || self.pending.is_some() {
            return Err(ApiError::Request);
        }
        let change = self
            .impact
            .as_ref()
            .ok_or(ApiError::Request)?
            .connection
            .change()?;
        let request = contract::change_request(dataset, &change)?;
        // Record exact CAS identity before dispatch. Even a rejection requires readback.
        self.pending = Some(change);
        self.acknowledged = false;
        self.close_review();
        Ok(request)
    }
    fn receive_write(&mut self, result: Result<Value, ApiError>) {
        self.acknowledged = self.pending.as_ref().is_some_and(|p| {
            result
                .ok()
                .and_then(|v| contract::parse_connection(v, &p.connection_id).ok())
                .is_some_and(|c| c.status == p.status && c.revision != p.expected_revision)
        });
    }
    fn accept_current(&mut self) -> bool {
        if !self.pending.as_ref().is_some_and(|p| {
            self.impact
                .as_ref()
                .is_some_and(|i| i.connection.connection_id == p.connection_id)
        }) {
            return false;
        }
        self.pending = None;
        self.refresh_sources = true;
        self.acknowledged = false;
        self.close_review(); // Another decision always starts a new impact review.
        true
    }
    fn restore(&mut self, dataset: &str) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let Ok(value) = serde_json::from_str::<Value>(&self.recovery) else {
            return false;
        };
        if value["dataset_id"].as_str() != Some(dataset) {
            return false;
        }
        let Ok(change) = serde_json::from_value::<contract::Change>(value["change"].clone()) else {
            return false;
        };
        if !change.valid() {
            return false;
        }
        self.pending = Some(change);
        self.acknowledged = false;
        self.close_review();
        self.recovery.clear();
        true
    }
}
#[derive(Clone)]
enum Read {
    List(u32),
    Impact(String, u32),
    Write,
}
#[derive(Default)]
struct Entry {
    state: State,
    job: RemoteJob,
    read: Option<Read>,
}
impl Entry {
    fn close(&mut self) {
        self.job.cancel_observation();
        self.read = None;
        self.state.close_review();
    }
    fn poll(&mut self) {
        let Some(message) = self.job.poll() else {
            return;
        };
        let Some(read) = self.read.take() else {
            return;
        };
        let result = match read {
            Read::Write => {
                self.state.receive_write(message.result);
                Ok(())
            }
            Read::List(offset) => message.result.and_then(|v| {
                self.state.connections = Some(contract::parse_managed(v, offset)?);
                Ok(())
            }),
            Read::Impact(id, offset) => message.result.and_then(|v| {
                self.state.impact = Some(contract::parse_impact(v, &id, offset)?);
                Ok(())
            }),
        };
        if result.is_err() {
            self.state.message =
                "Current metadata could not be read. Nothing was retried; read again.".into();
        }
    }
    fn request(
        &mut self,
        read: Read,
        request: ApiRequest,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.state.impact = None;
        self.state.confirmed = false;
        self.state.message.clear();
        if matches!(read, Read::List(_)) {
            self.state.connections = None;
        }
        let kind = if matches!(read, Read::Write) {
            "connection_change"
        } else {
            "connection_metadata"
        };
        self.read = Some(read);
        self.job.request(kind, api, request, runtime, ctx);
    }
}
#[derive(Default)]
pub struct ConnectionControls {
    session: Option<DesktopApi>,
    dataset: String,
    enabled: bool,
    entries: BTreeMap<String, Entry>,
}
impl ConnectionControls {
    /// Pending connection decisions block new manual-sync reviews, never exact UUID recovery.
    pub fn has_pending(&self, dataset: &str) -> bool {
        self.entries
            .get(dataset)
            .is_some_and(|e| e.state.pending.is_some())
    }
    pub fn take_source_refresh(&mut self, dataset: &str) -> bool {
        self.entries
            .get_mut(dataset)
            .is_some_and(|e| std::mem::take(&mut e.state.refresh_sources))
    }

    pub fn close_reviews(&mut self) {
        for entry in self.entries.values_mut() {
            entry.close();
        }
    }
    fn bind(&mut self, api: &DesktopApi, dataset: &str, enabled: bool) {
        if self
            .session
            .as_ref()
            .is_some_and(|old| !api.same_session(old))
        {
            self.close_reviews();
            *self = Self::default();
        }
        self.session = Some(api.clone());
        if self.dataset != dataset || self.enabled != enabled {
            self.close_reviews();
            self.dataset = dataset.into();
            self.enabled = enabled;
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        dataset: &str,
        enabled: bool,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) -> bool {
        self.bind(api, dataset, enabled);
        let entry = self.entries.entry(dataset.into()).or_default();
        entry.poll();
        egui::CollapsingHeader::new("Saved connection controls").show(ui, |ui| {
            ui.label("Pause affects every binding using this connection. It blocks new syncs and stops existing runs at their next authorization check. Already-issued work may finish and partial effects remain. Indexed content and provider credentials are unchanged. Resume never starts a sync.");
            if !enabled { ui.label("New changes require current connection-control access. Refresh source state; metadata recovery remains available."); }
            entry.job.status_ui(ui);
            if !entry.state.message.is_empty() { ui.label(&entry.state.message); }
            if entry.state.pending.is_none() {
                if ui.add_enabled(enabled && !entry.job.busy, egui::Button::new("Load saved connections")).clicked() {
                    if let Ok(r) = contract::list_request(dataset, 0) { entry.request(Read::List(0), r, api, runtime, ctx); }
                }
                if let Some(page) = entry.state.connections.clone() {
                    for connection in page.connections {
                        ui.label(format!("{} · {} · {} · {}", connection.name, connection.domain, connection.status, connection.sync_mode));
                        if ui.add_enabled(enabled && !entry.job.busy, egui::Button::new(format!("Review impact: {}", connection.name))).clicked() {
                            if let Ok(r) = contract::impact_request(dataset, &connection.connection_id, 0) { entry.request(Read::Impact(connection.connection_id, 0), r, api, runtime, ctx); }
                        }
                    }
                    if let Some(offset) = page.next_offset { if ui.add_enabled(enabled && !entry.job.busy, egui::Button::new("Next connections")).clicked() {
                        if let Ok(r) = contract::list_request(dataset, offset) { entry.request(Read::List(offset), r, api, runtime, ctx); }
                    } }
                }
                ui.label("After an interrupted application session, restore your saved metadata before making another change. Recovery is session-only unless you save this text yourself.");
                ui.add_enabled(!entry.job.busy, egui::TextEdit::multiline(&mut entry.state.recovery).hint_text("Saved recovery metadata (no credentials)").desired_rows(2));
                if ui.add_enabled(!entry.job.busy && !entry.state.recovery.is_empty(), egui::Button::new("Restore pending change (read only)")).clicked() {
                    if !entry.state.restore(dataset) { entry.state.message = "Invalid recovery metadata or wrong dataset. No request sent.".into(); }
                }
            }
            if let Some(pending) = entry.state.pending.clone() {
                ui.label(if entry.state.acknowledged { "Change acknowledged. Read current metadata before continuing." } else { "Change outcome unconfirmed. Read current metadata; no retry will be sent." });
                ui.label(format!("Connection: {} · expected {} at {} → {}", pending.connection_id, pending.expected_status, pending.expected_revision, pending.status));
                let mut recovery = serde_json::to_string(&json!({"dataset_id":dataset,"change":pending})).unwrap_or_default();
                ui.label("Save this non-secret metadata for recovery after closing the app:");
                ui.add(egui::TextEdit::multiline(&mut recovery).interactive(false).desired_rows(2));
                if ui.add_enabled(!entry.job.busy, egui::Button::new("Read current connection status")).clicked() {
                    if let Ok(r) = contract::impact_request(dataset, &pending.connection_id, 0) { entry.request(Read::Impact(pending.connection_id, 0), r, api, runtime, ctx); }
                }
            }
            if let Some(impact) = entry.state.impact.clone() {
                ui.separator();
                ui.label(format!("{} · {} · {} · revision {}", impact.connection.name, impact.connection.domain, impact.connection.status, impact.connection.revision));
                ui.label(format!("Affected bindings across all datasets: {}", impact.binding_count));
                for binding in &impact.bindings { ui.label(format!("{} · dataset {} · {}", binding.space_name.as_deref().unwrap_or(&binding.binding_id), binding.dataset_id, binding.status.as_deref().unwrap_or("unknown"))); }
                ui.label(format!("Hidden bindings on this page: {}. Counts and access may change after this snapshot.", impact.hidden_count));
                if let Some(offset) = impact.next_offset { if ui.add_enabled(!entry.job.busy, egui::Button::new("Next affected bindings")).clicked() {
                    if let Ok(r) = contract::impact_request(dataset, &impact.connection.connection_id, offset) { entry.request(Read::Impact(impact.connection.connection_id.clone(), offset), r, api, runtime, ctx); }
                } }
                if entry.state.pending.is_some() {
                    ui.label("Current metadata describes the present state; it does not prove who made a change. Accepting it never retries the earlier request.");
                    if ui.add_enabled(!entry.job.busy, egui::Button::new("Use this current state")).clicked() { entry.state.accept_current(); }
                } else if impact.connection.change().is_ok() {
                    ui.add_enabled_ui(enabled && !entry.job.busy, |ui| {
                        ui.checkbox(&mut entry.state.confirmed, "I understand this changes the connection for all affected bindings.");
                        let label = if impact.connection.status == "active" { "Pause connection once" } else { "Resume connection once" };
                        if ui.add_enabled(entry.state.confirmed, egui::Button::new(label)).clicked() {
                            if let Ok(r) = entry.state.submit(dataset, enabled) { entry.request(Read::Write, r, api, runtime, ctx); }
                        }
                    });
                } else { ui.label("This connection state does not support pause or resume."); }
            }
        });
        entry.job.expired
    }
}

#[cfg(test)]
mod connection_control_tests {
    use super::*;
    fn row(status: &str, revision: &str) -> Value {
        json!({"connection_id":"c","name":"Saved","domain":"example.atlassian.net","status":status,"sync_mode":"manual","revision":revision})
    }
    fn impact(status: &str, revision: &str) -> contract::Impact {
        contract::parse_impact(json!({"connection":row(status,revision),"binding_count":51,"bindings":[],"hidden_count":50,"next_offset":50}),"c",0).unwrap()
    }
    fn reviewed() -> State {
        State {
            impact: Some(impact("active", "r1")),
            confirmed: true,
            ..Default::default()
        }
    }
    #[test]
    fn one_explicit_review_one_cas_and_no_blind_replay() {
        let mut s = reviewed();
        assert!(s.submit("d", false).is_err());
        s.confirmed = false;
        assert!(s.submit("d", true).is_err());
        s.confirmed = true;
        let r = s.submit("d", true).unwrap();
        assert_eq!(r.method, reqwest::Method::PATCH);
        assert_eq!(s.pending.as_ref().unwrap().expected_revision, "r1");
        assert!(s.impact.is_none());
        assert!(!s.confirmed);
        s.impact = Some(impact("active", "r1"));
        s.confirmed = true;
        assert!(s.submit("d", true).is_err());
    }
    #[test]
    fn every_rejection_and_unknown_response_requires_readback() {
        for result in [
            Err(ApiError::Transport),
            Err(ApiError::Status(403)),
            Err(ApiError::Status(409)),
            Err(ApiError::Status(404)),
            Err(ApiError::Status(401)),
            Ok(Value::Null),
            Ok(row("active", "r2")),
            Ok(row("disabled", "r1")),
        ] {
            let mut s = reviewed();
            s.submit("d", true).unwrap();
            s.receive_write(result);
            assert!(!s.acknowledged);
            assert!(s.pending.is_some());
            assert!(!s.accept_current());
            assert!(s.submit("d", true).is_err());
        }
    }
    #[test]
    fn acknowledged_change_still_requires_read_and_explicit_accept() {
        let mut s = reviewed();
        s.submit("d", true).unwrap();
        s.receive_write(Ok(row("disabled", "r2")));
        assert!(s.acknowledged);
        assert!(s.pending.is_some());
        assert!(!s.accept_current());
        s.impact = Some(impact("disabled", "r2"));
        assert!(s.accept_current());
        assert!(s.pending.is_none());
        assert!(s.impact.is_none());
        assert!(!s.confirmed);
        assert!(s.submit("d", true).is_err());
        s.impact = Some(impact("disabled", "r2"));
        s.confirmed = true;
        let r = s.submit("d", true).unwrap();
        assert_eq!(r.body.unwrap()["status"], "active");
    }
    #[test]
    fn unchanged_and_superseded_readback_can_be_acknowledged_without_replay() {
        for (status, revision) in [
            ("active", "r1"),
            ("active", "r9"),
            ("disabled", "r2"),
            ("error", "r7"),
        ] {
            let mut s = reviewed();
            s.submit("d", true).unwrap();
            s.impact = Some(impact(status, revision));
            assert!(s.accept_current());
            assert!(s.pending.is_none());
            assert!(s.submit("d", true).is_err());
        }
    }
    #[test]
    fn metadata_recovery_is_dataset_bound_and_read_only() {
        let mut s = reviewed();
        s.submit("d", true).unwrap();
        let text = json!({"dataset_id":"d","change":s.pending}).to_string();
        let mut restored = State {
            recovery: text.clone(),
            ..Default::default()
        };
        assert!(!restored.restore("other"));
        assert!(restored.restore("d"));
        assert!(restored.pending.is_some());
        assert!(restored.submit("d", true).is_err());
        assert!(!restored.accept_current());
        restored.recovery = text;
        assert!(!restored.restore("d"));
        restored.impact = Some(impact("disabled", "r2"));
        assert!(restored.accept_current());
    }
    #[test]
    fn route_dataset_and_capability_changes_discard_reviews_keep_pending() {
        let api = DesktopApi::new("https://example.test", "synthetic".into()).unwrap();
        let mut c = ConnectionControls::default();
        c.bind(&api, "d", true);
        let mut e = Entry {
            state: reviewed(),
            ..Default::default()
        };
        e.state.submit("d", true).unwrap();
        e.state.impact = Some(impact("disabled", "r2"));
        e.state.confirmed = true;
        c.entries.insert("d".into(), e);
        c.bind(&api, "other", true);
        assert!(c.entries["d"].state.pending.is_some());
        assert!(c.entries["d"].state.impact.is_none());
        c.bind(&api, "d", true);
        c.entries.get_mut("d").unwrap().state.impact = Some(impact("disabled", "r2"));
        c.bind(&api, "d", false);
        assert!(c.entries["d"].state.impact.is_none());
        assert!(c.entries["d"].state.pending.is_some());
        c.entries.get_mut("d").unwrap().state.impact = Some(impact("disabled", "r2"));
        c.close_reviews();
        assert!(c.entries["d"].state.impact.is_none());
        assert!(c.entries["d"].state.pending.is_some());
        assert!(c.entries["d"].read.is_none());
    }
    #[test]
    fn account_or_origin_change_discards_private_metadata() {
        let api = DesktopApi::new("https://example.test", "synthetic".into()).unwrap();
        for new in [
            DesktopApi::new("https://other.test", "synthetic".into()).unwrap(),
            DesktopApi::new("https://example.test", "different".into()).unwrap(),
        ] {
            let mut c = ConnectionControls::default();
            c.bind(&api, "d", true);
            c.entries.insert(
                "d".into(),
                Entry {
                    state: reviewed(),
                    ..Default::default()
                },
            );
            c.bind(&new, "d", true);
            assert!(c.entries.is_empty());
        }
    }
    #[test]
    fn foreign_impact_cannot_release_pending() {
        let mut s = reviewed();
        s.submit("d", true).unwrap();
        let mut foreign = impact("disabled", "r2");
        foreign.connection.connection_id = "other".into();
        s.impact = Some(foreign);
        assert!(!s.accept_current());
    }
    #[test]
    fn queued_read_cannot_revive_closed_dataset_route_or_capability_review() {
        // In-process completed futures only: no transport, listener or GUI is used.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let ctx = egui::Context::default();
        let api = DesktopApi::new("https://example.test", "synthetic".into()).unwrap();
        for transition in ["dataset", "route", "capability"] {
            let mut controls = ConnectionControls::default();
            controls.bind(&api, "d", true);
            let mut entry = Entry {
                state: reviewed(),
                ..Default::default()
            };
            entry.state.submit("d", true).unwrap();
            entry.read = Some(Read::Impact("c".into(), 0));
            entry.job.start(
                "connection_metadata",
                async { Ok(serde_json::to_value(impact("disabled", "r2")).unwrap()) },
                &runtime,
                &ctx,
            );
            runtime.block_on(async {
                tokio::task::yield_now().await;
            });
            controls.entries.insert("d".into(), entry);
            match transition {
                "dataset" => controls.bind(&api, "other", true),
                "route" => controls.close_reviews(),
                _ => controls.bind(&api, "d", false),
            }
            let entry = controls.entries.get_mut("d").unwrap();
            entry.poll();
            assert!(entry.state.impact.is_none());
            assert!(!entry.state.confirmed);
            assert!(entry.state.pending.is_some());
            assert!(!entry.state.accept_current());
        }
    }
    #[test]
    fn pending_blocks_sync_until_accept_requests_one_metadata_refresh() {
        let mut controls = ConnectionControls::default();
        let mut entry = Entry {
            state: reviewed(),
            ..Default::default()
        };
        entry.state.submit("d", true).unwrap();
        controls.entries.insert("d".into(), entry);
        assert!(controls.has_pending("d"));
        assert!(!controls.take_source_refresh("d"));
        let state = &mut controls.entries.get_mut("d").unwrap().state;
        state.receive_write(Ok(row("disabled", "r2")));
        assert!(!state.accept_current());
        assert!(controls.has_pending("d"));
        let state = &mut controls.entries.get_mut("d").unwrap().state;
        state.impact = Some(impact("disabled", "r2"));
        assert!(state.accept_current());
        assert!(!controls.has_pending("d"));
        assert!(controls.take_source_refresh("d"));
        assert!(!controls.take_source_refresh("d"));
        assert!(!controls.take_source_refresh("other"));
    }
}
