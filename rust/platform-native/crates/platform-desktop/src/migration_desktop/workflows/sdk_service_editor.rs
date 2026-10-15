//! Typed official-SDK create-only registration and existing service edit routes.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::{JobResult, RemoteJob, json_view, select_text, text_field};
use ai_platform_client::sdk_services::{self, SavedService, Settings};
use eframe::egui;

#[derive(Default)]
pub struct SdkServiceEditor {
    pub job: RemoteJob,
    selected: String,
    id: String,
    name: String,
    adapter: String,
    settings: Settings,
    key: String,
    saved: Option<SavedService>,
    creating: bool,
    target: String,
    review: Option<ApiRequest>,
    confirmed: bool,
    // A submitted write remains fenced even if its observation is abandoned.
    recovery_id: Option<String>,
    refresh_for_review: bool,
    checking_absence: bool,
    recovery_read_attempted: bool,
    acknowledge_unresolved: bool,
    unresolved: Vec<String>,
}
impl SdkServiceEditor {
    pub fn leave(&mut self) {
        let selected = self.selected.clone();
        self.abandon(&selected);
    }
    fn abandon(&mut self, selected: &str) {
        let recovery = self.recovery_id.take();
        let unresolved = std::mem::take(&mut self.unresolved);
        self.job.cancel_observation();
        let expired = self.job.expired;
        *self = Self {
            selected: selected.into(),
            id: selected.into(),
            recovery_id: recovery,
            unresolved,
            ..Self::default()
        };
        self.job.expired = expired;
    }
    fn load(
        &mut self,
        id: &str,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        match sdk_services::load_request(id) {
            Ok(request) => {
                self.target = id.into();
                self.saved = None;
                self.job
                    .request("load_sdk_service", api, request, runtime, ctx);
            }
            Err(error) => self.job.status = error.to_string(),
        }
    }
    fn accept_read(&mut self, value: &serde_json::Value) -> Result<(), ApiError> {
        let saved = sdk_services::read_service(value, &self.target).map_err(ApiError::from)?;
        if self.recovery_id.as_ref().is_some_and(|id| id != &saved.id) {
            return Err(ApiError::Contract(
                "Reload the exact submitted service before another change",
            ));
        }
        let recovering = self.recovery_id.take().is_some();
        if self.refresh_for_review {
            if saved.framework != self.adapter {
                self.key.clear();
                self.refresh_for_review = false;
                return Err(ApiError::Contract(
                    "The service framework changed. Reload before editing it.",
                ));
            }
            let request = sdk_services::update_request(&saved, &self.settings, &self.key)
                .map_err(ApiError::from);
            self.key.clear();
            self.refresh_for_review = false;
            self.saved = Some(saved.clone());
            self.review = Some(request?);
        } else {
            self.settings = saved.settings.clone();
            self.key.clear();
        }
        self.id = saved.id.clone();
        self.adapter = saved.framework.clone();
        self.creating = false;
        self.saved = Some(saved);
        self.job.outcome_unknown = false;
        self.job.status = if recovering {
            "Current saved settings loaded. Review them before another change; a configured-key flag cannot verify a particular key replacement.".into()
        } else {
            "Saved SDK settings loaded".into()
        };
        Ok(())
    }
    fn prepare_create(&mut self) -> Result<(), ApiError> {
        self.review = Some(
            sdk_services::create_request(
                &self.id,
                &self.name,
                &self.adapter,
                &self.settings,
                &self.key,
            )
            .map_err(ApiError::from)?,
        );
        self.key.clear();
        self.confirmed = false;
        Ok(())
    }
    fn take_write(&mut self) -> Option<ApiRequest> {
        if !self.confirmed || self.recovery_id.is_some() || self.job.busy {
            return None;
        }
        let request = self.review.take()?;
        self.target = self.id.clone();
        self.recovery_id = Some(self.target.clone());
        self.recovery_read_attempted = false;
        self.acknowledge_unresolved = false;
        self.saved = None;
        self.confirmed = false;
        Some(request)
    }
    // Returns true only when a successful write needs a new public detail read.
    fn finish(&mut self, message: JobResult) -> bool {
        if message.kind == "save_sdk_service" {
            self.key.clear();
            self.refresh_for_review = false;
            if message.result.is_ok() {
                // A registration/update acknowledgement is not a public detail readback.
                return true;
            }
            if self.creating {
                let status = match &message.result { Err(ApiError::Status(status)) => Some(*status), _ => None };
                self.job.status = format!("{}. Read the submitted service before another change; no request was replayed.", sdk_services::registration_failure(status));
            }
        } else if self.checking_absence {
            self.checking_absence = false;
            match message.result {
                Err(ApiError::Status(404)) => {
                    if let Err(error) = self.prepare_create() {
                        self.key.clear();
                        self.job.status = error.to_string();
                    }
                }
                Ok(value) => {
                    self.key.clear();
                    self.creating = false;
                    if let Err(error) = self.accept_read(&value) {
                        self.job.status = error.to_string();
                    } else {
                        self.job.status = "This ID already exists. Its current settings were loaded; review a separate edit.".into();
                    }
                }
                Err(_) => {
                    self.key.clear();
                }
            }
        } else if let Ok(value) = message.result {
            self.recovery_read_attempted = self.recovery_id.is_some();
            if let Err(error) = self.accept_read(&value) {
                self.key.clear();
                self.refresh_for_review = false;
                self.job.status = error.to_string();
            }
        } else {
            self.recovery_read_attempted = self.recovery_id.is_some();
            self.key.clear();
            self.refresh_for_review = false;
        }
        false
    }
    fn abandon_unresolved(&mut self) {
        if self.job.busy || !self.recovery_read_attempted || !self.acknowledge_unresolved {
            return;
        }
        if let Some(id) = self.recovery_id.take() {
            self.unresolved.push(id);
        }
        let selected = self.selected.clone();
        self.abandon(&selected);
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if self.selected != selected {
            self.abandon(selected);
        }
        if let Some(message) = self.job.poll() {
            if self.finish(message) {
                let id = self.target.clone();
                self.load(&id, api, runtime, ctx);
            }
        }
        egui::CollapsingHeader::new("Official SDK service configuration").default_open(true).show(ui, |ui| {
            self.job.status_ui(ui);
            if let Some(id) = self.recovery_id.clone() {
                ui.label(format!("Submitted service: {id}. Read its saved settings before any further write. No request will be replayed."));
                if ui.add_enabled(!self.job.busy, egui::Button::new("Read submitted service")).clicked() {
                    self.refresh_for_review = false;
                    self.load(&id, api, runtime, ctx);
                }
                ui.add(egui::TextEdit::singleline(&mut id.clone()).interactive(false));
                ui.checkbox(&mut self.acknowledge_unresolved, "I understand this service and its credential grant may still exist or appear later; another registration could duplicate the configuration.");
                if ui.add_enabled(!self.job.busy && self.recovery_read_attempted && self.acknowledge_unresolved, egui::Button::new("Abandon unresolved configuration locally")).clicked() {
                    self.abandon_unresolved();
                }
                return;
            }
            for id in self.unresolved.clone() {
                ui.horizontal(|ui| {
                    ui.label("Unresolved earlier service (remembered only in this session):");
                    ui.add(egui::TextEdit::singleline(&mut id.clone()).interactive(false));
                    if ui.add_enabled(!self.job.busy && self.review.is_none(), egui::Button::new("Read earlier service")).clicked() {
                        self.key.clear(); self.refresh_for_review = false; self.load(&id, api, runtime, ctx);
                    }
                });
            }
            ui.add_enabled_ui(!self.job.busy && self.review.is_none(), |ui| {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!selected.is_empty(), egui::Button::new("Load selected SDK service")).clicked() {
                        self.key.clear(); self.creating = false;
                        self.load(selected, api, runtime, ctx);
                    }
                    if ui.button("New SDK service").clicked() {
                        self.abandon(selected); self.id = format!("sdk-{}", uuid::Uuid::new_v4()); self.creating = true; self.adapter = "openai_agents".into();
                    }
                });
                if self.creating || self.saved.is_some() {
                    if self.creating {
                        ui.label(format!("New service ID: {}", self.id));
                        text_field(ui, "Name", &mut self.name);
                        let previous = self.adapter.clone();
                        select_text(ui, "sdk_service_framework", &mut self.adapter, &["openai_agents", "claude_agent_sdk"]);
                        if self.adapter != previous { self.key.clear(); self.settings = Settings::default(); }
                    } else { ui.label(format!("{} · {}", self.id, self.adapter)); }
                    text_field(ui, "Model", &mut self.settings.model);
                    ui.label("Instructions (blank uses the server default)");
                    ui.add(egui::TextEdit::multiline(&mut self.settings.instructions).desired_rows(5));
                    ui.add(egui::DragValue::new(&mut self.settings.max_turns).range(1..=32).prefix("Maximum turns "));
                    ui.label(if self.creating { "Provider API key (required, write-only)" } else { "Replacement provider key (optional; blank keeps the saved key)" });
                    if let Some(saved) = &self.saved { ui.label(format!("Provider key configured: {}", saved.key_configured)); }
                    ui.add(egui::TextEdit::singleline(&mut self.key).password(true));
                    if ui.button("Review exact configuration").clicked() {
                        self.confirmed = false;
                        if self.creating {
                            match sdk_services::create_request(&self.id, &self.name, &self.adapter, &self.settings, &self.key) {
                                Ok(_) => { self.checking_absence = true; let id = self.id.clone(); self.load(&id, api, runtime, ctx); }
                                Err(error) => self.job.status = error.to_string(),
                            }
                        } else {
                            // Refresh metadata before building the frozen patch; this is not CAS.
                            self.refresh_for_review = true;
                            let id = self.id.clone(); self.load(&id, api, runtime, ctx);
                        }
                    }
                }
            });
            if let Some(request) = &self.review {
                ui.separator();
                ui.strong(format!("Review {} service {}", self.adapter, self.id));
                let mut public = request.body.clone().unwrap_or_default();
                let replaces_key = public.get("connector_config").is_some();
                if let Some(object) = public.as_object_mut() { object.remove("connector_config"); }
                json_view(ui, &public);
                ui.label(if replaces_key { "A write-only provider key will be submitted; its value is hidden." } else { "The saved key will remain unchanged." });
                if self.creating {
                    ui.label("Registration atomically creates this ID only if absent. An existing service causes a conflict and is not overwritten. Requires a supported native server/profile; no legacy fallback.");
                } else {
                    ui.label("Service metadata is replaced by PUT. A concurrent edit after this refresh can still be overwritten; no atomic version check is available.");
                }
                ui.checkbox(&mut self.confirmed, "I reviewed this exact service and configuration");
                if ui.add_enabled(self.confirmed && !self.job.busy, egui::Button::new("Submit once")).clicked() {
                    if let Some(request) = self.take_write() { self.job.request("save_sdk_service", api, request, runtime, ctx); }
                }
                if ui.button("Cancel review").clicked() { self.review = None; self.confirmed = false; }
            }
        });
    }
}

#[cfg(test)]
#[path = "sdk_service_editor_tests.rs"]
mod tests;
