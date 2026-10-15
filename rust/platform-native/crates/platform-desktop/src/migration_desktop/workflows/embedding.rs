//! Source-faithful blue/green embedding migration wizard and durable-job observer.
use super::super::api::{ApiError, ApiRequest, DesktopApi};
use super::common::*;
use eframe::egui;
use reqwest::Method;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
pub struct EmbeddingWizard {
    pub job: RemoteJob,
    review: ReviewedAction,
    bound: String,
    description: Value,
    migration: Value,
    action_job: Value,
    result: Value,
    provider: String,
    model: String,
    version: String,
    dimension: u64,
    capabilities: String,
    explicit_capabilities: bool,
    retention: u64,
    keep_shadow: bool,
    purge_shadow: bool,
    abort_reason: String,
    sample: u64,
    top_k: u64,
    tolerance: f64,
    floor: f64,
    advanced_gate: bool,
    next_poll: Option<Instant>,
    next_describe: Option<Instant>,
    observing: bool,
    pointer_migration: String,
    pointer_job: String,
}
impl Default for EmbeddingWizard {
    fn default() -> Self {
        Self {
            job: RemoteJob::default(),
            review: ReviewedAction::default(),
            bound: String::new(),
            description: Value::Null,
            migration: Value::Null,
            action_job: Value::Null,
            result: Value::Null,
            provider: String::new(),
            model: String::new(),
            version: String::new(),
            dimension: 1024,
            capabilities: String::new(),
            explicit_capabilities: false,
            retention: 604800,
            keep_shadow: true,
            purge_shadow: true,
            abort_reason: String::new(),
            sample: 20,
            top_k: 5,
            tolerance: 0.05,
            floor: 0.8,
            advanced_gate: false,
            next_poll: None,
            next_describe: None,
            observing: true,
            pointer_migration: String::new(),
            pointer_job: String::new(),
        }
    }
}
fn normalized_job(value: &Value) -> &str {
    let state = value["state"].as_str().unwrap_or("");
    let error = value["error"].as_str().unwrap_or("").to_ascii_lowercase();
    if ["cancelled", "canceled"].contains(&state)
        || (state == "failed" && (error.contains("cancelled") || error.contains("canceled")))
    {
        "cancelled"
    } else if ["queued", "running", "succeeded"].contains(&state) {
        state
    } else {
        "failed"
    }
}
impl EmbeddingWizard {
    fn migration_id(&self) -> String {
        self.migration["migration_id"].as_str().unwrap_or("").into()
    }
    fn describe_request(&self) -> ApiRequest {
        ApiRequest::get(&[
            "api",
            "v1",
            "knowledge",
            "datasets",
            &self.bound,
            "embedding-migration",
        ])
    }
    fn action_request(&self, operation: &str, body: Option<Value>) -> ApiRequest {
        ApiRequest::mutation(
            Method::POST,
            &[
                "api",
                "v1",
                "knowledge",
                "datasets",
                &self.bound,
                "embedding-migration",
                &self.migration_id(),
                operation,
            ],
            body,
        )
    }
    fn active(&self) -> bool {
        !self.action_job.is_null()
            && ["queued", "running"].contains(&normalized_job(&self.action_job))
    }
    fn adopt_job(&mut self, mut value: Value) {
        if self.action_job["job_id"] == value["job_id"] {
            for key in ["reused", "poll_after_ms"] {
                if value.get(key).is_none() {
                    if let Some(v) = self.action_job.get(key) {
                        value[key] = v.clone();
                    }
                }
            }
        }
        self.pointer_job = value["job_id"].as_str().unwrap_or("").into();
        self.pointer_migration = value["migration_id"].as_str().unwrap_or("").into();
        self.action_job = value;
        self.next_poll = if self.active() && self.observing {
            Some(
                Instant::now()
                    + Duration::from_millis(
                        self.action_job["poll_after_ms"]
                            .as_u64()
                            .unwrap_or(1000)
                            .clamp(250, 5000),
                    ),
            )
        } else {
            None
        };
    }
    fn adopt_description(&mut self, value: Value) {
        self.migration = value
            .get("live_migration")
            .filter(|v| !v.is_null())
            .or_else(|| value.get("latest_migration").filter(|v| !v.is_null()))
            .cloned()
            .or_else(|| {
                value["recent_migrations"]
                    .as_array()
                    .and_then(|a| a.first().cloned())
            })
            .unwrap_or(Value::Null);
        let id = self.migration_id();
        let server_job = value
            .get("active_action_job")
            .filter(|v| !v.is_null())
            .cloned()
            .or_else(|| {
                value["recent_action_jobs"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|j| j["migration_id"] == id).cloned())
            });
        if let Some(job) = server_job {
            self.adopt_job(job);
        } else if !self.active() {
            self.action_job = Value::Null;
        }
        self.next_describe = if self.observing
            && ["shadow_build", "backfilling", "verified", "gating", "ready"]
                .contains(&self.migration["state"].as_str().unwrap_or(""))
        {
            Some(Instant::now() + Duration::from_secs(5))
        } else {
            None
        };
        self.description = value;
    }
    fn poll(&mut self) {
        if let Some(message) = self.job.poll() {
            match message.result {
                Ok(value) => {
                    if message.kind == "load_description" {
                        if value["dataset_id"].as_str() != Some(self.bound.as_str()) {
                            self.job.status = "Dataset receipt identity mismatch".into();
                            self.job.outcome_unknown = true;
                            return;
                        }
                        self.adopt_description(value.clone());
                    } else if message.kind == "load_job"
                        || ["backfill", "verify", "gate", "retry_action"]
                            .contains(&message.kind.as_str())
                    {
                        if value["dataset_id"].as_str() != Some(self.bound.as_str())
                            || value["job_id"].as_str().unwrap_or("").is_empty()
                        {
                            self.job.status = "Action receipt identity mismatch".into();
                            self.job.outcome_unknown = true;
                            return;
                        }
                        self.adopt_job(value.clone());
                        if !self.active() {
                            self.next_describe = Some(Instant::now());
                        }
                    } else {
                        self.next_describe = Some(Instant::now());
                    }
                    self.result = value;
                }
                Err(error) => {
                    if message.kind == "load_job" {
                        self.next_poll = None;
                    }
                    if message.kind == "load_description" {
                        self.next_describe = None;
                    }
                    if matches!(error, ApiError::Status(404 | 409)) {
                        self.next_describe = Some(Instant::now());
                    }
                }
            }
        }
    }
    fn allowed_actions(&self) -> Vec<&'static str> {
        let state = self.migration["state"].as_str().unwrap_or("");
        let pending = self.description["pending_chunks"].as_u64();
        let mut actions = match state {
            "shadow_build" => vec!["backfill", "abort"],
            "backfilling" | "failed" => {
                if pending == Some(0) {
                    vec!["backfill", "verify", "abort"]
                } else {
                    vec!["backfill", "abort"]
                }
            }
            "verified" | "gating" | "gate_failed" => vec!["gate", "abort"],
            "ready" => vec!["cutover", "abort"],
            "completed" => vec!["rollback"],
            "rolled_back" => vec!["abort"],
            _ => vec![],
        };
        if !self.action_job.is_null() {
            match normalized_job(&self.action_job) {
                "queued" => {
                    if self.action_job["migration_id"] != self.migration_id() {
                        actions.clear();
                    } else {
                        actions.retain(|a| *a == "abort");
                    }
                }
                "running" => actions.clear(),
                "failed" => actions.retain(|a| Some(*a) != self.action_job["action"].as_str()),
                _ => {}
            }
        }
        actions
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
                ui.label("A previous dataset's embedding request is pending.");
                return;
            }
            let expired = self.job.expired;
            *self = Self::default();
            self.bound = dataset.into();
            self.job.expired = expired;
        }
        if self.observing && !self.job.busy && !self.bound.is_empty() {
            if self.next_poll.is_some_and(|t| t <= Instant::now()) {
                self.next_poll = None;
                self.job.request(
                    "load_job",
                    api,
                    ApiRequest::get(&[
                        "api",
                        "v1",
                        "knowledge",
                        "datasets",
                        &self.bound,
                        "embedding-migration",
                        &self.pointer_migration,
                        "jobs",
                        &self.pointer_job,
                    ]),
                    runtime,
                    ctx,
                );
            } else if self.next_describe.is_some_and(|t| t <= Instant::now()) {
                self.next_describe = None;
                self.job.request(
                    "load_description",
                    api,
                    self.describe_request(),
                    runtime,
                    ctx,
                );
            }
        }
        for at in [self.next_poll, self.next_describe].into_iter().flatten() {
            ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
        egui::CollapsingHeader::new("Embedding migration wizard").show(ui,|ui|{self.job.status_ui(ui);ui.label(format!("Dataset: {}",self.bound));ui.add_enabled_ui(!self.bound.is_empty()&&!self.job.busy,|ui|{
   ui.horizontal(|ui|{if ui.button("Refresh authoritative migration state").clicked(){self.observing=true;self.review.clear();self.job.request("load_description",api,self.describe_request(),runtime,ctx);}if self.observing&&ui.button("Detach observation").clicked(){self.observing=false;self.next_poll=None;self.next_describe=None;}});
   if self.description.is_null(){ui.label("Load authoritative serving state before starting or changing a migration.");return;}
   ui.label(format!("Serving binding: {}",self.description["serving_binding"]["collection_name"]));let health=&self.description["collection_health"];ui.label(format!("Collection health: {} · live checked: {}",health["status"].as_str().unwrap_or("unknown"),health["checked_live"].as_bool().unwrap_or(false)));if !health.is_null(){json_view(ui,health);}else{ui.label("No live collection-health receipt is available. Binding presence alone is not proof of health.");}
   let state=self.migration["state"].as_str().unwrap_or("").to_string();ui.label(format!("Migration {} · {state}",self.migration_id()));let total=self.description["enabled_chunks"].as_u64().or_else(||self.migration["totals"]["enabled_chunks"].as_u64()).or_else(||self.migration["totals"]["verified_enabled_chunks"].as_u64()).unwrap_or(0);let pending=self.description["pending_chunks"].as_u64().or_else(||self.migration["totals"]["pending_after_backfill"].as_u64()).or_else(||if ["verified","gating","gate_failed","ready","completed"].contains(&state.as_str()){Some(0)}else{None});if let Some(pending)=pending{ui.label(format!("{} / {} chunks complete; {} pending",total.saturating_sub(pending),total,pending));}else{ui.label(format!("{} enabled chunks; pending count unknown",total));}
   let can_start=(state.is_empty()||["completed","abandoned"].contains(&state.as_str()))&&!self.active();egui::CollapsingHeader::new("1. Choose replacement embedding identity").show(ui,|ui|{text_field(ui,"Embedding provider",&mut self.provider);text_field(ui,"Embedding model",&mut self.model);text_field(ui,"Model version",&mut self.version);ui.add(egui::DragValue::new(&mut self.dimension).range(1..=8192).prefix("Dimensions "));ui.checkbox(&mut self.explicit_capabilities,"Set explicit capabilities (unchecked inherits serving capabilities)");ui.label("Capabilities, one per line; an explicit empty list clears them");ui.text_edit_multiline(&mut self.capabilities);let serving=&self.description["serving_binding"];let same=serving["embedding_provider"]==self.provider.trim()&&serving["embedding_model"]==self.model.trim()&&serving["embedding_model_version"].as_str().unwrap_or("")==self.version.trim()&&serving["embedding_dimension"].as_u64()==Some(self.dimension);if same{ui.label("Choose an embedding identity different from the serving binding.");}if ui.add_enabled(can_start&&!same&&!self.provider.trim().is_empty()&&!self.model.trim().is_empty(),egui::Button::new("2. Review shadow migration")).clicked(){let mut body=json!({"embedding_provider":self.provider.trim(),"embedding_model":self.model.trim(),"embedding_model_version":self.version.trim(),"embedding_dimension":self.dimension});if self.explicit_capabilities{let mut caps=lines(&self.capabilities);caps.sort();caps.dedup();body["capabilities"]=json!(caps);}self.review.prepare("start","Start a shadow collection; serving data remains on the existing binding until cutover",ApiRequest::mutation(Method::POST,&["api","v1","knowledge","datasets",&self.bound,"embedding-migration","start"],Some(body)));}});
   if !self.migration.is_null(){egui::CollapsingHeader::new("Migration actions").default_open(true).show(ui,|ui|{ui.add(egui::DragValue::new(&mut self.retention).prefix("Retain previous binding seconds "));ui.checkbox(&mut self.keep_shadow,"Keep shadow on rollback");ui.checkbox(&mut self.purge_shadow,"Purge shadow on abort");text_field(ui,"Abort reason",&mut self.abort_reason);ui.checkbox(&mut self.advanced_gate,"Override gate sampling defaults");if self.advanced_gate{ui.add(egui::DragValue::new(&mut self.sample).range(1..=10000).prefix("Sample size "));ui.add(egui::DragValue::new(&mut self.top_k).range(1..=100).prefix("Gate top K "));ui.add(egui::Slider::new(&mut self.tolerance,0.0..=1.0).text("Tolerance"));ui.add(egui::Slider::new(&mut self.floor,0.0..=1.0).text("Floor"));}
    for action in self.allowed_actions(){if ui.button(format!("Review {action}")).clicked(){let body=match action{"gate"=>Some(if self.advanced_gate{json!({"sample_size":self.sample,"top_k":self.top_k,"tolerance":self.tolerance,"floor":self.floor})}else{json!({})}),"cutover"=>Some(json!({"retention_seconds":self.retention})),"rollback"=>Some(json!({"keep_shadow":self.keep_shadow})),"abort"=>Some(json!({"reason":if self.abort_reason.trim().is_empty(){"aborted from desktop console"}else{self.abort_reason.trim()},"purge_shadow":self.purge_shadow})),_=>None};let label=match action{"cutover"=>"Cut over serving retrieval to the verified target collection","rollback"=>"Roll back serving retrieval to the retained source binding","abort"=>"Abort migration and apply the selected shadow purge policy",_=>"Queue a durable embedding action; accepted jobs are observed, never silently resubmitted"};self.review.prepare(action,label,self.action_request(action,body));}}
   });}
   if !self.action_job.is_null(){ui.strong(format!("Action job {} · {} · {}",self.action_job["job_id"],self.action_job["action"],normalized_job(&self.action_job)));if self.action_job["reused"]==true{ui.label("The server reused an existing job.");}json_view(ui,&self.action_job);if normalized_job(&self.action_job)=="failed"&&!self.active(){if ui.button("Review explicit failed-action retry").clicked(){let action=self.action_job["action"].as_str().unwrap_or("");if ["backfill","verify","gate"].contains(&action)&&self.action_job["migration_id"]==self.migration_id(){let body=if action=="gate"{Some(self.action_job.get("payload").cloned().unwrap_or(json!({})))}else{None};self.review.prepare("retry_action","Explicitly retry this failed durable action",self.action_request(action,body));}}}}
   egui::CollapsingHeader::new("Recover an accepted job by ID").show(ui,|ui|{text_field(ui,"Migration ID",&mut self.pointer_migration);text_field(ui,"Action job ID",&mut self.pointer_job);if ui.add_enabled(!self.pointer_migration.is_empty()&&!self.pointer_job.is_empty(),egui::Button::new("Read and resume job observation")).clicked(){self.observing=true;self.job.request("load_job",api,ApiRequest::get(&["api","v1","knowledge","datasets",&self.bound,"embedding-migration",&self.pointer_migration,"jobs",&self.pointer_job]),runtime,ctx);}});
  });self.review.ui(ui,&mut self.job,api,runtime,ctx);if !self.result.is_null(){egui::CollapsingHeader::new("Last migration receipt").show(ui,|ui|json_view(ui,&self.result));}});
    }
}
