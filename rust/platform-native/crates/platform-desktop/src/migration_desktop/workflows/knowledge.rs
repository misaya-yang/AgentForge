//! Dataset/document authoring and retrieval, using the existing text-only upload contract.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::{io::Read, path::PathBuf};
pub struct KnowledgeEditor {
    pub sources: super::knowledge_sources::KnowledgeSources,
    pub evaluation: super::knowledge_eval::KnowledgeEval,
    pub citation: super::source_preview::SourcePreview,
    pub documents: super::documents::DocumentEditor,
    pub embedding: super::embedding::EmbeddingWizard,
    pub qa: super::stream_panel::StreamPanel,
    pub progress: super::stream_panel::StreamPanel,
    pub qa_model: String,
    bound_dataset: String,
    pub job: RemoteJob,
    pub dataset_id: String,
    pub document_id: String,
    pub title: String,
    pub content: String,
    pub source_url: String,
    pub selected_file: Option<PathBuf>,
    pub dropped_bytes: Option<Vec<u8>>,
    pub dropped_name: String,
    pub mode: String,
    pub query: String,
    pub top_k: u32,
    pub threshold: f64,
    pub rerank: bool,
    pub mmr: bool,
    pub metadata: String,
    pub advanced_retrieval: String,
    pub confirm: bool,
    pub result: Value,
    pub dataset_name: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub upload_pending: bool,
}
impl Default for KnowledgeEditor {
    fn default() -> Self {
        Self {
            sources: Default::default(),
            evaluation: Default::default(),
            citation: Default::default(),
            documents: super::documents::DocumentEditor::default(),
            embedding: super::embedding::EmbeddingWizard::default(),
            qa: super::stream_panel::StreamPanel::default(),
            progress: super::stream_panel::StreamPanel::default(),
            qa_model: String::new(),
            bound_dataset: String::new(),
            job: RemoteJob::default(),
            dataset_id: String::new(),
            document_id: String::new(),
            title: String::new(),
            content: String::new(),
            source_url: String::new(),
            selected_file: None,
            dropped_bytes: None,
            dropped_name: String::new(),
            mode: "hybrid".into(),
            query: String::new(),
            top_k: 5,
            threshold: 0.4,
            rerank: false,
            mmr: false,
            metadata: "{}".into(),
            advanced_retrieval: "{}".into(),
            confirm: false,
            result: Value::Null,
            dataset_name: String::new(),
            embedding_provider: String::new(),
            embedding_model: String::new(),
            upload_pending: false,
        }
    }
}
impl KnowledgeEditor {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        api: &DesktopApi,
        selected: &str,
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
    ) {
        if self.bound_dataset != self.dataset_id && !self.job.busy {
            self.qa = super::stream_panel::StreamPanel::default();
            self.progress = super::stream_panel::StreamPanel::default();
            self.result = Value::Null;
            self.citation.close();
            self.bound_dataset = self.dataset_id.clone();
        }
        if let Some(message) = self.job.poll() {
            self.upload_pending = false;
            if let Ok(value) = message.result {
                if message.kind == "create_dataset" {
                    self.dataset_id = value["id"]
                        .as_str()
                        .or_else(|| value["dataset_id"].as_str())
                        .unwrap_or("")
                        .into();
                }
                if message.kind == "upload"
                    || message.kind == "create_text"
                    || message.kind == "create_url"
                {
                    self.document_id = value["id"]
                        .as_str()
                        .or_else(|| value["document_id"].as_str())
                        .unwrap_or("")
                        .into();
                    self.confirm = false;
                }
                self.result = value;
            }
        }
        egui::CollapsingHeader::new("Knowledge workspace").default_open(true).show(ui,|ui|{self.job.status_ui(ui);ui.add_enabled_ui(!self.job.busy,|ui|{ui.horizontal(|ui|{text_field(ui,"Dataset ID",&mut self.dataset_id);if ui.button("Use selected dataset").clicked()&&!selected.is_empty(){self.dataset_id=selected.into();self.result=Value::Null;self.document_id.clear();}if ui.add_enabled(!self.dataset_id.is_empty(),egui::Button::new("Load documents")).clicked(){self.job.request("load_documents",api,ApiRequest::get(&["api","v1","knowledge",&self.dataset_id,"documents"]),runtime,ctx);}});
        egui::CollapsingHeader::new("Create dataset").show(ui,|ui|{text_field(ui,"Dataset name",&mut self.dataset_name);text_field(ui,"Embedding provider",&mut self.embedding_provider);text_field(ui,"Embedding model",&mut self.embedding_model);if ui.add_enabled(self.confirm&&!self.dataset_name.is_empty()&&!self.embedding_provider.is_empty()&&!self.embedding_model.is_empty()&&!self.job.outcome_unknown,egui::Button::new("Create dataset")).clicked(){self.job.request("create_dataset",api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge","datasets"],Some(json!({"name":self.dataset_name,"embedding_provider":self.embedding_provider,"embedding_model":self.embedding_model}))),runtime,ctx);self.confirm=false;}});
        ui.checkbox(&mut self.confirm,"I reviewed the selected dataset and the content/change to submit");
        ui.add_enabled_ui(!self.dataset_id.is_empty(),|ui|{
            egui::CollapsingHeader::new("Add document").default_open(true).show(ui,|ui|{text_field(ui,"Document title",&mut self.title);ui.label("Text content");ui.add(egui::TextEdit::multiline(&mut self.content).desired_rows(5));ui.label("Metadata");ui.add(egui::TextEdit::multiline(&mut self.metadata).code_editor().desired_rows(3));if ui.add_enabled(self.confirm&&!self.title.is_empty()&&!self.content.is_empty()&&!self.job.outcome_unknown,egui::Button::new("Create text document")).clicked(){match json_object(&self.metadata){Ok(metadata)=>{self.job.request("create_text",api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.dataset_id,"documents","text"],Some(json!({"title":self.title,"content":self.content,"metadata":metadata}))),runtime,ctx);self.confirm=false;},Err(_)=>self.job.status="Metadata must be a JSON object".into()}}
                text_field(ui,"Source URL",&mut self.source_url);if ui.add_enabled(self.confirm&&!self.source_url.is_empty()&&!self.job.outcome_unknown,egui::Button::new("Import URL")).clicked(){let mut body=json!({"url":self.source_url});if !self.title.is_empty(){body["title"]=json!(self.title);}self.job.request("create_url",api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.dataset_id,"documents","url"],Some(body)),runtime,ctx);self.confirm=false;}
                ui.label("Drop one document file onto this panel to select it. It is not uploaded until confirmed.");for dropped in ctx.input(|i|i.raw.dropped_files.clone()){if let Some(path)=dropped.path{self.dropped_name=path.file_name().map(|s|s.to_string_lossy().into_owned()).unwrap_or_default();self.selected_file=Some(path);self.dropped_bytes=None;}else if let Some(bytes)=dropped.bytes{self.dropped_name=dropped.name;self.dropped_bytes=Some(bytes.to_vec());self.selected_file=None;}self.confirm=false;break;}
                if !self.dropped_name.is_empty(){ui.label(format!("Selected: {} · processing mode: text_only",self.dropped_name));if ui.add_enabled(self.confirm&&!self.job.outcome_unknown,egui::Button::new("Upload selected document once")).clicked(){let path=self.selected_file.clone();let supplied=self.dropped_bytes.take();let filename=self.dropped_name.clone();let api=api.clone();let dataset=self.dataset_id.clone();self.upload_pending=true;self.confirm=false;self.job.start("upload",async move{let bytes=if let Some(bytes)=supplied{bytes}else{tokio::task::spawn_blocking(move||{let path=path.ok_or(ApiError::Request)?;let file=std::fs::File::open(path).map_err(|_|ApiError::Request)?;if !file.metadata().map_err(|_|ApiError::Request)?.is_file(){return Err(ApiError::Request);}let mut bytes=Vec::new();file.take(100*1024*1024+1).read_to_end(&mut bytes).map_err(|_|ApiError::Request)?;if bytes.len()>100*1024*1024{return Err(ApiError::Request);}Ok(bytes)}).await.map_err(|_|ApiError::Request)??};api.upload_fields(&["api","v1","knowledge",&dataset,"documents","upload"],"file",&filename,"application/octet-stream",bytes,&[("processing_mode","text_only")]).await},runtime,ctx);}}});
            egui::CollapsingHeader::new("Retrieval and hit testing").default_open(true).show(ui,|ui|{ui.label("Query");ui.text_edit_multiline(&mut self.query);ui.horizontal(|ui|{egui::ComboBox::from_id_salt("retrieval_mode").selected_text(&self.mode).show_ui(ui,|ui|{for mode in ["dense","bm25","hybrid"]{ui.selectable_value(&mut self.mode,mode.into(),mode);}});ui.add(egui::DragValue::new(&mut self.top_k).range(1..=100).prefix("Top K "));});ui.add(egui::Slider::new(&mut self.threshold,0.0..=1.0).text("Score threshold"));ui.horizontal(|ui|{ui.checkbox(&mut self.rerank,"Rerank");ui.checkbox(&mut self.mmr,"MMR diversity");});egui::CollapsingHeader::new("Advanced retrieval parameters").show(ui,|ui|{ui.add(egui::TextEdit::multiline(&mut self.advanced_retrieval).code_editor().desired_rows(4));});for(op,label)in[("retrieve","Retrieve"),("hit_test","Hit test")]{if ui.add_enabled(!self.query.trim().is_empty(),egui::Button::new(label)).clicked(){match json_object(&self.advanced_retrieval){Ok(mut body)=>{for(key,value)in json!({"query":self.query,"mode":self.mode,"top_k":self.top_k,"score_threshold":self.threshold,"rerank":self.rerank,"mmr":self.mmr}).as_object().unwrap(){body[key]=value.clone();}self.job.request("retrieve",api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.dataset_id,op],Some(body)),runtime,ctx);},Err(_)=>self.job.status="Advanced retrieval settings must be a JSON object".into()}}}});
            egui::CollapsingHeader::new("Document maintenance").show(ui,|ui|{text_field(ui,"Document ID",&mut self.document_id);ui.horizontal_wrapped(|ui|{if ui.add_enabled(!self.document_id.is_empty(),egui::Button::new("Read document")).clicked(){self.job.request("load_document",api,ApiRequest::get(&["api","v1","knowledge",&self.dataset_id,"documents",&self.document_id]),runtime,ctx);}for operation in ["reindex","reprocess","recover","retry"]{if ui.add_enabled(self.confirm&&!self.document_id.is_empty()&&!self.job.outcome_unknown,egui::Button::new(operation)).clicked(){self.job.request(operation,api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.dataset_id,"documents",&self.document_id,operation],None),runtime,ctx);self.confirm=false;}}});});
        });if let Some(hits)=self.result["results"].as_array(){for hit in hits{egui::Frame::group(ui.style()).show(ui,|ui|{ui.label(format!("Document {} · segment {} · score {}",hit["document_id"].as_str().unwrap_or(""),hit["segment_id"].as_str().unwrap_or(""),hit["score"]));ui.label(hit["text"].as_str().unwrap_or(""));if ui.button("Open cited source with current-access check").clicked(){self.citation.open(super::source_preview::citation_identity(&self.dataset_id,hit));}if !hit["source_hash"].is_null(){ui.weak(format!("Source version {} · hash {}",hit["source_version"],hit["source_hash"]));}});}}else if !self.result.is_null(){json_view(ui,&self.result);}
    });
        egui::CollapsingHeader::new("Knowledge answer stream").show(ui,|ui|{
            text_field(ui,"Answer model ID (blank uses server default)",&mut self.qa_model);
            ui.label("Uses the retrieval query and settings above.");
            if ui.add_enabled(!self.dataset_id.is_empty()&&!self.query.trim().is_empty()&&!self.qa.active,egui::Button::new("Generate grounded answer once")).clicked(){
                let mut body=json!({"query":self.query,"top_k":self.top_k,"mode":self.mode,"rerank":self.rerank,"mmr":self.mmr,"include_raw_results":true});
                if !self.qa_model.is_empty(){body["llm_config"]=json!({"provider":"dashscope","model":self.qa_model});}
                self.qa.start(api,ApiRequest::mutation(Method::POST,&["api","v1","knowledge",&self.dataset_id,"qa","stream"],Some(body)),true,runtime,ctx);self.qa.source_dataset=self.dataset_id.clone();
            }
            self.qa.ui(ui,api,runtime,ctx);self.job.expired|=self.qa.expired;
        });
        egui::CollapsingHeader::new("Document ingestion progress").show(ui,|ui|{
            if ui.add_enabled(!self.dataset_id.is_empty()&&!self.progress.active,egui::Button::new("Observe document progress")).clicked(){
                let mut request=ApiRequest::get(&["api","v1","knowledge",&self.dataset_id,"documents","progress"]);
                if let Some(cursor)=&self.progress.last_event_id{request=request.header("Last-Event-ID",cursor);}
                self.progress.start(api,request,false,runtime,ctx);
            }
            self.progress.ui(ui,api,runtime,ctx);self.job.expired|=self.progress.expired;
        });
        self.sources.ui(ui,api,&self.dataset_id,runtime,ctx);self.job.expired|=self.sources.job.expired;
        if self.result.get("results").is_some()&&ui.button("Use retrieval observation for feedback / failure review").clicked(){self.sources.seed_observation(&self.dataset_id,&self.query,&self.result,false);}
        self.evaluation.ui(ui,api,&self.dataset_id,runtime,ctx);self.job.expired|=self.evaluation.job.expired;
        self.citation.ui(ui,api,runtime,ctx);self.job.expired|=self.citation.job.expired;
        self.documents.ui(ui,api,&self.dataset_id,runtime,ctx);self.job.expired|=self.documents.job.expired;
        self.embedding.ui(ui,api,&self.dataset_id,runtime,ctx);self.job.expired|=self.embedding.job.expired;
    });
    }
}
