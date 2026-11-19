//! Labelled retrieval cases, authoritative persistence and side-by-side IR metrics.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::{
    common::*,
    eval_store::{self, EvalCase},
};
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub struct KnowledgeEval {
    pub job: RemoteJob,
    bound: String,
    linked: String,
    cases: Vec<EvalCase>,
    removed: HashSet<String>,
    loaded: bool,
    presets: Vec<Value>,
    preset_a: String,
    preset_b: String,
    result: Value,
    result_signature: String,
    result_a: String,
    result_b: String,
    gate_k: u64,
    ndcg: f64,
    recall: f64,
    mrr: f64,
    save_confirm: bool,
    import_text: String,
    import_confirm: bool,
    export_text: String,
    sequence: u64,
    selected_case: String,
}
impl Default for KnowledgeEval {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            bound: String::new(),
            linked: String::new(),
            cases: Vec::new(),
            removed: HashSet::new(),
            loaded: false,
            presets: Vec::new(),
            preset_a: String::new(),
            preset_b: String::new(),
            result: Value::Null,
            result_signature: String::new(),
            result_a: String::new(),
            result_b: String::new(),
            gate_k: 5,
            ndcg: 0.8,
            recall: 0.8,
            mrr: 0.8,
            save_confirm: false,
            import_text: String::new(),
            import_confirm: false,
            export_text: String::new(),
            sequence: 0,
            selected_case: String::new(),
        }
    }
}
impl KnowledgeEval {
    fn reload(&mut self, api: &DesktopApi, runtime: &tokio::runtime::Runtime, ctx: &egui::Context) {
        let api = api.clone();
        let kb = self.bound.clone();
        self.job.start("load_cases",async move{let found=eval_store::find_link(&api,&kb).await?;if let Some(dataset)=found{let id=dataset["dataset_id"].as_str().ok_or(ApiError::Response)?;Ok(json!({"dataset_id":id,"examples":eval_store::list_examples(&api,id).await?}))}else{Ok(json!({"dataset_id":"","examples":[]}))}},runtime,ctx);
    }
    fn poll(&mut self) {
        if let Some(message) = self.job.poll() {
            if let Ok(value) = message.result {
                match message.kind.as_str() {
                    "load_cases" | "save_cases" | "import" => {
                        self.linked = value["dataset_id"].as_str().unwrap_or("").into();
                        self.cases = value["examples"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(eval_store::project)
                            .collect();
                        self.removed.clear();
                        self.loaded = true;
                        self.save_confirm = false;
                        self.import_confirm = false;
                    }
                    "load_presets" => {
                        self.presets = value["presets"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|p| super::retrieval_presets::valid(&p["config"]))
                            .collect();
                        self.preset_a = value["recommended_default"]
                            .as_str()
                            .filter(|name| self.presets.iter().any(|p| p["name"] == *name))
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                self.presets
                                    .first()
                                    .and_then(|p| p["name"].as_str())
                                    .unwrap_or("")
                                    .into()
                            });
                        self.preset_b = self
                            .presets
                            .iter()
                            .find_map(|p| p["name"].as_str().filter(|name| *name != self.preset_a))
                            .unwrap_or(&self.preset_a)
                            .into();
                    }
                    "evaluate" => {
                        self.result_signature = value["signature"].as_str().unwrap_or("").into();
                        self.result_a = value["preset_a"].as_str().unwrap_or("").into();
                        self.result_b = value["preset_b"].as_str().unwrap_or("").into();
                    }
                    "export" => {
                        self.export_text = value["examples"]
                            .as_array()
                            .map(|rows| {
                                rows.iter()
                                    .filter_map(|v| serde_json::to_string(v).ok())
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                    }
                    kind if kind.starts_with("candidates:") => {
                        if let Some(case) = self
                            .cases
                            .iter_mut()
                            .find(|c| c.id == kind.trim_start_matches("candidates:"))
                        {
                            case.candidates = value["results"].clone();
                        }
                    }
                    _ => {}
                }
                self.result = value;
            }
        }
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        dataset: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        self.poll();
        if self.bound != dataset {
            if self.job.busy {
                ui.label("The previous knowledge evaluation operation is pending.");
                return;
            }
            *self = Self::default();
            self.bound = dataset.into();
        }
        egui::CollapsingHeader::new("Retrieval evaluation workbench").show(ui,|ui|{self.job.status_ui(ui);ui.label(format!("Knowledge dataset {} · linked evaluation dataset {}",self.bound,self.linked));if self.job.busy&&self.job.status=="Working…"{ui.label("Requests are submitted once; closing observation does not undo completed server work.");}ui.add_enabled_ui(!self.bound.is_empty()&&!self.job.busy,|ui|{
   ui.horizontal(|ui|{if ui.button("Load saved cases (read-only)").clicked(){self.reload(api,runtime,ctx);}if ui.button("Load validated retrieval presets").clicked(){let api=api.clone();self.job.start("load_presets",async move{api.execute_with_timeout(ApiRequest::get(&["api","v1","knowledge","retrieval","presets"]),Duration::from_secs(15)).await},runtime,ctx);}});
   if !self.loaded{ui.label("Load linked cases before editing. Reading never creates an evaluation dataset.");return;}
   if ui.button("Add labelled query").clicked(){self.sequence+=1;let n=SystemTime::now().duration_since(UNIX_EPOCH).map(|v|v.as_nanos()).unwrap_or(0);self.cases.push(EvalCase{id:format!("case_{n:x}_{:x}",self.sequence),..Default::default()});self.save_confirm=false;}
   let mut remove=None;let mut candidates=None;for(index,case)in self.cases.iter_mut().enumerate(){ui.push_id(&case.id,|ui|{egui::Frame::group(ui.style()).show(ui,|ui|{ui.strong(&case.id);if ui.button("Remove this case").clicked(){remove=Some(index);}ui.label("Query");if ui.text_edit_multiline(&mut case.query).changed(){self.save_confirm=false;}ui.label("Relevant segment IDs (commas or newlines; labels are user judgments)");if ui.text_edit_multiline(&mut case.relevant).changed(){self.save_confirm=false;}if ui.add_enabled(!case.query.trim().is_empty(),egui::Button::new("Find 10 candidate segments to label")).clicked(){candidates=Some((case.id.clone(),case.query.clone()));}if let Some(rows)=case.candidates.as_array(){let mut ids=eval_store::relevant_ids(&case.relevant);let mut seen=HashSet::new();for candidate in rows{let Some(id)=candidate["segment_id"].as_str()else{continue;};if !seen.insert(id){continue;}let mut relevant=ids.iter().any(|v|v==id);if ui.checkbox(&mut relevant,format!("{id} · score {}",candidate["score"])).changed(){ids.retain(|v|v!=id);if relevant{ids.push(id.into());}case.relevant=ids.join("\n");self.save_confirm=false;}ui.label(candidate["text"].as_str().unwrap_or(""));}}});});}if let Some(index)=remove{if let Some(id)=self.cases[index].example_id.clone(){self.removed.insert(id);}self.cases.remove(index);self.save_confirm=false;}if let Some((id,query))=candidates{self.selected_case=id.clone();self.job.request(&format!("candidates:{id}"),api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.bound,"retrieve"],Some(json!({"query":query,"top_k":10,"mode":"hybrid"}))),runtime,ctx);}
   ui.checkbox(&mut self.save_confirm,"Save these cases; delete only persisted cases I explicitly removed");if ui.add_enabled(self.save_confirm&&!self.job.outcome_unknown&&(!self.cases.is_empty()||!self.removed.is_empty())&&self.cases.iter().all(|c|!c.query.trim().is_empty()),egui::Button::new("Persist reviewed case changes once")).clicked(){let api=api.clone();let kb=self.bound.clone();let cases=self.cases.clone();let removed=self.removed.clone();self.save_confirm=false;self.job.start("save_cases",async move{eval_store::save_cases(&api,&kb,cases,removed).await},runtime,ctx);}
   self.import_ui(ui,api,runtime,ctx);
   egui::CollapsingHeader::new("A/B retrieval evaluation").default_open(true).show(ui,|ui|{for(label,current)in[("Preset A",&mut self.preset_a),("Preset B",&mut self.preset_b)]{egui::ComboBox::from_id_salt(label).selected_text(current.as_str()).show_ui(ui,|ui|{for preset in &self.presets{if let Some(name)=preset["name"].as_str(){ui.selectable_value(current,name.into(),preset["label"].as_str().unwrap_or(name));}}});}let a=self.presets.iter().find(|p|p["name"]==self.preset_a).cloned();let b=self.presets.iter().find(|p|p["name"]==self.preset_b).cloned();let runnable=(1..=20).contains(&self.cases.len())&&self.cases.iter().all(|c|!c.query.trim().is_empty()&&!eval_store::relevant_ids(&c.relevant).is_empty())&&a.is_some()&&b.is_some();ui.label("Run 1–20 labelled queries at K = 1, 3, 5, 10. Retrieved candidates are not automatically treated as truth.");if ui.add_enabled(runnable,egui::Button::new("Run A/B evaluation once")).clicked(){let a=a.unwrap();let b=b.unwrap();let mut body_a=a["config"].clone();let mut body_b=b["config"].clone();let cases:Vec<Value>=self.cases.iter().map(|c|json!({"case_id":c.id,"query":c.query,"relevant_segment_ids":eval_store::relevant_ids(&c.relevant)})).collect();for body in [&mut body_a,&mut body_b]{body["cases"]=json!(cases);body["k_values"]=json!([1,3,5,10]);body["return_retrieved"]=json!(true);}let api=api.clone();let kb=self.bound.clone();let signature=eval_store::signature(&self.cases);let preset_a=self.preset_a.clone();let preset_b=self.preset_b.clone();self.result=Value::Null;self.job.start("evaluate",async move{let(a,b)=tokio::try_join!(api.execute_with_timeout(ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&kb,"retrieve_evaluate"],Some(body_a)),Duration::from_secs(120)),api.execute_with_timeout(ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&kb,"retrieve_evaluate"],Some(body_b)),Duration::from_secs(120)))?;Ok(json!({"a":a,"b":b,"signature":signature,"preset_a":preset_a,"preset_b":preset_b}))},runtime,ctx);}});
  });if self.job.busy&&(self.job.current_kind.starts_with("load_")||self.job.current_kind=="evaluate"||self.job.current_kind.starts_with("candidates:"))&&ui.button("Detach in-flight read/evaluation (does not reverse saved changes)").clicked(){self.job.cancel_observation();}
  if self.result.get("a").is_some(){let stale=self.result_signature!=eval_store::signature(&self.cases)||self.result_a!=self.preset_a||self.result_b!=self.preset_b;if stale{ui.label("These metrics are stale because labels or preset selection changed.");}ui.horizontal(|ui|{for k in [1,3,5,10]{ui.selectable_value(&mut self.gate_k,k,format!("Gate K={k}"));}});ui.add(egui::Slider::new(&mut self.ndcg,0.0..=1.0).text("Minimum nDCG"));ui.add(egui::Slider::new(&mut self.recall,0.0..=1.0).text("Minimum recall"));ui.add(egui::Slider::new(&mut self.mrr,0.0..=1.0).text("Minimum MRR"));let key=self.gate_k.to_string();for variant in ["a","b"]{let metrics=&self.result[variant]["metrics"][&key];let passes=metrics["ndcg_at_k"].as_f64().is_some_and(|v|v>=self.ndcg)&&metrics["recall_at_k"].as_f64().is_some_and(|v|v>=self.recall)&&metrics["mrr"].as_f64().is_some_and(|v|v>=self.mrr);ui.strong(format!("{} gate: {}",variant.to_uppercase(),if stale{"stale"}else if metrics.is_null(){"unavailable"}else if passes{"pass"}else{"fail"}));json_view(ui,metrics);egui::CollapsingHeader::new(format!("{variant}: actual pipeline evidence")).show(ui,|ui|{json_view(ui,&self.result[variant]["requested_config"]);json_view(ui,&self.result[variant]["case_metadata"]);json_view(ui,&self.result[variant]["cases"]);});}}else if !self.result.is_null(){json_view(ui,&self.result);}
 });
    }
    fn import_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Golden JSONL import/export").show(ui,|ui|{ui.label("Paste UTF-8 JSONL (maximum 8 MiB). A linked evaluation dataset is created only on confirmed import/save.");ui.add(egui::TextEdit::multiline(&mut self.import_text).code_editor().desired_rows(6));ui.checkbox(&mut self.import_confirm,"Import these validated cases with skip_duplicates");if ui.add_enabled(self.import_confirm&&!self.job.outcome_unknown,egui::Button::new("Import reviewed JSONL once")).clicked(){match eval_store::parse_jsonl(&self.import_text){Ok(cases)if !cases.is_empty()=>{let api=api.clone();let kb=self.bound.clone();self.import_confirm=false;self.job.start("import",async move{let dataset=eval_store::resolve_link(&api,&kb).await?;let id=dataset["dataset_id"].as_str().ok_or(ApiError::Response)?;let mut imported=0u64;let mut skipped=0u64;for chunk in cases.chunks(500){let value=api.execute(ApiRequest::mutation(Method::POST,&["api","v1","eval","datasets",id,"examples:import"],Some(json!({"examples":chunk,"mode":"skip_duplicates"})))).await?;imported+=value["imported"].as_u64().unwrap_or(0);skipped+=value["skipped"].as_u64().unwrap_or(0);}Ok(json!({"dataset_id":id,"imported":imported,"skipped":skipped,"examples":eval_store::list_examples(&api,id).await?}))},runtime,ctx);},Ok(_)=>self.job.status="No import cases supplied".into(),Err(error)=>self.job.status=error}}if ui.add_enabled(!self.linked.is_empty(),egui::Button::new("Export persisted examples as JSONL")).clicked(){self.job.request("export",api,ApiRequest::get(&["api","v1","eval","datasets",&self.linked,"examples:export"]),runtime,ctx);}if !self.export_text.is_empty(){ui.add(egui::TextEdit::multiline(&mut self.export_text).code_editor().interactive(false).desired_rows(5));if ui.button("Copy JSONL export").clicked(){ctx.copy_text(self.export_text.clone());}}});
    }
}
