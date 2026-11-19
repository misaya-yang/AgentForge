//! Native tenant MCP registry editor, using src/api/schemas/mcp.py's closed shapes.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct McpEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    server_id: String,
    loaded_id: String,
    name: String,
    description: String,
    base_url: String,
    auth_method: String,
    origins: String,
    oauth_metadata_url: String,
    oauth_resource: String,
    oauth_audience: String,
    timeout_ms: u32,
    concurrency: u32,
    response_limit: u32,
    enabled: bool,
    principal: String,
    owner: String,
    secret_ref: String,
    scopes: String,
    audience: String,
    expires: String,
    connection_id: String,
    tool_id: String,
    channel: String,
    result: Value,
}
impl Default for McpEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            server_id: String::new(),
            loaded_id: String::new(),
            name: String::new(),
            description: String::new(),
            base_url: String::new(),
            auth_method: "none".into(),
            origins: String::new(),
            oauth_metadata_url: String::new(),
            oauth_resource: String::new(),
            oauth_audience: String::new(),
            timeout_ms: 30000,
            concurrency: 5,
            response_limit: 1048576,
            enabled: true,
            principal: "service_account".into(),
            owner: String::new(),
            secret_ref: String::new(),
            scopes: String::new(),
            audience: String::new(),
            expires: String::new(),
            connection_id: String::new(),
            tool_id: String::new(),
            channel: "preview".into(),
            result: Value::Null,
        }
    }
}
impl McpEditor {
    fn accept(&mut self, value: &Value) {
        let v = value.get("server").unwrap_or(value);
        self.loaded_id = v["server_id"].as_str().unwrap_or("").into();
        self.server_id = self.loaded_id.clone();
        for (target, key) in [
            (&mut self.name, "name"),
            (&mut self.description, "description"),
            (&mut self.base_url, "base_url"),
            (&mut self.auth_method, "auth_method"),
            (&mut self.oauth_metadata_url, "oauth_metadata_url"),
            (&mut self.oauth_resource, "oauth_resource"),
            (&mut self.oauth_audience, "oauth_audience"),
        ] {
            *target = v[key].as_str().unwrap_or("").into();
        }
        self.origins = string_list(&v["allowed_origins"]);
        self.timeout_ms = v["timeout_ms"].as_u64().unwrap_or(30000) as u32;
        self.concurrency = v["max_concurrency"].as_u64().unwrap_or(5) as u32;
        self.response_limit = v["response_limit_bytes"].as_u64().unwrap_or(1048576) as u32;
        self.enabled = v["enabled"].as_bool().unwrap_or(false);
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
                if ["load_server", "save_server", "create_server"].contains(&message.kind.as_str())
                {
                    self.accept(&value);
                }
                if message.kind == "create_connection" {
                    self.connection_id = value["connection"]["connection_id"]
                        .as_str()
                        .unwrap_or("")
                        .into();
                }
                self.result = value;
            }
        }
        egui::CollapsingHeader::new("MCP server and principal editor").default_open(true).show(ui,|ui| {
            self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui| {
                text_field(ui,"Server ID",&mut self.server_id);ui.horizontal(|ui|{if ui.button("Use selected").clicked(){self.server_id=selected.into();self.review.clear();}if ui.add_enabled(!self.server_id.is_empty(),egui::Button::new("Load server")).clicked(){self.review.clear();self.job.request("load_server",api,ApiRequest::get(&["api","v1","mcp","servers",&self.server_id]),runtime,ctx);}if ui.button("New server").clicked(){*self=Self::default();}});
                text_field(ui,"Name",&mut self.name);text_field(ui,"Description",&mut self.description);text_field(ui,"HTTPS endpoint",&mut self.base_url);select_text(ui,"mcp_auth",&mut self.auth_method,&["none","bearer","oauth"]);ui.label("Allowed HTTPS origins, one per line");ui.text_edit_multiline(&mut self.origins);
                if self.auth_method=="oauth"{text_field(ui,"OAuth metadata URL",&mut self.oauth_metadata_url);text_field(ui,"OAuth resource",&mut self.oauth_resource);text_field(ui,"OAuth audience",&mut self.oauth_audience);}
                ui.add(egui::DragValue::new(&mut self.timeout_ms).range(100..=120000).prefix("Timeout ms "));ui.add(egui::DragValue::new(&mut self.concurrency).range(1..=32).prefix("Concurrency "));ui.add(egui::DragValue::new(&mut self.response_limit).range(1024..=8388608).prefix("Response bytes "));ui.checkbox(&mut self.enabled,"Enabled (saved servers)");
                if ui.add_enabled(!self.name.trim().is_empty()&&!self.base_url.is_empty()&&!lines(&self.origins).is_empty(),egui::Button::new("Review server save")).clicked(){let mut body=json!({"name":self.name,"description":self.description,"base_url":self.base_url,"auth_method":self.auth_method,"allowed_origins":lines(&self.origins),"timeout_ms":self.timeout_ms,"max_concurrency":self.concurrency,"response_limit_bytes":self.response_limit,"oauth_metadata_url":if self.auth_method=="oauth"{optional_text(&self.oauth_metadata_url)}else{Value::Null},"oauth_resource":if self.auth_method=="oauth"{optional_text(&self.oauth_resource)}else{Value::Null},"oauth_audience":if self.auth_method=="oauth"{optional_text(&self.oauth_audience)}else{Value::Null}});if self.loaded_id.is_empty(){body["transport"]=json!("streamable_http");self.review.prepare("create_server","Create tenant MCP server",ApiRequest::mutation(Method::POST,&["api","v1","mcp","servers"],Some(body)));}else if self.loaded_id==self.server_id{body["enabled"]=json!(self.enabled);self.review.prepare("save_server","Update this MCP server",ApiRequest::mutation(Method::PATCH,&["api","v1","mcp","servers",&self.loaded_id],Some(body)));}else{self.job.status="Load the selected server before saving".into();}}
                ui.add_enabled_ui(!self.loaded_id.is_empty()&&self.loaded_id==self.server_id,|ui| {
                    ui.horizontal(|ui|{for view in ["tools","connections"]{if ui.button(format!("Load {view}")).clicked(){self.job.request(&format!("load_{view}"),api,ApiRequest::get(&["api","v1","mcp","servers",&self.loaded_id,view]),runtime,ctx);}}if ui.button("Delete server…").clicked(){self.review.prepare("delete_server","Delete this server and invalidate its bindings",ApiRequest::mutation(Method::DELETE,&["api","v1","mcp","servers",&self.loaded_id],None));}});
                    egui::CollapsingHeader::new("Connection and discovery").show(ui,|ui|{select_text(ui,"mcp_principal",&mut self.principal,&["service_account","user_delegated"]);if self.principal=="user_delegated"{text_field(ui,"Owner user ID",&mut self.owner);}text_field(ui,"Secret Store reference (not a credential)",&mut self.secret_ref);text_field(ui,"Audience",&mut self.audience);text_field(ui,"Expires at (RFC3339, optional)",&mut self.expires);ui.label("Scopes, one per line");ui.text_edit_multiline(&mut self.scopes);
                        if ui.button("Create connection…").clicked(){self.review.prepare("create_connection","Create a scoped MCP principal",ApiRequest::mutation(Method::POST,&["api","v1","mcp","servers",&self.loaded_id,"connections"],Some(json!({"principal_type":self.principal,"owner_user_id":if self.principal=="user_delegated"{optional_text(&self.owner)}else{Value::Null},"secret_ref":optional_text(&self.secret_ref),"scopes":lines(&self.scopes),"audience":optional_text(&self.audience),"expires_at":optional_text(&self.expires)}))));}
                        text_field(ui,"Connection ID",&mut self.connection_id);ui.add_enabled_ui(!self.connection_id.is_empty(),|ui|{if ui.button("Discover tools…").clicked(){self.review.prepare("discover","Discover schemas using the chosen principal",ApiRequest::mutation(Method::POST,&["api","v1","mcp","servers",&self.loaded_id,"discover"],Some(json!({"connection_id":self.connection_id,"principal_type":self.principal}))));}if ui.button("Revoke connection…").clicked(){self.review.prepare("revoke","Revoke this MCP connection",ApiRequest::mutation(Method::DELETE,&["api","v1","mcp","connections",&self.connection_id],None));}text_field(ui,"Tool ID",&mut self.tool_id);select_text(ui,"mcp_channel",&mut self.channel,&["preview","hosted_private","hosted_public","embed","api"]);if ui.add_enabled(!self.tool_id.is_empty(),egui::Button::new("Grant read-only channel…")).clicked(){self.review.prepare("grant","Grant this tool on the selected channel (read-only only)",ApiRequest::mutation(Method::PUT,&["api","v1","mcp","connections",&self.connection_id,"channel-grants"],Some(json!({"tool_id":self.tool_id,"channel":self.channel,"read_only_only":true}))));}});
                    });
                });
            });self.review.ui(ui,&mut self.job,api,runtime,ctx);if !self.result.is_null(){json_view(ui,&self.result);}
        });
    }
}
