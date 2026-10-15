//! Provider-disambiguated model CRUD and revision-fenced capability editing.
use super::super::api::{ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
pub struct ModelEditor {
    pub job: RemoteJob,
    review: ReviewedAction,
    rows: Value,
    result: Value,
    adapters: Value,
    templates: Value,
    provider: String,
    loaded_provider: String,
    id: String,
    loaded_id: String,
    name: String,
    context: u64,
    output: u64,
    vision: bool,
    tools: bool,
    input_price: f64,
    output_price: f64,
    access: String,
    enabled: bool,
    order: i64,
    revision: u64,
    capabilities: String,
    baseline: Value,
}
impl Default for ModelEditor {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            rows: Value::Null,
            result: Value::Null,
            adapters: Value::Null,
            templates: Value::Null,
            provider: String::new(),
            loaded_provider: String::new(),
            id: String::new(),
            loaded_id: String::new(),
            name: String::new(),
            context: 128000,
            output: 4096,
            vision: false,
            tools: true,
            input_price: 0.0,
            output_price: 0.0,
            access: "public".into(),
            enabled: true,
            order: 0,
            revision: 0,
            capabilities: serde_json::to_string_pretty(&super::model_capabilities::safe_profile(
                true, false,
            ))
            .unwrap_or_default(),
            baseline: super::model_capabilities::safe_profile(true, false),
        }
    }
}
fn numeric(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}
impl ModelEditor {
    fn accept(&mut self, v: &Value) {
        self.loaded_id = v["model_id"].as_str().unwrap_or("").into();
        self.loaded_provider = v["provider_id"].as_str().unwrap_or("").into();
        self.id = self.loaded_id.clone();
        self.provider = self.loaded_provider.clone();
        self.name = v["display_name"].as_str().unwrap_or("").into();
        self.context = v["context_window"].as_u64().unwrap_or(128000);
        self.output = v["max_output_tokens"].as_u64().unwrap_or(4096);
        self.vision = v["supports_vision"].as_bool().unwrap_or(false);
        self.tools = v["supports_tools"].as_bool().unwrap_or(true);
        self.enabled = v["is_enabled"].as_bool().unwrap_or(false);
        self.access = v["access_level"].as_str().unwrap_or("public").into();
        self.order = v["sort_order"].as_i64().unwrap_or(0);
        self.input_price = numeric(&v["input_price_per_1k"]);
        self.output_price = numeric(&v["output_price_per_1k"]);
        self.revision = v["capability_revision"].as_u64().unwrap_or(0);
        self.baseline = v
            .get("effective_capabilities")
            .filter(|v| super::model_capabilities::usable(v))
            .cloned()
            .unwrap_or_else(|| super::model_capabilities::safe_profile(self.tools, self.vision));
        self.capabilities =
            serde_json::to_string_pretty(&self.baseline).unwrap_or_else(|_| "{}".into());
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
                match message.kind.as_str() {
                    "load_models" => self.rows = value.clone(),
                    "load_adapters" => self.adapters = value.clone(),
                    "load_templates" => self.templates = value.clone(),
                    "save" | "create" | "toggle" | "reset" => self.accept(&value),
                    _ => {}
                }
                self.result = value;
            }
        }
        egui::CollapsingHeader::new("Model configuration").default_open(true).show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui|{
   text_field(ui,"Provider ID",&mut self.provider);ui.horizontal(|ui|{if ui.button("Load models including disabled").clicked(){let mut r=ApiRequest::get(&["api","v1","models"]).query("include_disabled",true);if !self.provider.is_empty(){r=r.query("provider_id",&self.provider);}self.job.request("load_models",api,r,runtime,ctx);}if ui.button("New model").clicked(){*self=Self::default();}if ui.button("Use selected ID").clicked(){self.id=selected.into();self.review.clear();}});
   if let Some(rows)=self.rows.as_array(){let mut pick=None;egui::ScrollArea::vertical().id_salt("model-editor-list").max_height(140.0).show(ui,|ui|{for row in rows{if ui.button(format!("{} / {}",row["provider_id"].as_str().unwrap_or(""),row["model_id"].as_str().unwrap_or(""))).clicked(){pick=Some(row.clone());}}});if let Some(row)=pick{self.accept(&row);}}
   ui.horizontal(|ui|{if ui.button("Load provider model templates").clicked(){self.job.request("load_templates",api,ApiRequest::get(&["api","v1","provider-templates"]),runtime,ctx);}if ui.button("Load capability adapters").clicked(){self.job.request("load_adapters",api,ApiRequest::get(&["api","v1","model-capability-adapters"]),runtime,ctx);}});
   if self.loaded_id.is_empty(){if let Some(templates)=self.templates.as_array(){let mut pick=None;for template in templates{egui::CollapsingHeader::new(template["display_name"].as_str().unwrap_or("Provider template")).show(ui,|ui|{if let Some(models)=template["default_models"].as_array(){for model in models{if ui.button(model["display_name"].as_str().unwrap_or("Model")).clicked(){let mut model=model.clone();model["provider_id"]=json!(self.provider);pick=Some(model);}}}});}if let Some(model)=pick{self.accept(&model);self.loaded_id.clear();self.loaded_provider.clear();}}}
   text_field(ui,"Model ID (may rename)",&mut self.id);text_field(ui,"Display name",&mut self.name);ui.add(egui::DragValue::new(&mut self.context).range(1..=100000000).prefix("Context tokens "));ui.add(egui::DragValue::new(&mut self.output).range(1..=100000000).prefix("Output tokens "));ui.checkbox(&mut self.vision,"Supports vision");ui.checkbox(&mut self.tools,"Supports tools");ui.checkbox(&mut self.enabled,"Enabled");ui.add(egui::DragValue::new(&mut self.input_price).range(0.0..=1000000.0).speed(0.001).prefix("Input $ / 1K "));ui.add(egui::DragValue::new(&mut self.output_price).range(0.0..=1000000.0).speed(0.001).prefix("Output $ / 1K "));ui.add(egui::DragValue::new(&mut self.order).prefix("Sort order "));select_text(ui,"model_access",&mut self.access,&["public","premium","admin"]);
   egui::CollapsingHeader::new("Reasoning, cache, search and tool capabilities").show(ui,|ui|{ui.label(format!("Loaded capability revision: {}. An unchanged profile is omitted from saves.",self.revision));if let Ok(mut profile)=json_object(&self.capabilities){if super::model_capabilities::ui(ui,&mut profile,&self.adapters){self.capabilities=serde_json::to_string_pretty(&profile).unwrap_or_default();}}if ui.button("Initialize safe capability profile").clicked(){self.capabilities=serde_json::to_string_pretty(&super::model_capabilities::safe_profile(self.tools,self.vision)).unwrap_or_default();}ui.label("Advanced complete capability profile");ui.add(egui::TextEdit::multiline(&mut self.capabilities).code_editor().desired_rows(12));if !self.adapters.is_null(){json_view(ui,&self.adapters);}if ui.button("Restore local loaded capability draft").clicked(){self.capabilities=serde_json::to_string_pretty(&self.baseline).unwrap_or_default();}});
   if ui.add_enabled(!self.id.trim().is_empty()&&!self.name.trim().is_empty()&&!self.provider.is_empty(),egui::Button::new("Review model save")).clicked(){match json_object(&self.capabilities){Ok(caps)if super::model_capabilities::validate_options(&caps)=>{let mut body=json!({"model_id":self.id,"display_name":self.name,"context_window":self.context,"max_output_tokens":self.output,"supports_vision":self.vision,"supports_tools":self.tools,"input_price_per_1k":self.input_price,"output_price_per_1k":self.output_price,"access_level":self.access,"is_enabled":self.enabled,"sort_order":self.order});if caps!=self.baseline{body["capability_overrides"]=caps;}if self.loaded_id.is_empty(){body["provider_id"]=json!(self.provider);self.review.prepare("create","Create model under this provider",ApiRequest::mutation(Method::POST,&["api","v1","models"],Some(body)));}else if self.provider==self.loaded_provider{body["expected_capability_revision"]=json!(self.revision);self.review.prepare("save","Save model, including any model ID rename",ApiRequest::mutation(Method::PUT,&["api","v1","models",&self.loaded_id],Some(body)).query("provider_id",&self.loaded_provider));}else{self.job.status="Provider cannot change in edit mode. Create another model instead.".into();}},_=>self.job.status="Capabilities must be an object with unique valid reasoning option IDs".into()}}
   if !self.loaded_id.is_empty(){ui.horizontal(|ui|{if ui.button("Reset to provider capabilities…").clicked(){self.review.prepare("reset","Clear tenant capability overrides",ApiRequest::mutation(Method::POST,&["api","v1","models",&self.loaded_id,"capabilities","reset"],None).query("provider_id",&self.loaded_provider).query("expected_capability_revision",self.revision));}if ui.button("Apply enabled switch…").clicked(){self.review.prepare("toggle","Change model availability",ApiRequest::mutation(Method::PATCH,&["api","v1","models",&self.loaded_id,"toggle"],None).query("provider_id",&self.loaded_provider).query("is_enabled",self.enabled));}if ui.button("Delete model…").clicked(){self.review.prepare("delete","Delete this provider's model record",ApiRequest::mutation(Method::DELETE,&["api","v1","models",&self.loaded_id],None).query("provider_id",&self.loaded_provider));}});}
  });self.review.ui(ui,&mut self.job,api,runtime,ctx);if !self.result.is_null(){json_view(ui,&self.result);}});
    }
}
