use super::{
    api::ApiRequest,
    catalog::{self, Section},
    state::{DesktopWorkspace, LoadState},
};
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
fn list_items(value: &Value) -> Vec<&Value> {
    if let Some(items) = value.as_array() {
        return items.iter().collect();
    }
    for key in [
        "tools",
        "devices",
        "sessions",
        "items",
        "skills",
        "servers",
        "agents",
        "datasets",
        "models",
        "providers",
        "services",
        "tasks",
        "users",
        "roles",
        "keys",
        "events",
        "grants",
        "data",
    ] {
        if let Some(items) = value[key].as_array() {
            return items.iter().collect();
        }
    }
    Vec::new()
}
fn title(item: &Value) -> String {
    for key in [
        "display_name",
        "name",
        "title",
        "email",
        "key",
        "device_id",
        "session_id",
        "id",
    ] {
        if let Some(value) = item[key].as_str() {
            return value.into();
        }
    }
    "Record".into()
}
fn summary(item: &Value) -> String {
    [
        "status", "platform", "kind", "effect", "version", "category",
    ]
    .iter()
    .filter_map(|k| item[*k].as_str())
    .collect::<Vec<_>>()
    .join(" · ")
}
fn value_view(ui: &mut egui::Ui, label: &str, value: &Value, depth: usize) {
    if depth > 12 {
        ui.label("Nested content omitted");
        return;
    }
    match value {
        Value::Object(map) => {
            egui::CollapsingHeader::new(label)
                .default_open(depth < 1)
                .show(ui, |ui| {
                    for (key, item) in map {
                        value_view(ui, key, item, depth + 1);
                    }
                });
        }
        Value::Array(items) => {
            egui::CollapsingHeader::new(format!("{label} ({})", items.len()))
                .default_open(depth < 1)
                .show(ui, |ui| {
                    for (index, item) in items.iter().enumerate() {
                        ui.push_id(index, |ui| {
                            value_view(ui, &format!("{}", index + 1), item, depth + 1)
                        });
                    }
                });
        }
        Value::String(text) => {
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!("{label}:"));
                ui.label(text);
            });
        }
        _ => {
            ui.label(format!("{label}: {value}"));
        }
    }
}
fn mutation(method: Method, path: &[&str], body: Option<Value>) -> ApiRequest {
    ApiRequest::mutation(method, path, body)
}
impl DesktopWorkspace {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll();
        if let Some(api) = self.api.clone() {
            self.workflows.conversation.poll(&api, runtime, ctx);
            self.expired |= self.workflows.conversation.expired;
            if self.section != Section::Tasks || !self.workflows.tasks_updates {
                self.workflows.scheduled_notifications.close_observation();
            }
            self.workflows.scheduled_notifications.poll(&api, runtime, ctx);
            self.expired |= self.workflows.scheduled_notifications.expired();
        }
        ui.separator();
        ui.heading("Platform workspace");
        ui.horizontal_wrapped(|ui| {
            for section in Section::ALL {
                if ui
                    .selectable_label(self.section == *section, section.title())
                    .clicked()
                {
                    self.select(*section);
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading(self.section.title());
            if ui
                .add_enabled(!self.mutation_busy, egui::Button::new("Refresh"))
                .clicked()
            {
                if self.section == Section::Tasks {
                    if let Some(api) = self.api.clone() {
                        if self.workflows.tasks_updates {
                            self.workflows.scheduled_notifications.refresh(&api, runtime, ctx);
                        } else {
                            self.workflows.tasks.refresh(&api, runtime, ctx);
                        }
                    }
                } else if self.section == Section::ScheduledTasks {
                    if let Some(api) = self.api.clone() {
                        self.workflows.scheduled_tasks.refresh(&api, runtime, ctx);
                    }
                } else {
                    self.refresh(runtime, ctx);
                }
            }
            if matches!(self.page().state, LoadState::Loading) {
                ui.spinner();
                if ui.button("Cancel read").clicked() {
                    self.cancel_read();
                }
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        if let LoadState::Failed(message) = &self.page().state {
            ui.colored_label(egui::Color32::LIGHT_RED, message);
        }
        if self.api.is_none() {
            ui.label("Sign in to access the platform workspace.");
            return;
        }
        if self.section == Section::Conversation {
            if let Some(api) = self.api.clone() {
                self.workflows.conversation.ui(ui, &api, runtime, ctx);
                self.expired |= self.workflows.conversation.expired;
            }
            return;
        }
        if self.section == Section::Tasks {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.workflows.tasks_updates, false, "Execution tasks");
                ui.selectable_value(&mut self.workflows.tasks_updates, true, "Updates");
            });
            if let Some(api) = self.api.clone() {
                if self.workflows.tasks_updates {
                    self.workflows.scheduled_notifications.ui(ui, &api, runtime, ctx);
                    self.expired |= self.workflows.scheduled_notifications.expired();
                } else {
                    self.workflows.scheduled_notifications.close_observation();
                    self.workflows.tasks.ui(ui, &api, runtime, ctx);
                    self.expired |=
                        self.workflows.tasks.job.expired || self.workflows.tasks.list_job.expired;
                }
            }
            return;
        }
        if self.section == Section::ScheduledTasks {
            if let Some(api) = self.api.clone() {
                self.workflows.scheduled_tasks.ui(ui, &api, runtime, ctx);
                self.expired |= self.workflows.scheduled_tasks.job.expired;
            }
            return;
        }
        let section = self.section;
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.text_edit_singleline(&mut self.page_mut().filter);
        });
        let filter = self.page().filter.to_lowercase();
        let data = self.page().data.clone();
        let selected = self.page().selected_id.clone();
        let items = list_items(&data);
        if items.is_empty() {
            if !data.is_null() {
                value_view(ui, "Result", &data, 0);
            } else if !matches!(self.page().state, LoadState::Loading) {
                ui.label("Refresh to load this section.");
            }
        } else {
            egui::ScrollArea::vertical()
                .id_salt("workspace-list")
                .max_height(260.0)
                .show(ui, |ui| {
                    for (index, item) in items.iter().enumerate() {
                        let name = title(item);
                        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let id = section
                            .identity_fields()
                            .iter()
                            .find_map(|key| item[*key].as_str())
                            .unwrap_or("");
                        ui.push_id(index, |ui| {
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(!id.is_empty() && selected == id, &name)
                                    .clicked()
                                    && !id.is_empty()
                                {
                                    chosen = Some(id.to_owned());
                                }
                                ui.weak(summary(item));
                            });
                        });
                    }
                });
        }
        if let Some(id) = chosen {
            self.choose_item(id);
        }
        let mut selection = self.page().selected_id.clone();
        ui.horizontal(|ui| {
            ui.label("Selected ID");
            ui.text_edit_singleline(&mut selection);
        });
        self.choose_item(selection);
        let selected = self.page().selected_id.clone();
        ui.add_enabled_ui(!selected.is_empty(), |ui| {
            ui.horizontal_wrapped(|ui| {
                for view in catalog::detail_views(section) {
                    if ui.button(*view).clicked() {
                        match catalog::detail_request(section, &selected, view) {
                            Ok(request) => self.read(request, Some((*view).into()), runtime, ctx),
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                }
            });
        });
        if let Some(api) = self.api.clone() {
            match section {
                Section::LocalNodes => {
                    self.workflows
                        .node_events
                        .ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.node_events.job.expired;
                }
                Section::Capabilities => {
                    if ui.button("Load effective policies").clicked() {
                        self.read(
                            ai_platform_client::management::policies_request(),
                            Some("Effective policies".into()),
                            runtime,
                            ctx,
                        );
                    }
                    if let Some(tool) = list_items(&data).into_iter().find(|item| {
                        item["name"].as_str() == Some(selected.as_str())
                            || item["id"].as_str() == Some(selected.as_str())
                    }) {
                        value_view(ui, "Capability", tool, 0);
                    }
                }
                Section::Agents => {
                    self.workflows.agent.ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.agent.job.expired;
                }
                Section::Knowledge => {
                    self.workflows
                        .knowledge
                        .ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.knowledge.job.expired;
                }
                Section::Services => {
                    self.workflows.sdk_services.ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.sdk_services.job.expired;
                }
                Section::Providers => {
                    self.workflows
                        .provider
                        .ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.provider.job.expired;
                }
                Section::Mcp => {
                    self.workflows.mcp.ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.mcp.job.expired;
                }
                Section::Skills => {
                    self.workflows.skills.ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.skills.job.expired;
                }
                Section::Connectors => {
                    self.workflows
                        .connectors
                        .ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.connectors.job.expired;
                }
                Section::Models => {
                    self.workflows.models.ui(ui, &api, &selected, runtime, ctx);
                    self.expired |= self.workflows.models.job.expired;
                }
                Section::Usage => {
                    self.workflows.usage.ui(ui, &api, runtime, ctx);
                    self.expired |= self.workflows.usage.job.expired;
                }
                Section::Users | Section::Roles | Section::ApiKeys | Section::Settings => {
                    self.workflows
                        .admin
                        .ui(ui, &api, section, &selected, runtime, ctx);
                    self.expired |= self.workflows.admin.job.expired;
                }
                _ => {}
            }
        }
        ui.add_enabled_ui(!self.mutation_busy, |ui| self.actions(ui, runtime, ctx));
        if self.mutation_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Waiting for the submitted change. Do not repeat it.");
            });
        }
        if let Some(pairing) = &self.pairing {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong("Pairing challenge");
                ui.label(pairing["user_code"].as_str().unwrap_or(""));
                ui.label(format!(
                    "Expires: {}",
                    pairing["expires_at"].as_str().unwrap_or("")
                ));
                ui.label("Complete pairing in the trusted local companion.");
            });
        }
        let mut submit = false;
        let mut cancel = false;
        if let Some(pending) = &mut self.pending {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong(&pending.label);
                ui.label(format!(
                    "{} /{}",
                    pending.request.method,
                    pending.request.segments.join("/")
                ));
                if pending.request.body.is_some() {
                    ui.add(
                        egui::TextEdit::multiline(&mut pending.body_text)
                            .code_editor()
                            .desired_rows(8),
                    );
                }
                ui.checkbox(&mut pending.confirmation, "I reviewed this exact change");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            pending.confirmation,
                            egui::Button::new("Confirm and submit once"),
                        )
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        }
        if cancel {
            self.pending = None;
        }
        if submit {
            self.submit(runtime, ctx);
        }
        if let Some(result) = &self.mutation_result {
            value_view(ui, "Submitted change result", result, 0);
        }
        let detail = self.page().detail.clone();
        if !detail.is_null() {
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("workspace-detail")
                .max_height(420.0)
                .show(ui, |ui| {
                    value_view(ui, &self.page().detail_view, &detail, 0);
                });
            if section == Section::LocalNodes && self.page().detail_view == "grants" {
                let grants = list_items(&detail);
                for grant in grants {
                    let Some(id) = grant["grant_id"].as_str() else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        ui.label(title(grant));
                        if ui
                            .add_enabled(!self.mutation_busy, egui::Button::new("Revoke grant"))
                            .clicked()
                        {
                            self.prepare(
                                "Revoke directory/app/domain grant",
                                mutation(
                                    Method::DELETE,
                                    &["api", "v1", "local-nodes", &selected, "grants", id],
                                    None,
                                ),
                            );
                        }
                    });
                }
            }
        }
    }
    fn actions(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        let id = self.page().selected_id.clone();
        let section = self.section;
        ui.horizontal_wrapped(|ui|{match section{
        Section::LocalNodes=>{if ui.button("Create pairing challenge").clicked(){self.prepare("Create one-use pairing challenge",mutation(Method::POST,&["api","v1","local-nodes","pairing","challenges"],Some(json!({"expires_in_seconds":180}))));}},
        Section::Memory=>{let enabled=self.page().data["enabled"].as_bool().unwrap_or(false);if ui.button(if enabled{"Disable memory"}else{"Enable memory"}).clicked(){self.prepare("Change assistant memory setting",mutation(Method::PATCH,&["api","v1","assistant","memory"],Some(json!({"enabled":!enabled}))));}if ui.button("Add/update memory item").clicked(){self.prepare("Update assistant memory item",mutation(Method::PUT,&["api","v1","assistant","memory","items"],Some(json!({"key":id,"value":""}))));}if !id.is_empty()&&ui.button("Delete memory item").clicked(){self.prepare("Delete selected memory item",mutation(Method::DELETE,&["api","v1","assistant","memory","items"],None).query("key",&id));}},
        Section::Sessions=>{if ui.button("New session").clicked(){self.prepare("Create session",mutation(Method::POST,&["api","v1","sessions"],Some(json!({}))));}if !id.is_empty(){if ui.button("Rename session").clicked(){self.prepare("Update session",mutation(Method::PATCH,&["api","v1","sessions",&id],Some(json!({"metadata":{"title":""}}))));}if ui.button("Delete session").clicked(){self.prepare("Delete selected session",mutation(Method::DELETE,&["api","v1","sessions",&id],None));}}},
        Section::Skills if !id.is_empty()=>{for(enabled,label)in[(true,"Enable skill"),(false,"Disable skill")]{if ui.button(label).clicked(){self.prepare(label,mutation(Method::POST,&["api","v1","skills",&id,if enabled{"enable"}else{"disable"}],None));}}if ui.button("Delete skill").clicked(){self.prepare("Delete selected skill",mutation(Method::DELETE,&["api","v1","skills",&id],None));}},
        Section::Agents=>{if ui.button("Create agent").clicked(){self.prepare("Create agent draft",mutation(Method::POST,&["api","v1","agents"],Some(json!({"name":"","description":"","spec":default_agent_spec()}))));}if !id.is_empty(){for(operation,label)in[("copy","Copy agent"),("archive","Archive agent")]{if ui.button(label).clicked(){self.prepare(label,mutation(Method::POST,&["api","v1","agents",&id,operation],Some(json!({}))));}}}},
        Section::Knowledge=>{if ui.button("Create dataset").clicked(){self.prepare("Create knowledge dataset",mutation(Method::POST,&["api","v1","knowledge","datasets"],Some(json!({"name":"","description":"","embedding_provider":"","embedding_model":""}))));}if !id.is_empty()&&ui.button("Add text document").clicked(){self.prepare("Create text document",mutation(Method::POST,&["api","v1","knowledge",&id,"documents","text"],Some(json!({"content":"","title":""}))));}},
        Section::Users if !id.is_empty()=>{for(op,label)in[("enable","Enable user"),("disable","Disable user"),("reset-password","Reset user password")]{if ui.button(label).clicked(){self.prepare(label,mutation(Method::POST,&["api","v1","users",&id,op],None));}}},
        Section::Connectors=>{if ui.button("My connections").clicked(){self.read(ApiRequest::get(&["api","v1","connectors","mine"]),Some("My connections".into()),runtime,ctx);}},
        _=>{}
    }});
    }
}

fn default_agent_spec() -> Value {
    json!({"schema_version":"agent-spec/v1","identity":{"icon_url":null,"theme_color":"#7B7BE8","welcome_message":"","suggested_prompts":[]},"outcome":{"goal":"","triggers":[],"inputs":[],"human_boundaries":[],"success_criteria":[]},"instructions":"Fulfill the Agent purpose described in its name and description. Follow the configured capability, knowledge, memory, and safety policies.","model":{"model_id":"","provider_id":"dashscope","temperature":0.3,"max_tokens":4096},"capabilities":[],"knowledge":[],"memory":{"mode":"session"}})
}
