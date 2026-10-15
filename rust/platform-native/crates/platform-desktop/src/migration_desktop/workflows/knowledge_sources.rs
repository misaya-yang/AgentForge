//! Source status, query/feedback observability and reviewed failure-to-evaluation saves.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::{common::*, eval_store};
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::{io::Read, path::PathBuf, time::Duration};
pub struct KnowledgeSources {
    pub job: RemoteJob,
    sync: super::knowledge_source_sync::SourceSync,
    controls: super::knowledge_connections::ConnectionControls,
    review: ReviewedAction,
    bound: String,
    sources: Value,
    sources_failed: bool,
    result: Value,
    import_mode: String,
    title: String,
    url: String,
    file: Option<PathBuf>,
    file_bytes: Option<Vec<u8>>,
    file_name: String,
    upload_confirm: bool,
    query_mode: String,
    zero_results: bool,
    cursor: String,
    limit: u32,
    feedback_rating: String,
    feedback_reason: String,
    target: String,
    trace: String,
    fingerprint: String,
    segment: String,
    comment: String,
    query: String,
    expected: String,
    observed: String,
    hits: String,
    failure_reason: String,
    failure_source: String,
    save_failure_confirm: bool,
    evaluator_id: String,
    judge_model: String,
    judge_provider: String,
    judge_metrics: String,
    contexts: String,
    only_unscored: bool,
    score_confirm: bool,
}
impl Default for KnowledgeSources {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            sync: Default::default(),
            controls: Default::default(),
            review: ReviewedAction::default(),
            bound: String::new(),
            sources: Value::Null,
            sources_failed: false,
            result: Value::Null,
            import_mode: String::new(),
            title: String::new(),
            url: String::new(),
            file: None,
            file_bytes: None,
            file_name: String::new(),
            upload_confirm: false,
            query_mode: String::new(),
            zero_results: false,
            cursor: String::new(),
            limit: 50,
            feedback_rating: "negative".into(),
            feedback_reason: "other".into(),
            target: "retrieval_hit".into(),
            trace: String::new(),
            fingerprint: String::new(),
            segment: String::new(),
            comment: String::new(),
            query: String::new(),
            expected: String::new(),
            observed: String::new(),
            hits: "[]".into(),
            failure_reason: String::new(),
            failure_source: "kb-hit-test".into(),
            save_failure_confirm: false,
            evaluator_id: String::new(),
            judge_model: String::new(),
            judge_provider: "dashscope".into(),
            judge_metrics: "context_precision\ncontext_recall".into(),
            contexts: String::new(),
            only_unscored: true,
            score_confirm: false,
        }
    }
}
impl KnowledgeSources {
    pub fn close_sync_reviews(&mut self) { self.sync.close_reviews(); self.controls.close_reviews(); }
    pub fn seed_observation(&mut self, dataset: &str, query: &str, observation: &Value, qa: bool) {
        if self.bound != dataset || self.job.busy {
            return;
        }
        self.query = query.into();
        self.trace = observation["trace_id"].as_str().unwrap_or("").into();
        self.fingerprint = observation["query_fingerprint"]
            .as_str()
            .unwrap_or("")
            .into();
        self.observed = observation["answer"].as_str().unwrap_or("").into();
        self.hits = serde_json::to_string_pretty(
            &observation
                .get("results")
                .or_else(|| observation.get("retrieved_chunks"))
                .cloned()
                .unwrap_or(json!([])),
        )
        .unwrap_or_else(|_| "[]".into());
        self.failure_source = if qa { "kb-qa" } else { "kb-hit-test" }.into();
        self.target = if qa { "qa_answer" } else { "retrieval_hit" }.into();
        self.save_failure_confirm = false;
    }
    fn poll(&mut self) {
        if let Some(message) = self.job.poll() {
            match message.result {
                Ok(value) => {
                    if message.kind == "load_sources" {
                        self.sources = value.clone();
                        self.sources_failed = false;
                    }
                    self.result = value;
                }
                Err(_) => {
                    if message.kind == "load_sources" {
                        self.sources_failed = true;
                    }
                }
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
        if self.sync.bind(api, dataset) {
            self.job.cancel_observation();
            *self = Self::default();
            self.sync.bind(api, dataset);
        }
        self.poll();
        self.job.expired |= self.controls.ui(ui, api, dataset,
            self.bound == dataset && !self.sources_failed && !self.job.busy
                && self.sources["source_connection_controls_available"] == true,
            runtime, ctx);
        if self.controls.has_pending(dataset) {
            // Close unsubmitted sync reviews but preserve exact submitted UUID recovery.
            self.sync.close_reviews();
        }
        if self.bound != dataset {
            if self.job.busy {
                ui.label("A previous source-workbench request is pending.");
                return;
            }
            let sync = std::mem::take(&mut self.sync);
            let controls = std::mem::take(&mut self.controls);
            *self = Self::default();
            self.sync = sync;
            self.controls = controls;
            self.bound = dataset.into();
        }
        // Consume this dataset's refresh only after navigation and any older job finish.
        // While busy, new sync is already disabled; the dataset-keyed signal stays retained.
        if !self.job.busy && self.controls.take_source_refresh(dataset) {
            self.sources_failed = true;
            self.sync.close_reviews();
            self.job.request("load_sources", api,
                ApiRequest::get(&["api", "v1", "knowledge", dataset, "sources"]), runtime, ctx);
        }
        egui::CollapsingHeader::new("Sources, observations and evaluation feedback").show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.bound.is_empty()&&!self.job.busy,|ui|{
   if ui.button("Refresh source state").clicked(){self.job.request("load_sources",api,ApiRequest::get(&["api","v1","knowledge",&self.bound,"sources"]),runtime,ctx);}let known=!self.sources.is_null()&&!self.sources_failed;ui.label(format!("File uploads: {} · URL imports: {}",if known{self.sources["file_uploads"]["count"].to_string()}else{"unknown".into()},if known{self.sources["url_imports"]["count"].to_string()}else{"unknown".into()}));let status=if self.sources_failed{"load_failed"}else if self.sources.is_null(){"unknown"}else{match self.sources["connector_status"].as_str(){Some("available")=>"available",Some("not_configured")=>"not_configured",Some("unavailable")=>"unavailable",_=>"unknown"}};ui.strong(format!("Recurring connector: {status}"));if status=="available"{if let Some(bindings)=self.sources["confluence_bindings"].as_array(){for binding in bindings{let problem=binding["has_problem"]==true||["error","failed","unavailable"].contains(&binding["status"].as_str().unwrap_or("").to_ascii_lowercase().as_str());let success=binding["last_success_at"].as_str().filter(|s|!s.is_empty());ui.label(format!("{} · {} · {}",binding["space_name"],binding["status"],if problem{"problem"}else if success.is_some(){"last_success"}else{"never_succeeded"}));if let Some(at)=success{ui.label(format!("Last successful sync: {at}"));if !problem{if let Some(count)=binding["page_count"].as_u64(){ui.label(format!("Last known pages: {count}"));}}}}}}ui.label("Recurring sync remains managed by the configured connector. Manual sync uses a separately reviewed saved binding.");ui.horizontal(|ui|{if ui.button("Upload document source").clicked(){self.import_mode="file".into();}if ui.button("Import URL source").clicked(){self.import_mode="url".into();}});self.import_ui(ui,api,runtime,ctx);
   egui::CollapsingHeader::new("Query history and feedback").show(ui,|ui|{select_text(ui,"query_mode_filter",&mut self.query_mode,&["","dense","bm25","hybrid","vector","keyword"]);ui.checkbox(&mut self.zero_results,"Only zero-result queries");text_field(ui,"Cursor",&mut self.cursor);ui.add(egui::DragValue::new(&mut self.limit).range(1..=200).prefix("Limit "));if ui.button("Load query page").clicked(){let mut r=ApiRequest::get(&["api","v1","knowledge",&self.bound,"queries"]).query("limit",self.limit).query("zero_results",self.zero_results);if !self.cursor.is_empty(){r=r.query("cursor",&self.cursor);}if !self.query_mode.is_empty(){r=r.query("mode",&self.query_mode);}self.job.request("load_queries",api,r,runtime,ctx);}if let Some(queries)=self.result["queries"].as_array(){for query in queries{if ui.button(query["query"].as_str().or_else(||query["query_content"].as_str()).unwrap_or("Select query observation")).clicked(){self.trace=query["trace_id"].as_str().unwrap_or("").into();self.fingerprint=query["query_fingerprint"].as_str().unwrap_or("").into();self.query=query["query"].as_str().or_else(||query["query_content"].as_str()).unwrap_or("").into();self.review.clear();}}}if let Some(cursor)=self.result["next_cursor"].as_str(){if ui.button("Use next cursor").clicked(){self.cursor=cursor.into();}}
    text_field(ui,"Trace ID",&mut self.trace);text_field(ui,"Query fingerprint",&mut self.fingerprint);select_text(ui,"feedback_target",&mut self.target,&["retrieval_hit","qa_answer"]);if self.target=="retrieval_hit"{text_field(ui,"Segment ID",&mut self.segment);}select_text(ui,"feedback_rating",&mut self.feedback_rating,&["positive","negative"]);select_text(ui,"feedback_reason",&mut self.feedback_reason,&["relevant","helpful","well_cited","irrelevant","incorrect","missing_context","bad_citation","stale","unsafe","other"]);text_field(ui,"Feedback comment",&mut self.comment);if ui.add_enabled(!self.trace.is_empty()&&!self.fingerprint.is_empty()&&(self.target=="qa_answer"||!self.segment.is_empty()),egui::Button::new("Save feedback…")).clicked(){let mut body=json!({"trace_id":self.trace,"query_fingerprint":self.fingerprint,"target_type":self.target,"rating":self.feedback_rating,"reason_code":self.feedback_reason,"comment":self.comment});if self.target=="retrieval_hit"{body["segment_id"]=json!(self.segment);}self.review.prepare("feedback","Save feedback on this exact observed result",ApiRequest::mutation(Method::PUT,&["api","v1","knowledge",&self.bound,"feedback"],Some(body)));}if ui.button("Read filtered feedback").clicked(){let mut r=ApiRequest::get(&["api","v1","knowledge",&self.bound,"feedback"]).query("limit",self.limit).query("rating",&self.feedback_rating).query("reason_code",&self.feedback_reason).query("target_type",&self.target);if !self.trace.is_empty(){r=r.query("trace_id",&self.trace);}if !self.cursor.is_empty(){r=r.query("cursor",&self.cursor);}self.job.request("load_feedback",api,r,runtime,ctx);}
   });self.failure_ui(ui,api,runtime,ctx);self.ragas_ui(ui,api,runtime,ctx);
  });
  {
      for binding in self.sync.visible_bindings(&self.bound, &self.sources) {
          self.job.expired |= self.sync.ui(ui, api, &self.bound, &binding, !self.sources_failed && self.sources["connector_status"] == "available" && !self.job.busy && !self.controls.has_pending(&self.bound), runtime, ctx);
      }
  }
  self.review.ui(ui,&mut self.job,api,runtime,ctx);if !self.result.is_null(){json_view(ui,&self.result);}});
    }
    fn import_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if self.import_mode.is_empty() {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            text_field(ui, "Source title", &mut self.title);
            if self.import_mode == "url" {
                text_field(ui, "Web page URL", &mut self.url);
                if ui
                    .add_enabled(!self.url.is_empty(), egui::Button::new("Review URL import"))
                    .clicked()
                {
                    let mut body = json!({"url":self.url});
                    if !self.title.is_empty() {
                        body["title"] = json!(self.title);
                    }
                    self.review.prepare(
                        "import_url",
                        "Fetch and ingest this URL into the dataset",
                        ApiRequest::mutation(
                            Method::POST,
                            &["api", "v1", "knowledge", &self.bound, "documents", "url"],
                            Some(body),
                        ),
                    );
                }
            } else {
                ui.label("Drop one document for text_only ingestion.");
                if !self.job.busy {
                    for file in ctx.input(|i| i.raw.dropped_files.clone()) {
                        self.file_name = file
                            .path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or(file.name);
                        self.file = file.path;
                        self.file_bytes = file.bytes.map(|b| b.to_vec());
                        self.upload_confirm = false;
                        break;
                    }
                }
                ui.label(&self.file_name);
                ui.checkbox(
                    &mut self.upload_confirm,
                    "Upload this file into the selected dataset",
                );
                if ui
                    .add_enabled(
                        self.upload_confirm
                            && !self.file_name.is_empty()
                            && !self.job.outcome_unknown,
                        egui::Button::new("Upload source once"),
                    )
                    .clicked()
                {
                    let api = api.clone();
                    let kb = self.bound.clone();
                    let path = self.file.clone();
                    let supplied = self.file_bytes.clone();
                    let filename = self.file_name.clone();
                    self.upload_confirm = false;
                    self.job.start(
                        "upload_source",
                        async move {
                            let bytes = tokio::task::spawn_blocking(move || {
                                if let Some(bytes) = supplied {
                                    return if bytes.len() <= 100 * 1024 * 1024 {
                                        Ok(bytes)
                                    } else {
                                        Err(ApiError::Request)
                                    };
                                }
                                let file = std::fs::File::open(path.ok_or(ApiError::Request)?)
                                    .map_err(|_| ApiError::Request)?;
                                if !file.metadata().map_err(|_| ApiError::Request)?.is_file() {
                                    return Err(ApiError::Request);
                                }
                                let mut bytes = Vec::new();
                                file.take(100 * 1024 * 1024 + 1)
                                    .read_to_end(&mut bytes)
                                    .map_err(|_| ApiError::Request)?;
                                if bytes.len() > 100 * 1024 * 1024 {
                                    return Err(ApiError::Request);
                                }
                                Ok(bytes)
                            })
                            .await
                            .map_err(|_| ApiError::Request)??;
                            api.upload_fields(
                                &["api", "v1", "knowledge", &kb, "documents", "upload"],
                                "file",
                                &filename,
                                "application/octet-stream",
                                bytes,
                                &[("processing_mode", "text_only")],
                            )
                            .await
                        },
                        runtime,
                        ctx,
                    );
                }
            }
            if ui.button("Close source import").clicked() {
                self.import_mode.clear();
                self.upload_confirm = false;
            }
        });
    }
    fn failure_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Save a failed observation to the evaluation review queue").show(ui,|ui|{select_text(ui,"failure_source",&mut self.failure_source,&["kb-hit-test","kb-qa"]);ui.label("Question");ui.text_edit_multiline(&mut self.query);ui.label("Expected answer supplied by reviewer");ui.text_edit_multiline(&mut self.expected);ui.label("Observed answer");ui.text_edit_multiline(&mut self.observed);text_field(ui,"Failure reason",&mut self.failure_reason);ui.label("Observed hits (evidence only; not relevance labels)");ui.add(egui::TextEdit::multiline(&mut self.hits).code_editor().desired_rows(4));ui.checkbox(&mut self.save_failure_confirm,"Save this observation as pending review, preserving source identities");if ui.add_enabled(self.save_failure_confirm&&!self.query.trim().is_empty()&&!self.expected.trim().is_empty()&&!self.job.outcome_unknown,egui::Button::new("Save failure revision once")).clicked(){match json_array(&self.hits){Ok(hits)=>{let api=api.clone();let kb=self.bound.clone();let query=self.query.trim().to_owned();let expected=self.expected.trim().to_owned();let source=self.failure_source.clone();let trace=self.trace.clone();let fingerprint=self.fingerprint.clone();let observed=self.observed.clone();let reason=self.failure_reason.trim().to_owned();let hits=hits.as_array().cloned().unwrap_or_default();self.save_failure_confirm=false;self.job.start("save_failure",async move{let dataset=eval_store::resolve_link(&api,&kb).await?;let id=dataset["dataset_id"].as_str().ok_or(ApiError::Response)?;let mut confirmed=None;if !trace.is_empty(){for delay in [0,100,300]{if delay>0{tokio::time::sleep(Duration::from_millis(delay)).await;}if api.execute(ApiRequest::get(&["api","v1","eval","traces",&trace]).query("trace_family","rag")).await.is_ok(){confirmed=Some(trace.clone());break;}}}let observation=if !trace.is_empty(){&trace}else if !fingerprint.is_empty(){&fingerprint}else{&query};let case=format!("kb-failure-{source}-{}-{}",eval_store::hash_input(&kb),eval_store::hash_input(observation));let latest=api.execute(ApiRequest::get(&["api","v1","eval","datasets",id,"kb-failures",&case])).await;let revision=match latest{Ok(value)=>value["metadata"]["case_revision"].as_u64().filter(|v|*v>0).unwrap_or(1),Err(ApiError::Status(404))=>0,Err(error)=>return Err(error)};let body=json!({"case_id":case,"kb_dataset_id":kb,"query":query,"expected_answer":expected,"source":source,"observed_segment_ids":hits.iter().filter_map(|h|h["segment_id"].as_str()).collect::<Vec<_>>(),"source_versions":eval_store::source_versions(&kb,&hits),"source_trace_id":confirmed,"kb_trace_id":optional_text(&trace),"query_fingerprint":optional_text(&fingerprint),"observed_answer":optional_text(&observed),"failure_reason":optional_text(&reason),"expected_revision":revision});api.execute(ApiRequest::mutation(Method::POST,&["api","v1","eval","datasets",id,"kb-failures:save"],Some(body))).await},runtime,ctx);},Err(_)=>self.job.status="Observed hits must be an array".into()}}});
    }
    fn ragas_ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        egui::CollapsingHeader::new("Knowledge judge workbench").show(ui,|ui|{if ui.button("Read seven-day knowledge quality summary").clicked(){self.job.request("load_ragas",api,ApiRequest::get(&["api","v1","eval","knowledge","summary"]).query("days",7).query("dataset_id",&self.bound),runtime,ctx);}text_field(ui,"Evaluator ID",&mut self.evaluator_id);ui.checkbox(&mut self.only_unscored,"Only unscored traces");if ui.add_enabled(!self.evaluator_id.is_empty(),egui::Button::new("Queue dataset scoring…")).clicked(){self.review.prepare("batch_score","Queue the selected evaluator for this dataset; provider usage may be incurred",ApiRequest::mutation(Method::POST,&["api","v1","eval","knowledge",&self.bound,"batch-score"],Some(json!({"evaluator_id":self.evaluator_id,"limit":self.limit,"only_unscored":self.only_unscored}))));}ui.label("Judge contexts (JSON array of strings)");ui.add(egui::TextEdit::multiline(&mut self.contexts).code_editor().desired_rows(3));text_field(ui,"Judge provider",&mut self.judge_provider);text_field(ui,"Judge model",&mut self.judge_model);ui.label("Metrics, one per line");ui.text_edit_multiline(&mut self.judge_metrics);if ui.add_enabled(!self.query.trim().is_empty(),egui::Button::new("Review retrieval judge request")).clicked(){match json_array(&self.contexts){Ok(contexts)if contexts.as_array().is_some_and(|a|a.iter().all(Value::is_string))=>{let mut body=json!({"query":self.query,"contexts":contexts,"metrics":lines(&self.judge_metrics),"ground_truth":optional_text(&self.expected),"dataset_id":self.bound});if !self.judge_model.is_empty(){body["llm_config"]=json!({"provider":self.judge_provider,"model":self.judge_model});}self.review.prepare("score_retrieval","Run a retrieval judge on these exact contexts",ApiRequest::mutation(Method::POST,&["api","v1","eval","knowledge","score-retrieval"],Some(body)));},_=>self.job.status="Contexts must be an array of strings".into()}}});
    }
}
