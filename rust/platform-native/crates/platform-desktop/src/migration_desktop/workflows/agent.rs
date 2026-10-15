//! Agent draft editor preserving the canonical AgentSpec and If-Match revision.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct AgentEditor {
    pub bindings: super::agent_bindings::BindingPicker,
    pub versions: super::version_preview::VersionPreview,
    pub governance: super::governance::GovernanceEditor,
    pub release: super::release::ReleaseEditor,
    pub preview: super::stream_panel::StreamPanel,
    pub preview_session: String,
    pub preview_revision: Option<u64>,
    pub preview_prompt: String,
    pub job: RemoteJob,
    pub agent_id: String,
    loaded_id: String,
    pub name: String,
    pub description: String,
    pub revision: Option<u64>,
    pub spec: Value,
    capabilities: String,
    knowledge: String,
    advanced: String,
    pub dirty: bool,
    pub confirm: bool,
    pub result: Value,
    pub catalogs: Value,
}
impl Default for AgentEditor {
    fn default() -> Self {
        Self {
            bindings: Default::default(),
            versions: Default::default(),
            governance: super::governance::GovernanceEditor::default(),
            release: super::release::ReleaseEditor::default(),
            preview: super::stream_panel::StreamPanel::default(),
            preview_session: String::new(),
            preview_revision: None,
            preview_prompt: String::new(),
            job: RemoteJob::default(),
            agent_id: String::new(),
            loaded_id: String::new(),
            name: String::new(),
            description: String::new(),
            revision: None,
            spec: default_spec(),
            capabilities: "[]".into(),
            knowledge: "[]".into(),
            advanced: String::new(),
            dirty: false,
            confirm: false,
            result: Value::Null,
            catalogs: Value::Null,
        }
    }
}
pub fn default_spec() -> Value {
    json!({"schema_version":"agent-spec/v1","identity":{"icon_url":null,"theme_color":"#7B7BE8","welcome_message":"","suggested_prompts":[]},"outcome":{"goal":"","triggers":[],"inputs":[],"human_boundaries":[],"success_criteria":[]},"instructions":"Fulfill the Agent purpose described in its name and description. Follow the configured capability, knowledge, memory, and safety policies.","model":{"model_id":"","provider_id":"dashscope","temperature":0.3,"max_tokens":4096},"capabilities":[],"knowledge":[],"memory":{"mode":"session"}})
}
fn string_edit(
    ui: &mut egui::Ui,
    value: &mut Value,
    key: &str,
    label: &str,
    multiline: bool,
) -> bool {
    let mut text = value[key].as_str().unwrap_or("").to_string();
    ui.label(label);
    let changed = if multiline {
        ui.add(egui::TextEdit::multiline(&mut text).desired_rows(4))
            .changed()
    } else {
        ui.text_edit_singleline(&mut text).changed()
    };
    if changed {
        value[key] = json!(text);
    }
    changed
}
impl AgentEditor {
    fn receive(&mut self) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                match message.kind.as_str() {
                    "load_agent" => {
                        let detail = &value["agent"];
                        self.name = detail["name"].as_str().unwrap_or("").into();
                        self.description = detail["description"].as_str().unwrap_or("").into();
                        self.accept_draft(value["draft"].clone());
                        self.loaded_id = value["requested_id"].as_str().unwrap_or("").into();
                    }
                    "save" => {
                        self.accept_draft(value["draft"].clone());
                        self.job.status = "Draft saved".into();
                    }
                    "create" => {
                        self.agent_id = value["agent"]["agent_id"]
                            .as_str()
                            .or_else(|| value["agent"]["id"].as_str())
                            .unwrap_or("")
                            .into();
                        self.loaded_id.clear();
                        self.revision = None;
                        self.result = value;
                        self.job.status =
                            "Agent created. Load its draft to continue editing.".into();
                        self.dirty = false;
                    }
                    "catalogs" => self.catalogs = value,
                    "preview_session" => {
                        self.preview_session = value["session_id"].as_str().unwrap_or("").into();
                        self.preview_revision = value["draft_revision"].as_u64();
                        self.result = value;
                    }
                    _ => self.result = value,
                }
            }
        }
    }
    fn accept_draft(&mut self, value: Value) {
        if value["spec"].is_object() {
            self.spec = value["spec"].clone();
            self.revision = value["revision"].as_u64();
            self.capabilities = serde_json::to_string_pretty(&self.spec["capabilities"])
                .unwrap_or_else(|_| "[]".into());
            self.knowledge = serde_json::to_string_pretty(&self.spec["knowledge"])
                .unwrap_or_else(|_| "[]".into());
            self.advanced = serde_json::to_string_pretty(&self.spec).unwrap_or_default();
            self.dirty = false;
            self.confirm = false;
        } else {
            self.job.status = "Draft response is missing its spec".into();
        }
    }
    fn payload(&self) -> Result<Value, ApiError> {
        let capabilities: Value =
            serde_json::from_str(&self.capabilities).map_err(|_| ApiError::Request)?;
        let knowledge: Value =
            serde_json::from_str(&self.knowledge).map_err(|_| ApiError::Request)?;
        if !capabilities.is_array() || !knowledge.is_array() || self.name.trim().is_empty() {
            return Err(ApiError::Request);
        }
        let mut spec = self.spec.clone();
        spec["capabilities"] = capabilities;
        spec["knowledge"] = knowledge;
        Ok(json!({"name":self.name,"description":self.description,"spec":spec}))
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.receive();
        egui::CollapsingHeader::new("Agent editor").default_open(true).show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui|{ui.horizontal(|ui|{text_field(ui,"Agent ID",&mut self.agent_id);if !selected.is_empty()&&ui.button("Use selected agent").clicked()&&!self.dirty{self.agent_id=selected.into();}if ui.button("New draft").clicked()&&(!self.dirty||self.confirm){let id=String::new();*self=Self::default();self.agent_id=id;}if ui.add_enabled(!self.agent_id.is_empty()&&(!self.dirty||self.confirm),egui::Button::new("Load draft")).clicked(){let api=api.clone();let id=self.agent_id.clone();self.job.start("load_agent",async move{let agent=api.execute(ApiRequest::get(&["api","v1","agents",&id])).await?;let draft=api.agent_draft(&id).await?;Ok(json!({"agent":agent,"draft":draft,"requested_id":id}))},runtime,ctx);}});
        if self.dirty{ui.label("Unsaved draft changes");}ui.horizontal(|ui|{ui.label("Name");self.dirty|=ui.text_edit_singleline(&mut self.name).changed();});ui.label("Description");self.dirty|=ui.text_edit_multiline(&mut self.description).changed();self.dirty|=string_edit(ui,&mut self.spec,"instructions","Instructions",true);self.dirty|=string_edit(ui,&mut self.spec["outcome"],"goal","Outcome goal",true);self.dirty|=string_edit(ui,&mut self.spec["identity"],"welcome_message","Welcome message",true);
        ui.horizontal(|ui|{self.dirty|=string_edit(ui,&mut self.spec["model"],"model_id","Model ID (blank uses deployment default)",false);self.dirty|=string_edit(ui,&mut self.spec["model"],"provider_id","Provider ID",false);});let mut temperature=self.spec["model"]["temperature"].as_f64().unwrap_or(0.3);if ui.add(egui::Slider::new(&mut temperature,0.0..=2.0).text("Temperature")).changed(){self.spec["model"]["temperature"]=json!(temperature);self.dirty=true;}let mut max_tokens=self.spec["model"]["max_tokens"].as_u64().unwrap_or(4096);if ui.add(egui::DragValue::new(&mut max_tokens).range(1..=1_000_000).prefix("Max tokens ")).changed(){self.spec["model"]["max_tokens"]=json!(max_tokens);self.dirty=true;}
        ui.horizontal(|ui|{ui.label("Memory");let mut mode=self.spec["memory"]["mode"].as_str().unwrap_or("session").to_string();for option in ["off","session","user"]{if ui.selectable_value(&mut mode,option.into(),option).changed(){self.spec["memory"]["mode"]=json!(mode);self.dirty=true;}}});
        self.dirty|=self.bindings.ui(ui,api,&mut self.capabilities,&mut self.knowledge,&mut self.spec["model"],runtime,ctx);self.job.expired|=self.bindings.job.expired;
        egui::CollapsingHeader::new("Capabilities and knowledge bindings").show(ui,|ui|{ui.label("Canonical capability bindings");self.dirty|=ui.add(egui::TextEdit::multiline(&mut self.capabilities).code_editor().desired_rows(5)).changed();ui.label("Canonical knowledge bindings");self.dirty|=ui.add(egui::TextEdit::multiline(&mut self.knowledge).code_editor().desired_rows(5)).changed();if ui.button("Load available catalogs").clicked(){let api=api.clone();self.job.start("catalogs",async move{let models=api.execute(ApiRequest::get(&["api","v1","assistant","models"])).await?;let tools=api.execute(ApiRequest::get(&["api","v1","assistant","tools"])).await?;let knowledge=api.execute(ApiRequest::get(&["api","v1","knowledge","datasets"])).await?;let skills=api.execute(ApiRequest::get(&["api","v1","skills"]).query("enabled_only",true)).await?;Ok(json!({"models":models,"tools":tools,"knowledge":knowledge,"skills":skills}))},runtime,ctx);}if !self.catalogs.is_null(){json_view(ui,&self.catalogs);}});
        egui::CollapsingHeader::new("Full AgentSpec").show(ui,|ui|{if ui.button("Copy current fields into spec editor").clicked(){if let Ok(payload)=self.payload(){self.advanced=serde_json::to_string_pretty(&payload["spec"]).unwrap_or_default();}}ui.add(egui::TextEdit::multiline(&mut self.advanced).code_editor().desired_rows(10));if ui.button("Apply edited spec to form").clicked(){match json_object(&self.advanced){Ok(spec)if spec["schema_version"]=="agent-spec/v1"=>{self.spec=spec;self.capabilities=serde_json::to_string_pretty(&self.spec["capabilities"]).unwrap_or_default();self.knowledge=serde_json::to_string_pretty(&self.spec["knowledge"]).unwrap_or_default();self.dirty=true;},_=>self.job.status="Enter an agent-spec/v1 object".into()}}});
        ui.checkbox(&mut self.confirm,"I reviewed these changes (or allow discarding unsaved changes on reload)");let can_change=self.confirm&&!self.job.outcome_unknown;
        ui.horizontal(|ui|{if ui.add_enabled(can_change&&self.agent_id.is_empty(),egui::Button::new("Create agent")).clicked(){match self.payload(){Ok(body)=>self.job.request("create",api,ApiRequest::mutation(Method::POST,&["api","v1","agents"],Some(body)),runtime,ctx),Err(_)=>self.job.status="Name and valid capability/knowledge arrays are required".into()}}
            if ui.add_enabled(can_change&&self.loaded_id==self.agent_id&&self.revision.is_some(),egui::Button::new("Save draft")).clicked(){match self.payload(){Ok(body)=>{let revision=self.revision.unwrap();self.job.request("save",api,ApiRequest::mutation(Method::PUT,&["api","v1","agents",&self.agent_id,"draft"],Some(body)).header("If-Match",format!("\"{revision}\"")),runtime,ctx);self.confirm=false;},Err(_)=>self.job.status="Name and valid capability/knowledge arrays are required".into()}}
            if ui.add_enabled(can_change&&!self.dirty&&self.revision.is_some()&&self.loaded_id==self.agent_id,egui::Button::new("Create version")).clicked(){self.job.request("version",api,ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.agent_id,"versions"],Some(json!({}))).header("If-Match",format!("\"{}\"",self.revision.unwrap())),runtime,ctx);}
            if ui.add_enabled(can_change&&!self.dirty&&self.revision.is_some()&&self.loaded_id==self.agent_id,egui::Button::new("Create preview session")).clicked(){self.job.request("preview_session",api,ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.agent_id,"preview","sessions"],Some(json!({"draft_revision":self.revision}))),runtime,ctx);}
        });if let Some(revision)=self.revision{ui.label(format!("Loaded draft revision: {revision}"));}if !self.result.is_null(){json_view(ui,&self.result);}
    });
        egui::CollapsingHeader::new("Saved draft preview").default_open(true).show(ui,|ui|{
            ui.label(format!("Preview session: {} · pinned revision: {:?}",self.preview_session,self.preview_revision));
            let current=self.preview_revision==self.revision&&self.preview_revision.is_some()&&self.loaded_id==self.agent_id;
            if !current&&!self.preview_session.is_empty(){ui.label("This preview is pinned to an older saved draft. Create a new preview session before another turn.");}
            let prompt_response=ui.add(egui::TextEdit::multiline(&mut self.preview_prompt).desired_rows(3).hint_text("Preview message"));if self.bindings.request_preview{prompt_response.request_focus();prompt_response.scroll_to_me(Some(egui::Align::Center));self.bindings.request_preview=false;}
            if ui.add_enabled(current&&!self.dirty&&!self.preview.active&&!self.preview_prompt.trim().is_empty()&&!self.preview_session.is_empty(),egui::Button::new("Run preview once")).clicked(){
                let body=json!({"message":std::mem::take(&mut self.preview_prompt),"session_id":self.preview_session,"attachments":[],"draft_revision":self.preview_revision});
                self.preview.start(api,ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.agent_id,"preview","chat","stream"],Some(body)),true,runtime,ctx);
            }
            self.preview.ui(ui,api,runtime,ctx);self.job.expired|=self.preview.expired;
        });
        self.release.ui(ui,api,&self.loaded_id,self.revision,self.dirty,runtime,ctx);
        self.job.expired|=self.release.job.expired;
        self.governance.ui(ui,api,&self.loaded_id,runtime,ctx);self.job.expired|=self.governance.job.expired;
        self.versions.ui(ui,api,&self.loaded_id,runtime,ctx);self.job.expired|=self.versions.job.expired;
    });
    }
}
