//! Agent Studio binding pickers preserve immutable resource and credential references.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use serde_json::{Value, json};
#[derive(Default)]
pub struct BindingPicker {
    pub job: RemoteJob,
    catalog: Value,
    pub request_preview: bool,
}
fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn toggle(
    ui: &mut egui::Ui,
    bindings: &mut Vec<Value>,
    binding: Value,
    title: &str,
    description: &str,
    enabled: bool,
) -> bool {
    let mut checked = bindings.iter().any(|item| {
        item["type"] == binding["type"] && item["resource_id"] == binding["resource_id"]
    });
    ui.label(description);
    if ui
        .add_enabled(enabled, egui::Checkbox::new(&mut checked, title))
        .changed()
    {
        bindings.retain(|item| {
            item["type"] != binding["type"] || item["resource_id"] != binding["resource_id"]
        });
        if checked {
            bindings.push(binding);
        }
        true
    } else {
        false
    }
}
impl BindingPicker {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        capabilities: &mut String,
        knowledge: &mut String,
        model: &mut Value,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) -> bool {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                self.catalog = value;
            }
        }
        let mut dirty = false;
        egui::CollapsingHeader::new("Capability and knowledge pickers").default_open(true).show(ui,|ui|{self.job.status_ui(ui);if ui.add_enabled(!self.job.busy,egui::Button::new("Refresh available bindings")).clicked(){let api=api.clone();self.job.start("load_bindings",async move{let mut catalog=json!({});let mut degraded=Vec::<String>::new();for(key,path)in[("tools",vec!["api","v1","assistant","tools"]),("models",vec!["api","v1","assistant","models"]),("datasets",vec!["api","v1","knowledge","datasets"]),("skills",vec!["api","v1","skills"])]{let request=if key=="skills"{ApiRequest::get(&path).query("enabled_only",true)}else{ApiRequest::get(&path)};match api.execute(request).await{Ok(value)=>catalog[key]=value.get(key).cloned().unwrap_or(value),Err(error)=>{if matches!(error,ApiError::Status(401)){return Err(error);}degraded.push(key.into())}}}
   match api.execute(ApiRequest::get(&["api","v1","mcp","servers"])).await{Ok(servers)=>{let mut mcp=Vec::new();for server in rows(&servers,"servers").iter().filter(|s|s["enabled"]==true){let Some(id)=server["server_id"].as_str()else{continue;};let tools=api.execute(ApiRequest::get(&["api","v1","mcp","servers",id,"tools"])).await;let connections=api.execute(ApiRequest::get(&["api","v1","mcp","servers",id,"connections"])).await;if let(Ok(tools),Ok(connections))=(tools,connections){let connection=rows(&connections,"connections").iter().find(|c|c["enabled"]==true&&c["credential_configured"]==true);for tool in rows(&tools,"tools"){let mut tool=tool.clone();tool["server_name"]=server["name"].clone();tool["connection_id"]=connection.map(|c|c["connection_id"].clone()).unwrap_or(Value::Null);tool["principal_type"]=connection.map(|c|c["principal_type"].clone()).unwrap_or(Value::Null);mcp.push(tool);}}else{degraded.push(format!("MCP {id}"));}}catalog["mcp"]=json!(mcp);},Err(error)=>{if matches!(error,ApiError::Status(401)){return Err(error);}degraded.push("MCP".into())}}
   let available=api.execute(ApiRequest::get(&["api","v1","connectors","available"])).await;let mine=api.execute(ApiRequest::get(&["api","v1","connectors","mine"])).await;if let(Ok(available),Ok(mine))=(available,mine){let mut connectors=Vec::new();for item in available.as_array().into_iter().flatten(){let mut connector=item.clone();let provider=item["provider"].as_str().unwrap_or("");let connected=mine.as_array().is_some_and(|rows|rows.iter().any(|c|c["provider"]==provider));connector["connected"]=json!(connected);if connected&&provider=="confluence"{if let Ok(principals)=api.execute(ApiRequest::get(&["api","v1","connectors",provider,"principals"])).await{if let Some(p)=rows(&principals,"principals").iter().find(|p|p["enabled"]==true&&rows(p,"allowed_channels").iter().any(|c|c=="preview")){for key in ["grant_id","principal_type","scopes","allowed_channels"]{connector[key]=p[key].clone();}}}}connectors.push(connector);}catalog["connectors"]=json!(connectors);}else{degraded.push("Connectors".into());}catalog["degraded"]=json!(degraded);Ok(catalog)},runtime,ctx);}
   if let Some(degraded)=self.catalog["degraded"].as_array(){for item in degraded{ui.label(format!("Catalog unavailable: {}. Existing bindings are retained.",item.as_str().unwrap_or("unknown")));}}
   if let Ok(Value::Array(mut bindings))=serde_json::from_str::<Value>(capabilities){
    egui::CollapsingHeader::new("Platform tools").show(ui,|ui|{for tool in rows(&self.catalog,"tools").iter().filter(|t|!["mcp","skill","platform_tool_discovery"].contains(&t["capability_kind"].as_str().unwrap_or(""))){let id=tool["name"].as_str().unwrap_or("");dirty|=toggle(ui,&mut bindings,json!({"type":"native","resource_id":id,"config":{"risk":tool["risk_level"]}}),tool["title"].as_str().unwrap_or(id),tool["summary"].as_str().or_else(||tool["description"].as_str()).unwrap_or(""),!id.is_empty());}});
    egui::CollapsingHeader::new("MCP tools").show(ui,|ui|{for tool in rows(&self.catalog,"mcp"){ui.label(format!("{} · {} · schema {} · snapshot {}",tool["server_name"],tool["principal_type"],tool["schema_hash"],tool["snapshot_id"]));dirty|=toggle(ui,&mut bindings,json!({"type":"mcp","resource_id":tool["runtime_name"],"resource_version":tool["snapshot_id"],"schema_hash":tool["schema_hash"],"config":{"connection_id":tool["connection_id"],"principal_type":tool["principal_type"],"risk":tool["risk_level"]}}),tool["runtime_name"].as_str().unwrap_or("MCP tool"),tool["description"].as_str().unwrap_or(""),tool["enabled"]==true&&tool["connection_id"].as_str().is_some_and(|v|!v.is_empty()));}});
    egui::CollapsingHeader::new("Immutable skill versions").show(ui,|ui|{for skill in rows(&self.catalog,"skills"){let version=skill["version_id"].as_str().unwrap_or("");ui.label(format!("Version {} · content hash {}",skill["version"],skill["content_hash"]));dirty|=toggle(ui,&mut bindings,json!({"type":"skill","resource_id":version,"resource_version":version,"schema_hash":skill["content_hash"],"config":{}}),skill["title"].as_str().or_else(||skill["name"].as_str()).unwrap_or("Skill"),skill["description"].as_str().unwrap_or(""),!version.is_empty());}});
    egui::CollapsingHeader::new("Connector principals").show(ui,|ui|{for connector in rows(&self.catalog,"connectors"){let can_bind=connector["provider"]=="confluence"&&connector["grant_id"].is_string()&&connector["principal_type"].is_string();ui.label(format!("Connected {} · principal {} · preview channels {}",connector["connected"],connector["principal_type"],connector["allowed_channels"]));dirty|=toggle(ui,&mut bindings,json!({"type":"connector","resource_id":"confluence_read","config":{"provider":connector["provider"],"principal_type":connector["principal_type"],"grant_id":connector["grant_id"],"tool_name":"confluence_read","risk":"low"}}),connector["display_name"].as_str().unwrap_or("Connector"),if can_bind{"Ready for preview"}else{"A configured explicit principal is required"},can_bind);}});
    if dirty{*capabilities=serde_json::to_string_pretty(&bindings).unwrap_or_default();}
   }else{ui.label("Fix the capability JSON array before using pickers.");}
   if let Ok(Value::Array(mut bindings))=serde_json::from_str::<Value>(knowledge){let mut changed=false;egui::CollapsingHeader::new("Knowledge datasets").show(ui,|ui|{for dataset in rows(&self.catalog,"datasets"){let id=dataset["dataset_id"].as_str().or_else(||dataset["id"].as_str()).unwrap_or("");ui.push_id(id,|ui|{let mut selected=bindings.iter().any(|b|b["dataset_id"]==id);if ui.checkbox(&mut selected,dataset["name"].as_str().unwrap_or(id)).changed(){bindings.retain(|b|b["dataset_id"]!=id);if selected{bindings.push(json!({"dataset_id":id,"retrieval_config":{"mode":"auto","top_k":5,"threshold":0.4,"include_images":false}}));}changed=true;}ui.label(format!("Permission {} · {}",dataset["my_permission"],dataset["description"]));if let Some(binding)=bindings.iter_mut().find(|b|b["dataset_id"]==id){let before=binding.clone();let config=&mut binding["retrieval_config"];let mut mode=config["mode"].as_str().unwrap_or("auto").to_owned();select_text(ui,"binding_mode",&mut mode,&["auto","tool","off"]);config["mode"]=json!(mode);let mut k=config["top_k"].as_u64().unwrap_or(5);if ui.add(egui::DragValue::new(&mut k).range(1..=20).prefix("Top K ")).changed(){config["top_k"]=json!(k);}let mut threshold=config["threshold"].as_f64().unwrap_or(0.4);if ui.add(egui::Slider::new(&mut threshold,0.0..=1.0).text("Threshold")).changed(){config["threshold"]=json!(threshold);}let mut images=config["include_images"].as_bool().unwrap_or(false);if ui.checkbox(&mut images,"Include source images").changed(){config["include_images"]=json!(images);}changed|=*binding!=before;}});}});if changed{*knowledge=serde_json::to_string_pretty(&bindings).unwrap_or_default();dirty=true;}}
   egui::CollapsingHeader::new("Model picker").show(ui,|ui|{for candidate in rows(&self.catalog,"models"){let id=candidate["id"].as_str().or_else(||candidate["model_id"].as_str()).unwrap_or("");if ui.selectable_label(model["model_id"]==id,candidate["name"].as_str().unwrap_or(id)).clicked(){model["model_id"]=json!(id);model["provider_id"]=candidate.get("provider").or_else(||candidate.get("provider_id")).cloned().unwrap_or(json!("dashscope"));dirty=true;}}});
   ui.label("High-risk capabilities remain subject to runtime confirmation and release gates.");if ui.button("Test selected bindings in saved-draft preview").clicked(){self.request_preview=true;}
  });
        dirty
    }
}
