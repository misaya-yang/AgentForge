//! Connector settings, OAuth handoff, activation and explicit runtime principals.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct ConnectorEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    catalog: Value,
    result: Value,
    provider: String,
    loaded: String,
    name: String,
    description: String,
    mode: String,
    enabled: bool,
    sync: bool,
    search: bool,
    client_id: String,
    client_secret: String,
    auth_url: String,
    token_url: String,
    scopes: String,
    tools: String,
    oauth_handoff: String,
    principal: String,
    owner: String,
    secret_ref: String,
    audience: String,
    expires: String,
    metadata: String,
    channels: String,
    grant_id: String,
}
impl Default for ConnectorEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            catalog: Value::Null,
            result: Value::Null,
            provider: String::new(),
            loaded: String::new(),
            name: String::new(),
            description: String::new(),
            mode: "live".into(),
            enabled: true,
            sync: false,
            search: true,
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: String::new(),
            token_url: String::new(),
            scopes: String::new(),
            tools: "[]".into(),
            oauth_handoff: String::new(),
            principal: "service_account".into(),
            owner: String::new(),
            secret_ref: String::new(),
            audience: String::new(),
            expires: String::new(),
            metadata: "{}".into(),
            channels: "preview\nhosted_private".into(),
            grant_id: String::new(),
        }
    }
}
impl ConnectorEditor {
    fn accept(&mut self, v: &Value) {
        self.loaded = v["provider"].as_str().unwrap_or("").into();
        self.provider = self.loaded.clone();
        self.name = v["display_name"].as_str().unwrap_or("").into();
        self.description = v["description"].as_str().unwrap_or("").into();
        self.mode = v["mode"].as_str().unwrap_or("live").into();
        self.enabled = v["enabled"].as_bool().unwrap_or(false);
        self.sync = v["supports_sync"].as_bool().unwrap_or(false);
        self.search = v["supports_search"].as_bool().unwrap_or(false);
        for (target, key) in [
            (&mut self.client_id, "client_id"),
            (&mut self.auth_url, "auth_url"),
            (&mut self.token_url, "token_url"),
            (&mut self.scopes, "scopes"),
        ] {
            *target = v["auth"][key].as_str().unwrap_or("").into();
        }
        self.client_secret.clear();
        self.tools =
            serde_json::to_string_pretty(&v.get("mcp_tools").cloned().unwrap_or(json!([])))
                .unwrap_or_else(|_| "[]".into());
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
            if let Ok(mut value) = message.result {
                match message.kind.as_str() {
                    "load_catalog" => self.catalog = value.clone(),
                    "save" | "create" | "toggle" => self.accept(&value),
                    "oauth" => {
                        self.oauth_handoff = value["auth_url"].as_str().unwrap_or("").into();
                        value = Value::Null;
                    }
                    _ => {}
                }
                self.result = value;
            }
        }
        egui::CollapsingHeader::new("Connector catalog and connections").default_open(true).show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui|{
   text_field(ui,"Provider",&mut self.provider);ui.horizontal(|ui|{if ui.button("Use selected").clicked(){self.provider=selected.into();self.review.clear();self.oauth_handoff.clear();}if ui.button("Load admin catalog").clicked(){self.job.request("load_catalog",api,ApiRequest::get(&["api","v1","connectors","admin","configs"]),runtime,ctx);}if ui.button("New definition").clicked(){*self=Self::default();}});
   if let Some(rows)=self.catalog.as_array(){let mut picked=None;for row in rows{if ui.button(format!("Edit {}",row["display_name"].as_str().unwrap_or("Connector"))).clicked(){picked=Some(row.clone());}}if let Some(row)=picked{self.accept(&row);}}
   text_field(ui,"Display name",&mut self.name);text_field(ui,"Description",&mut self.description);select_text(ui,"connector_mode",&mut self.mode,&["live","ingest","both"]);ui.checkbox(&mut self.enabled,"Enabled");ui.checkbox(&mut self.sync,"Supports sync");ui.checkbox(&mut self.search,"Supports search");text_field(ui,"OAuth client ID",&mut self.client_id);ui.label("OAuth client secret (write-only; blank keeps existing)");ui.add(egui::TextEdit::singleline(&mut self.client_secret).password(true));text_field(ui,"Authorization URL",&mut self.auth_url);text_field(ui,"Token URL",&mut self.token_url);text_field(ui,"OAuth scopes",&mut self.scopes);ui.label("MCP tool names/descriptions");ui.add(egui::TextEdit::multiline(&mut self.tools).code_editor().desired_rows(4));
   if ui.add_enabled(!self.provider.trim().is_empty()&&!self.name.trim().is_empty(),egui::Button::new("Save definition…")).clicked(){match json_array(&self.tools){Ok(tools)=>{let mut body=json!({"display_name":self.name,"description":optional_text(&self.description),"mode":self.mode,"enabled":self.enabled,"supports_sync":self.sync,"supports_search":self.search,"auth":{"client_id":self.client_id,"client_secret":optional_text(&self.client_secret),"auth_url":self.auth_url,"token_url":self.token_url,"scopes":self.scopes},"mcp_tools":tools});self.client_secret.clear();if self.loaded.is_empty(){body["provider"]=json!(self.provider);self.review.prepare_sensitive("create","Create connector definition",ApiRequest::mutation(Method::POST,&["api","v1","connectors","admin","configs"],Some(body)),true);}else if self.loaded==self.provider{self.review.prepare_sensitive("save","Update connector definition",ApiRequest::mutation(Method::PUT,&["api","v1","connectors","admin","configs",&self.loaded],Some(body)),true);}else{self.job.status="Load this provider before updating".into();}},Err(_)=>self.job.status="MCP tools must be a JSON array".into()}}
   if !self.loaded.is_empty()&&self.loaded==self.provider{ui.horizontal(|ui|{if ui.button("Apply enabled switch…").clicked(){self.review.prepare("toggle","Change connector availability",ApiRequest::mutation(Method::PATCH,&["api","v1","connectors","admin","configs",&self.loaded,"enabled"],Some(json!({"enabled":self.enabled}))));}if ui.button("Delete definition…").clicked(){self.review.prepare("delete","Delete connector definition; connected users prevent deletion",ApiRequest::mutation(Method::DELETE,&["api","v1","connectors","admin","configs",&self.loaded],None));}});}
   ui.add_enabled_ui(!self.provider.is_empty(),|ui|{egui::CollapsingHeader::new("My connection").show(ui,|ui|{ui.horizontal(|ui|{if ui.button("Check connection").clicked(){self.job.request("load_connection",api,ApiRequest::get(&["api","v1","connectors",&self.provider,"mcp-status"]),runtime,ctx);}if ui.button("Connect via OAuth…").clicked(){self.review.prepare("oauth","Start this provider's OAuth authorization",ApiRequest::get(&["api","v1","connectors","auth",&self.provider]));}if ui.button("Activate MCP tools…").clicked(){self.review.prepare("activate","Expose tools for this connected provider",ApiRequest::mutation(Method::POST,&["api","v1","connectors",&self.provider,"activate"],None));}if ui.button("Disconnect…").clicked(){self.review.prepare("disconnect","Disconnect this provider and remove its tool activation",ApiRequest::mutation(Method::DELETE,&["api","v1","connectors",&self.provider],None));}});});
    egui::CollapsingHeader::new("Runtime principals").show(ui,|ui|{if ui.button("Load principals").clicked(){self.job.request("load_principals",api,ApiRequest::get(&["api","v1","connectors",&self.provider,"principals"]),runtime,ctx);}select_text(ui,"connector_principal",&mut self.principal,&["service_account","user_delegated"]);if self.principal=="user_delegated"{text_field(ui,"Owner user ID",&mut self.owner);}text_field(ui,"Secret Store reference",&mut self.secret_ref);text_field(ui,"Audience",&mut self.audience);text_field(ui,"Expiry (RFC3339)",&mut self.expires);ui.label("Allowed channels, one per line");ui.text_edit_multiline(&mut self.channels);ui.label("Connection metadata: domain/email/site_name/cloud_id only");ui.add(egui::TextEdit::multiline(&mut self.metadata).code_editor().desired_rows(3));
      if ui.add_enabled(!self.secret_ref.is_empty(),egui::Button::new("Register principal…")).clicked(){match json_object(&self.metadata){Ok(metadata)=>{let channels=lines(&self.channels);if self.principal=="user_delegated"&&channels.iter().any(|v|["hosted_public","embed"].contains(&v.as_str())){self.job.status="Delegated principals cannot use public channels".into();}else{self.review.prepare("principal","Register an explicit runtime credential principal",ApiRequest::mutation(Method::POST,&["api","v1","connectors",&self.provider,"principals"],Some(json!({"principal_type":self.principal,"owner_user_id":if self.principal=="user_delegated"{optional_text(&self.owner)}else{Value::Null},"secret_ref":self.secret_ref,"scopes":lines(&self.scopes),"audience":optional_text(&self.audience),"expires_at":optional_text(&self.expires),"connection_metadata":metadata,"allowed_channels":channels}))));}},Err(_)=>self.job.status="Connection metadata must be a JSON object".into()}}
      text_field(ui,"Grant ID",&mut self.grant_id);if ui.add_enabled(!self.grant_id.is_empty(),egui::Button::new("Revoke principal…")).clicked(){self.review.prepare("revoke","Revoke this runtime principal",ApiRequest::mutation(Method::DELETE,&["api","v1","connectors",&self.provider,"principals",&self.grant_id],None));}
    });
   });
  });self.review.ui(ui,&mut self.job,api,runtime,ctx);
  if !self.oauth_handoff.is_empty(){if let Ok(url)=reqwest::Url::parse(&self.oauth_handoff){if url.scheme()=="https"&&url.username().is_empty()&&url.password().is_none(){ui.label("Continue authorization in your browser, then refresh the connection.");if ui.button("Open authorization page").clicked(){ctx.open_url(egui::OpenUrl::new_tab(self.oauth_handoff.clone()));}}else{ui.label("The server returned an unsafe authorization URL.");}}if ui.button("Dismiss authorization link").clicked(){self.oauth_handoff.clear();}}
  if !self.result.is_null(){json_view(ui,&self.result);}
 });
    }
}
