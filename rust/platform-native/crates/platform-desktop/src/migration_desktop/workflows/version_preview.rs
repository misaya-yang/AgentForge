//! Isolated historical-version preview, with session pin and restore identity checks.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
#[derive(Default)]
pub struct VersionPreview {
    pub job: RemoteJob,
    stream: super::stream_panel::StreamPanel,
    bound: String,
    versions: Value,
    target: String,
    pinned: String,
    session: String,
    restore_id: String,
    prompt: String,
    session_info: Value,
    history: Value,
    artifacts: super::attachments::AttachmentWorkspace,
}
impl VersionPreview {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        agent: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                match message.kind.as_str() {
                    "load_versions" => self.versions = value,
                    "create_session" | "restore_session" => {
                        let pin = value.get("pin").unwrap_or(&value);
                        if pin["agent_id"].as_str() != Some(self.bound.as_str())
                            || pin["channel"] != "preview"
                            || pin["agent_version_id"]
                                .as_str()
                                .is_none_or(|s| s.is_empty())
                        {
                            self.job.status = "Version preview pin identity mismatch".into();
                        } else {
                            self.session = pin["session_id"].as_str().unwrap_or("").into();
                            self.pinned = pin["agent_version_id"].as_str().unwrap_or("").into();
                            self.target = self.pinned.clone();
                            self.session_info = pin.clone();
                            self.history = value.get("history").cloned().unwrap_or(Value::Null);
                            self.stream = Default::default();
                        }
                    }
                    "load_history" => self.history = value,
                    _ => {}
                }
            }
        }
        if self.bound != agent {
            if self.job.busy {
                ui.label("A previous preview-session request is pending.");
                return;
            }
            *self = Self::default();
            self.bound = agent.into();
        }
        egui::CollapsingHeader::new("Historical version preview").show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.bound.is_empty()&&!self.job.busy&&!self.stream.active,|ui|{if ui.button("Load immutable versions").clicked(){self.job.request("load_versions",api,ApiRequest::get(&["api","v1","agents",&self.bound,"versions"]),runtime,ctx);}if let Some(versions)=self.versions.as_array(){for v in versions{let id=v["agent_version_id"].as_str().unwrap_or("");ui.horizontal(|ui|{if ui.selectable_label(self.target==id,format!("Version {} · {}",v["version_number"],id)).clicked(){self.target=id.into();}egui::CollapsingHeader::new("Snapshot spec").id_salt(id).show(ui,|ui|json_view(ui,&v["spec"]));});}}text_field(ui,"Version ID",&mut self.target);if ui.add_enabled(!self.target.is_empty()&&!self.job.outcome_unknown,egui::Button::new("Start isolated version preview session")).clicked(){self.job.request("create_session",api,ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.bound,"versions",&self.target,"preview","sessions"],Some(json!({}))),runtime,ctx);}text_field(ui,"Restore preview session ID",&mut self.restore_id);if ui.add_enabled(!self.restore_id.is_empty(),egui::Button::new("Restore version preview and history")).clicked(){let api=api.clone();let agent=self.bound.clone();let session=self.restore_id.clone();self.job.start("restore_session",async move{let pin=api.execute(ApiRequest::get(&["api","v1","agents",&agent,"preview","sessions",&session])).await?;if pin["session_id"]!=session||pin["agent_id"]!=agent||pin["channel"]!="preview"{return Err(ApiError::Response);}let history=api.execute(ApiRequest::get(&["api","v1","sessions",&session,"history"])).await?;Ok(json!({"pin":pin,"history":history}))},runtime,ctx);}});
   ui.label(format!("Preview session {} · pinned version {}",self.session,self.pinned));if !self.session_info.is_null(){egui::CollapsingHeader::new("Effective preview capabilities and memory").show(ui,|ui|json_view(ui,&self.session_info));}ui.add(egui::TextEdit::multiline(&mut self.prompt).desired_rows(3));if ui.add_enabled(!self.session.is_empty()&&self.target==self.pinned&&!self.stream.active&&!self.prompt.trim().is_empty(),egui::Button::new("Send to pinned version once")).clicked(){self.stream.start(api,ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.bound,"versions",&self.pinned,"preview","chat","stream"],Some(json!({"message":std::mem::take(&mut self.prompt),"session_id":self.session,"attachments":[]}))),true,runtime,ctx);}self.stream.ui(ui,api,runtime,ctx);self.job.expired|=self.stream.expired;if ui.add_enabled(!self.session.is_empty()&&!self.job.busy,egui::Button::new("Refresh preview history")).clicked(){self.job.request("load_history",api,ApiRequest::get(&["api","v1","sessions",&self.session,"history"]),runtime,ctx);}if !self.history.is_null(){json_view(ui,&self.history);}self.artifacts.ui_artifacts(ui,api,&self.session,&[],runtime,ctx);self.job.expired|=self.artifacts.expired;
  });
    }
}
