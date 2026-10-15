//! Instruction-only skill import, metadata editing and policy-preserving enable/revoke.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::{io::Read, path::PathBuf};
#[derive(Default)]
pub struct SkillEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    name: String,
    loaded: String,
    title: String,
    description: String,
    tags: String,
    permissions: String,
    enabled: bool,
    builtin: bool,
    instructions: String,
    test_input: String,
    result: Value,
    file: Option<PathBuf>,
    bytes: Option<Vec<u8>>,
    filename: String,
    upload_confirm: bool,
}
impl SkillEditor {
    fn accept(&mut self, v: &Value) {
        self.loaded = v["name"].as_str().unwrap_or("").into();
        self.name = self.loaded.clone();
        self.title = v["title"].as_str().unwrap_or("").into();
        self.description = v["description"].as_str().unwrap_or("").into();
        self.instructions = v["instructions"].as_str().unwrap_or("").into();
        self.tags = string_list(&v["tags"]);
        self.permissions = string_list(&v["permissions"]);
        self.enabled = v["enabled"].as_bool().unwrap_or(false);
        self.builtin = v["source"] == "builtin";
        self.review.clear();
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                if ["load_skill", "save_skill"].contains(&message.kind.as_str()) {
                    self.accept(&value);
                }
                if message.kind == "upload" {
                    self.name = value["name"].as_str().unwrap_or("").into();
                }
                self.result = value;
            }
        }
        egui::CollapsingHeader::new("Skill authoring and import").default_open(true).show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui|{
            text_field(ui,"Skill name",&mut self.name);ui.horizontal(|ui|{if ui.button("Use selected").clicked(){self.name=selected.into();self.review.clear();}if ui.add_enabled(!self.name.is_empty(),egui::Button::new("Load skill")).clicked(){self.job.request("load_skill",api,ApiRequest::get(&["api","v1","skills",&self.name]),runtime,ctx);}});
            ui.add_enabled_ui(!self.loaded.is_empty()&&self.loaded==self.name,|ui|{text_field(ui,"Title",&mut self.title);text_field(ui,"Description",&mut self.description);ui.label("Tags, one per line");ui.text_edit_multiline(&mut self.tags);ui.label("Requested permissions, one per line");ui.text_edit_multiline(&mut self.permissions);ui.checkbox(&mut self.enabled,"Enabled");ui.label("Instructions (immutable artifact content; import a new artifact to replace)");ui.add(egui::TextEdit::multiline(&mut self.instructions).interactive(false).desired_rows(6));
                ui.add_enabled_ui(!self.builtin,|ui|{if ui.button("Save metadata…").clicked(){self.review.prepare("save_skill","Save this skill's metadata and permissions",ApiRequest::mutation(Method::PATCH,&["api","v1","skills",&self.loaded],Some(json!({"title":self.title,"description":self.description,"tags":lines(&self.tags),"permissions":lines(&self.permissions),"enabled":self.enabled}))));}for(op,label)in[("enable","Enable skill…"),("disable","Disable skill…")]{if ui.button(label).clicked(){self.review.prepare(op,label,ApiRequest::mutation(Method::POST,&["api","v1","skills",&self.loaded,op],None));}}if ui.button("Delete skill…").clicked(){self.review.prepare("delete","Delete this tenant skill artifact",ApiRequest::mutation(Method::DELETE,&["api","v1","skills",&self.loaded],None));}});
                text_field(ui,"Test input",&mut self.test_input);if ui.add_enabled(!self.test_input.trim().is_empty(),egui::Button::new("Review skill test")).clicked(){self.review.prepare("test","Test this instruction skill with the supplied input",ApiRequest::mutation(Method::POST,&["api","v1","skills",&self.loaded,"test"],Some(json!({"input":self.test_input}))));}
            });
            egui::CollapsingHeader::new("Import SKILL.md").show(ui,|ui|{ui.label("Drop the instruction-only SKILL.md to select it. Server policy validates content and reserved names.");for dropped in ctx.input(|i|i.raw.dropped_files.clone()){self.filename=dropped.path.as_ref().and_then(|p|p.file_name()).map(|n|n.to_string_lossy().into_owned()).unwrap_or(dropped.name);self.file=dropped.path;self.bytes=dropped.bytes.map(|b|b.to_vec());self.upload_confirm=false;break;}ui.label(&self.filename);ui.checkbox(&mut self.upload_confirm,"Upload this selected file to the skill catalog");if ui.add_enabled(self.upload_confirm&&!self.filename.is_empty()&&!self.job.outcome_unknown,egui::Button::new("Upload once")).clicked(){let path=self.file.clone();let supplied=self.bytes.take();let filename=self.filename.clone();let api=api.clone();self.upload_confirm=false;self.job.start("upload",async move{if !filename.ends_with(".md"){return Err(ApiError::Request);}let bytes=if let Some(bytes)=supplied{bytes}else{tokio::task::spawn_blocking(move||{let f=std::fs::File::open(path.ok_or(ApiError::Request)?).map_err(|_|ApiError::Request)?;if !f.metadata().map_err(|_|ApiError::Request)?.is_file(){return Err(ApiError::Request);}let mut bytes=Vec::new();f.take(50_001).read_to_end(&mut bytes).map_err(|_|ApiError::Request)?;Ok(bytes)}).await.map_err(|_|ApiError::Request)??};if bytes.len()>50_000{return Err(ApiError::Request);}if std::str::from_utf8(&bytes).is_err(){return Err(ApiError::Request);}api.upload(&["api","v1","skills","upload"],"file",&filename,"text/markdown",bytes).await},runtime,ctx);}});
        });self.review.ui(ui,&mut self.job,api,runtime,ctx);if !self.result.is_null(){json_view(ui,&self.result);}});
    }
}
