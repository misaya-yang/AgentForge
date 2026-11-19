//! Agent release workflow from web/src/api/agents.ts. Server evaluations remain
//! authoritative; publication always carries its selected evaluation and idempotency key.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct ReleaseEditor {
    pub job: RemoteJob,
    pub evaluation_id: String,
    pub dataset_id: String,
    pub channel: String,
    pub auth_mode: String,
    pub attachments: bool,
    pub high_risk_tools: bool,
    pub allowed_origins: String,
    pub reason: String,
    pub idempotency_key: String,
    pub publication_id: String,
    pub target_version: String,
    pub confirm: bool,
    pub result: Value,
}
impl Default for ReleaseEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            evaluation_id: String::new(),
            dataset_id: String::new(),
            channel: "hosted".into(),
            auth_mode: "private".into(),
            attachments: false,
            high_risk_tools: false,
            allowed_origins: String::new(),
            reason: String::new(),
            idempotency_key: String::new(),
            publication_id: String::new(),
            target_version: String::new(),
            confirm: false,
            result: Value::Null,
        }
    }
}
impl ReleaseEditor {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        agent: &str,
        revision: Option<u64>,
        dirty: bool,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                if message.kind == "evaluate" {
                    if let Some(id) = value["evaluation_id"].as_str() {
                        self.evaluation_id = id.into();
                    }
                }
                self.result = value;
                self.confirm = false;
            }
        }
        egui::CollapsingHeader::new("Release evaluation and publication").show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!agent.is_empty()&&!self.job.busy,|ui|{if dirty{ui.label("Save the draft before release evaluation.");}text_field(ui,"Evaluation dataset ID (optional)",&mut self.dataset_id);ui.horizontal(|ui|{ui.label("Channel");for channel in ["hosted","embed","api"]{ui.selectable_value(&mut self.channel,channel.into(),channel);}});ui.horizontal(|ui|{ui.label("Audience");for mode in ["private","tenant","public","token"]{ui.selectable_value(&mut self.auth_mode,mode.into(),mode);}});ui.checkbox(&mut self.attachments,"Allow attachments");ui.checkbox(&mut self.high_risk_tools,"Allow high-risk tools under approval policy");ui.label("Allowed embed origins, one per line");ui.text_edit_multiline(&mut self.allowed_origins);ui.checkbox(&mut self.confirm,"I reviewed this release action, audience and policy");let ready=self.confirm&&!self.job.outcome_unknown;
        ui.horizontal_wrapped(|ui|{if ui.add_enabled(ready&&!dirty&&revision.is_some(),egui::Button::new("Create release evaluation")).clicked(){let body=json!({"draft_revision":revision,"dataset_id":if self.dataset_id.is_empty(){Value::Null}else{json!(self.dataset_id)},"channel":self.channel,"auth_mode":self.auth_mode,"channel_policy":{"attachments":self.attachments,"high_risk_tools":self.high_risk_tools,"allowed_origins":lines(&self.allowed_origins)}});self.job.request("evaluate",api,ApiRequest::mutation(Method::POST,&["api","v1","agents",agent,"evals"],Some(body)),runtime,ctx);self.confirm=false;}if ui.button("Load evaluations").clicked(){self.job.request("load_evaluations",api,ApiRequest::get(&["api","v1","agents",agent,"evals"]),runtime,ctx);}if ui.button("Load publications").clicked(){self.job.request("load_publications",api,ApiRequest::get(&["api","v1","agents",agent,"publications"]),runtime,ctx);}});
        text_field(ui,"Evaluation ID",&mut self.evaluation_id);ui.horizontal_wrapped(|ui|{if ui.add_enabled(!self.evaluation_id.is_empty(),egui::Button::new("Read release diff")).clicked(){self.job.request("load_diff",api,ApiRequest::get(&["api","v1","agents",agent,"evals",&self.evaluation_id,"diff"]),runtime,ctx);}for(op,label)in[("execute","Execute evaluation"),("cancel","Cancel evaluation")]{if ui.add_enabled(ready&&!self.evaluation_id.is_empty(),egui::Button::new(label)).clicked(){self.job.request(op,api,ApiRequest::mutation(Method::POST,&["api","v1","agents",agent,"evals",&self.evaluation_id,op],Some(json!({}))),runtime,ctx);self.confirm=false;}}});
        text_field(ui,"Publication reason",&mut self.reason);text_field(ui,"Idempotency key (keep unchanged when reconciling this action)",&mut self.idempotency_key);if ui.add_enabled(ready&&!self.evaluation_id.is_empty()&&!self.reason.trim().is_empty()&&!self.idempotency_key.trim().is_empty(),egui::Button::new("Publish evaluated version")).clicked(){self.job.request("publish",api,ApiRequest::mutation(Method::POST,&["api","v1","agents",agent,"publish"],Some(json!({"evaluation_id":self.evaluation_id,"reason":self.reason}))).header("Idempotency-Key",&self.idempotency_key),runtime,ctx);self.confirm=false;}
        egui::CollapsingHeader::new("Publication rollback").show(ui,|ui|{text_field(ui,"Publication ID",&mut self.publication_id);text_field(ui,"Target version ID",&mut self.target_version);if ui.add_enabled(ready&&!self.publication_id.is_empty()&&!self.target_version.is_empty()&&!self.reason.is_empty()&&!self.idempotency_key.is_empty(),egui::Button::new("Roll back publication")).clicked(){self.job.request("rollback",api,ApiRequest::mutation(Method::POST,&["api","v1","publications",&self.publication_id,"rollback"],Some(json!({"target_version_id":self.target_version,"reason":self.reason}))).header("Idempotency-Key",&self.idempotency_key),runtime,ctx);self.confirm=false;}});
        if !self.result.is_null(){json_view(ui,&self.result);}
    });});
    }
}
