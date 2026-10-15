//! Agent policy, audit, deletion requests and publication-token lifecycle.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
const POLICY_FIELDS: &[&str] = &[
    "trace_retention_days",
    "runtime_retention_days",
    "attachment_retention_days",
    "principal_requests_per_minute",
    "principal_requests_per_day",
    "ip_requests_per_minute",
    "ip_requests_per_day",
    "publication_requests_per_minute",
    "publication_requests_per_day",
    "max_agents_per_tenant",
    "max_active_publications",
    "max_concurrent_runs",
    "max_daily_tokens",
    "max_daily_mcp_calls",
    "max_storage_bytes",
    "alert_threshold_percent",
];
pub struct GovernanceEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    bound: String,
    policy: Value,
    result: Value,
    scope: String,
    subject: String,
    idempotency: String,
    deletion_ack: bool,
    publication: String,
    token_id: String,
    token_name: String,
    token_expiry: String,
    tokens: Value,
    issued: IssuedSecret,
    version: String,
    channel: String,
    status: String,
    started_after: String,
    started_before: String,
    action: String,
    offset: u64,
    limit: u64,
}
impl Default for GovernanceEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            bound: String::new(),
            policy: Value::Null,
            result: Value::Null,
            scope: "retention".into(),
            subject: String::new(),
            idempotency: String::new(),
            deletion_ack: false,
            publication: String::new(),
            token_id: String::new(),
            token_name: String::new(),
            token_expiry: String::new(),
            tokens: Value::Null,
            issued: IssuedSecret::default(),
            version: String::new(),
            channel: String::new(),
            status: String::new(),
            started_after: String::new(),
            started_before: String::new(),
            action: String::new(),
            offset: 0,
            limit: 50,
        }
    }
}
impl GovernanceEditor {
    fn filter(&self, view: &str) -> ApiRequest {
        let mut r = ApiRequest::get(&["api", "v1", "agents", &self.bound, view])
            .query("offset", self.offset)
            .query("limit", self.limit);
        for (k, v) in [
            ("version_id", &self.version),
            ("publication_id", &self.publication),
            ("channel", &self.channel),
            ("status", &self.status),
            ("started_after", &self.started_after),
            ("started_before", &self.started_before),
        ] {
            if !v.is_empty() {
                r = r.query(k, v);
            }
        }
        if view == "audit-events" && !self.action.is_empty() {
            r = r.query("action", &self.action);
        }
        r
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        agent: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if let Some(message) = self.job.poll() {
            if let Ok(mut value) = message.result {
                match message.kind.as_str() {
                    "load_policy" | "save_policy" => self.policy = value.clone(),
                    "load_tokens" => self.tokens = value.clone(),
                    "issue_token" | "rotate_token" => {
                        self.issued
                            .set(value["token"].as_str().unwrap_or("").into());
                        if let Some(obj) = value.as_object_mut() {
                            obj.remove("token");
                        }
                        self.tokens = Value::Null;
                    }
                    _ => {}
                }
                self.result = value;
            }
        }
        if self.bound != agent {
            if self.job.busy {
                ui.label("A previous agent operation is pending; wait before changing governance target.");
                return;
            }
            let expired = self.job.expired;
            *self = Self::default();
            self.bound = agent.into();
            self.job.expired = expired;
        }
        egui::CollapsingHeader::new("Governance, audit and credentials").show(ui,|ui|{ui.label(format!("Agent: {}",self.bound));self.job.status_ui(ui);ui.add_enabled_ui(!self.bound.is_empty()&&!self.job.busy,|ui|{
   if ui.button("Load governance policy").clicked(){self.review.clear();self.job.request("load_policy",api,ApiRequest::get(&["api","v1","agents",&self.bound,"governance"]),runtime,ctx);}
   if self.policy.is_object(){for key in POLICY_FIELDS{let mut value=self.policy[*key].as_u64().unwrap_or(1);let max=if key.ends_with("retention_days"){3650}else if *key=="alert_threshold_percent"{100}else{u64::MAX};if ui.add(egui::DragValue::new(&mut value).range(1..=max).prefix(format!("{key}: "))).changed(){self.policy[*key]=json!(value);}}let mut hold=self.policy["legal_hold"].as_bool().unwrap_or(false);if ui.checkbox(&mut hold,"Legal hold").changed(){self.policy["legal_hold"]=json!(hold);}if ui.button("Save governance policy…").clicked(){let mut body=json!({"legal_hold":hold});for key in POLICY_FIELDS{body[*key]=self.policy[*key].clone();}self.review.prepare("save_policy","Update retention, legal hold, limits and budgets",ApiRequest::mutation(Method::PUT,&["api","v1","agents",&self.bound,"governance"],Some(body)));}
    ui.horizontal(|ui|{if ui.button("Invalidate cache…").clicked(){self.review.prepare("invalidate_cache","Invalidate agent caches",ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.bound,"governance","cache:invalidate"],Some(json!({}))));}if ui.button("Revoke all credentials…").clicked(){self.review.prepare("revoke_credentials","Revoke all credentials for this agent; existing clients lose access",ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.bound,"governance","credentials:revoke"],Some(json!({}))));}});
    egui::CollapsingHeader::new("Data deletion request").show(ui,|ui|{select_text(ui,"deletion_scope",&mut self.scope,&["retention","user","tenant"]);if self.scope=="user"{text_field(ui,"Subject user ID",&mut self.subject);}text_field(ui,"Idempotency key (keep to reconcile this exact request)",&mut self.idempotency);ui.checkbox(&mut self.deletion_ack,"I understand this requests permanent data deletion");if hold{ui.label("Deletion is blocked while legal hold is active.");}if ui.add_enabled(!hold&&self.deletion_ack&&!self.idempotency.trim().is_empty()&&(self.scope!="user"||!self.subject.trim().is_empty()),egui::Button::new("Review deletion request")).clicked(){let mut body=json!({"scope":self.scope,"idempotency_key":self.idempotency});if self.scope=="user"{body["subject_user_id"]=json!(self.subject.trim());}self.review.prepare("delete_data","Permanently delete agent data for this exact scope",ApiRequest::mutation(Method::POST,&["api","v1","agents",&self.bound,"governance","data-deletions"],Some(body)));self.deletion_ack=false;}});
   }
   egui::CollapsingHeader::new("Analytics and audit filters").show(ui,|ui|{text_field(ui,"Version ID",&mut self.version);text_field(ui,"Publication ID",&mut self.publication);text_field(ui,"Channel",&mut self.channel);text_field(ui,"Status",&mut self.status);text_field(ui,"Started after",&mut self.started_after);text_field(ui,"Started before",&mut self.started_before);text_field(ui,"Audit action",&mut self.action);ui.add(egui::DragValue::new(&mut self.offset).prefix("Offset "));ui.add(egui::DragValue::new(&mut self.limit).range(1..=200).prefix("Limit "));ui.horizontal(|ui|{for view in ["analytics","audit-events"]{if ui.button(format!("Load {view}")).clicked(){self.job.request(&format!("load_{view}"),api,self.filter(view),runtime,ctx);}}});});
   egui::CollapsingHeader::new("Publication API tokens").show(ui,|ui|{text_field(ui,"Publication ID",&mut self.publication);if ui.add_enabled(!self.publication.is_empty(),egui::Button::new("Load publication tokens")).clicked(){self.review.clear();self.job.request("load_tokens",api,ApiRequest::get(&["api","v1","publications",&self.publication,"tokens"]),runtime,ctx);}text_field(ui,"New token name",&mut self.token_name);text_field(ui,"Expiry (optional RFC3339)",&mut self.token_expiry);ui.label("Scopes: chat:write, sessions:write, attachments:write, feedback:write");if ui.add_enabled(!self.publication.is_empty()&&!self.token_name.trim().is_empty(),egui::Button::new("Create publication token…")).clicked(){self.issued.clear();let mut body=json!({"name":self.token_name.trim(),"scopes":["chat:write","sessions:write","attachments:write","feedback:write"]});if !self.token_expiry.is_empty(){body["expires_at"]=json!(self.token_expiry);}self.review.prepare("issue_token","Issue a publication token with these scopes",ApiRequest::mutation(Method::POST,&["api","v1","publications",&self.publication,"tokens"],Some(body)));}
    if let Some(tokens)=self.tokens["tokens"].as_array(){for token in tokens{let Some(id)=token["token_id"].as_str()else{continue;};ui.horizontal(|ui|{if ui.selectable_label(self.token_id==id,token["name"].as_str().unwrap_or(id)).clicked(){self.token_id=id.into();self.review.clear();}ui.label(if token["revoked_at"].is_null(){"Active"}else{"Revoked"});});}}text_field(ui,"Token ID",&mut self.token_id);if !self.publication.is_empty()&&!self.token_id.is_empty(){ui.horizontal(|ui|{if ui.button("Rotate token…").clicked(){self.issued.clear();self.review.prepare("rotate_token","Rotate this publication token; replace it in clients",ApiRequest::mutation(Method::POST,&["api","v1","publications",&self.publication,"tokens",&self.token_id,"rotate"],Some(json!({}))));}if ui.button("Revoke token…").clicked(){self.review.prepare("revoke_token","Revoke this publication token immediately",ApiRequest::mutation(Method::DELETE,&["api","v1","publications",&self.publication,"tokens",&self.token_id],None));}});}
   });
  });self.review.ui(ui,&mut self.job,api,runtime,ctx);self.issued.ui(ui);if !self.result.is_null(){json_view(ui,&self.result);}});
    }
}
