//! Provider form migration: write-only credentials are never loaded or persisted.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct ProviderEditor {
    catalog: super::provider_catalog::ProviderCatalog,
    pub job: RemoteJob,
    pub provider_id: String,
    loaded_id: String,
    pub display_name: String,
    pub api_type: String,
    pub base_url: String,
    pub enabled: bool,
    pub has_api_key: bool,
    api_key: String,
    pub metadata: String,
    pub templates: Value,
    pub template_id: String,
    pub confirm: bool,
    pub result: Value,
}
impl Default for ProviderEditor {
    fn default() -> Self {
        Self {
            catalog: Default::default(),
            job: RemoteJob::default(),
            provider_id: String::new(),
            loaded_id: String::new(),
            display_name: String::new(),
            api_type: "openai".into(),
            base_url: "https://api.openai.com".into(),
            enabled: true,
            has_api_key: false,
            api_key: String::new(),
            metadata: "{}".into(),
            templates: Value::Null,
            template_id: String::new(),
            confirm: false,
            result: Value::Null,
        }
    }
}
impl ProviderEditor {
    fn accept(&mut self, value: &Value) {
        self.catalog = Default::default();
        self.loaded_id = value["provider_id"].as_str().unwrap_or("").into();
        self.provider_id = self.loaded_id.clone();
        self.display_name = value["display_name"].as_str().unwrap_or("").into();
        self.api_type = value["api_type"].as_str().unwrap_or("openai").into();
        self.base_url = value["base_url"].as_str().unwrap_or("").into();
        self.enabled = value["is_enabled"].as_bool().unwrap_or(false);
        self.has_api_key = value["has_api_key"].as_bool().unwrap_or(false);
        self.metadata =
            serde_json::to_string_pretty(&value.get("metadata").cloned().unwrap_or(json!({})))
                .unwrap_or_else(|_| "{}".into());
        self.api_key.clear();
        self.confirm = false;
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
                match message.kind.as_str() {
                    "load_provider" | "save" | "create" => {
                        self.accept(&value);
                        self.result = value;
                    }
                    "load_templates" => self.templates = value,
                    _ => self.result = value,
                }
            }
        }
        egui::CollapsingHeader::new("Provider configuration").default_open(true).show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui|{ui.horizontal(|ui|{text_field(ui,"Provider ID",&mut self.provider_id);if ui.button("Use selected").clicked()&&!selected.is_empty(){self.provider_id=selected.into();}if ui.add_enabled(!self.provider_id.is_empty(),egui::Button::new("Load provider")).clicked(){self.api_key.clear();self.job.request("load_provider",api,ApiRequest::get(&["api","v1","providers",&self.provider_id]),runtime,ctx);}if ui.button("New provider").clicked(){*self=Self::default();}});
        if ui.button("Load guided templates").clicked(){self.job.request("load_templates",api,ApiRequest::get(&["api","v1","provider-templates"]),runtime,ctx);}if let Some(templates)=self.templates.as_array(){let mut picked=None;ui.horizontal_wrapped(|ui|{for template in templates{let id=template["template_id"].as_str().unwrap_or("");if ui.selectable_label(self.template_id==id,template["display_name"].as_str().unwrap_or(id)).clicked(){picked=Some(template.clone());}}});if let Some(template)=picked{self.template_id=template["template_id"].as_str().unwrap_or("").into();self.provider_id=template["default_provider_id"].as_str().unwrap_or("").into();self.display_name=template["display_name"].as_str().unwrap_or("").into();self.api_type=template["api_type"].as_str().unwrap_or("openai").into();self.base_url=template["default_base_url"].as_str().unwrap_or("").into();self.loaded_id.clear();}}
        text_field(ui,"Display name",&mut self.display_name);ui.horizontal(|ui|{ui.label("API type");egui::ComboBox::from_id_salt("provider_api_type").selected_text(&self.api_type).show_ui(ui,|ui|{for kind in ["openai","dashscope","anthropic","google","google-vertex"]{ui.selectable_value(&mut self.api_type,kind.into(),kind);}});});text_field(ui,"Base URL",&mut self.base_url);ui.checkbox(&mut self.enabled,"Enabled");ui.label(if self.has_api_key{"API key is configured. Leave blank to keep it unchanged."}else{"API key (write-only)"});ui.add(egui::TextEdit::singleline(&mut self.api_key).password(true));ui.label("Provider metadata");ui.add(egui::TextEdit::multiline(&mut self.metadata).code_editor().desired_rows(5));ui.checkbox(&mut self.confirm,"I reviewed the provider and exact settings");let ready=self.confirm&&(self.loaded_id.is_empty()||self.provider_id==self.loaded_id)&&!self.job.outcome_unknown&&!self.provider_id.trim().is_empty()&&!self.display_name.trim().is_empty();
        ui.horizontal(|ui|{if ui.add_enabled(ready,egui::Button::new(if self.loaded_id.is_empty(){"Create provider"}else{"Save provider"})).clicked(){match json_object(&self.metadata){Ok(metadata)=>{let creating=self.loaded_id.is_empty();let mut body=json!({"display_name":self.display_name,"base_url":self.base_url,"metadata":metadata,"is_enabled":self.enabled});if !self.api_key.is_empty(){body["api_key"]=json!(std::mem::take(&mut self.api_key));}let request=if creating&&!self.template_id.is_empty(){body["template_id"]=json!(self.template_id);body["provider_id"]=json!(self.provider_id);ApiRequest::mutation(Method::POST,&["api","v1","providers","from-template"],Some(body))}else if creating{body["provider_id"]=json!(self.provider_id);body["api_type"]=json!(self.api_type);ApiRequest::mutation(Method::POST,&["api","v1","providers"],Some(body))}else{body["api_type"]=json!(self.api_type);ApiRequest::mutation(Method::PUT,&["api","v1","providers",&self.loaded_id],Some(body))};self.job.request(if creating{"create"}else{"save"},api,request,runtime,ctx);self.confirm=false;},Err(_)=>self.job.status="Provider metadata must be a JSON object".into()}}
            if ui.add_enabled(ready&&!self.loaded_id.is_empty(),egui::Button::new("Test saved connection")).clicked(){self.job.request("test",api,ApiRequest::mutation(Method::POST,&["api","v1","providers",&self.loaded_id,"test"],None),runtime,ctx);self.confirm=false;}
            if ui.add_enabled(ready&&!self.loaded_id.is_empty(),egui::Button::new("Preview model catalog")).clicked(){self.catalog.start(&self.loaded_id,api,runtime,ctx);self.confirm=false;}
            if ui.add_enabled(ready&&!self.loaded_id.is_empty(),egui::Button::new("Delete provider")).clicked(){self.job.request("delete",api,ApiRequest::mutation(Method::DELETE,&["api","v1","providers",&self.loaded_id],None),runtime,ctx);self.confirm=false;}
        });self.catalog.ui(ui,&self.loaded_id,api,runtime,ctx);self.catalog.propagate_expiry(&mut self.job);if !self.result.is_null(){json_view(ui,&self.result);}
    });});
    }
}
