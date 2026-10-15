use super::{
    api::{ApiError, ApiRequest, DesktopApi},
    catalog::{Operation, Section},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::mpsc::{self, Receiver, Sender},
};
#[derive(Clone, Debug)]
pub enum LoadState {
    Idle,
    Loading,
    Ready,
    Failed(String),
    OutcomeUnknown,
}
#[derive(Clone)]
pub struct ResourcePage {
    pub data: Value,
    pub state: LoadState,
    pub selected_id: String,
    pub detail: Value,
    pub detail_view: String,
    pub filter: String,
    pub revision: u64,
}
impl Default for ResourcePage {
    fn default() -> Self {
        Self {
            data: Value::Null,
            state: LoadState::Idle,
            selected_id: String::new(),
            detail: Value::Null,
            detail_view: String::new(),
            filter: String::new(),
            revision: 0,
        }
    }
}
pub struct PendingMutation {
    pub section: Section,
    pub label: String,
    pub request: ApiRequest,
    pub body_text: String,
    pub confirmation: bool,
}
struct Completion {
    epoch: u64,
    request_id: u64,
    section: Section,
    detail: Option<String>,
    mutation: bool,
    result: Result<Value, ApiError>,
}
pub struct DesktopWorkspace {
    pub workflows: super::workflows::Workflows,
    pub api: Option<DesktopApi>,
    pub section: Section,
    pub pages: BTreeMap<Section, ResourcePage>,
    pub status: String,
    pub expired: bool,
    pub pending: Option<PendingMutation>,
    pub pairing: Option<Value>,
    pub mutation_result: Option<Value>,
    pub mutation_busy: bool,
    pub epoch: u64,
    next_request: u64,
    active_reads: BTreeMap<Section, u64>,
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    reads: BTreeMap<Section, tokio::task::AbortHandle>,
}
impl Default for DesktopWorkspace {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            workflows: super::workflows::Workflows::default(),
            api: None,
            section: Section::Conversation,
            pages: Section::ALL
                .iter()
                .map(|s| (*s, ResourcePage::default()))
                .collect(),
            status: String::new(),
            expired: false,
            pending: None,
            pairing: None,
            mutation_result: None,
            mutation_busy: false,
            epoch: 0,
            next_request: 0,
            active_reads: BTreeMap::new(),
            sender,
            receiver,
            reads: BTreeMap::new(),
        }
    }
}
impl DesktopWorkspace {
    pub fn new(api: DesktopApi) -> Self {
        Self {
            api: Some(api),
            ..Self::default()
        }
    }
    pub fn set_api(&mut self, api: DesktopApi) {
        self.disconnect();
        self.api = Some(api);
        self.expired = false;
    }
    pub fn disconnect(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        for (_, task) in std::mem::take(&mut self.reads) {
            task.abort();
        }
        self.active_reads.clear();
        self.api = None;
        self.workflows = super::workflows::Workflows::default();
        self.pending = None;
        self.pairing = None;
        self.mutation_result = None;
        self.mutation_busy = false;
        self.pages = Section::ALL
            .iter()
            .map(|s| (*s, ResourcePage::default()))
            .collect();
        self.status.clear();
    }
    pub fn page(&self) -> &ResourcePage {
        self.pages.get(&self.section).expect("section initialized")
    }
    pub fn page_mut(&mut self) -> &mut ResourcePage {
        self.pages
            .get_mut(&self.section)
            .expect("section initialized")
    }
    pub fn select(&mut self, section: Section) {
        if self.section != section && self.section == Section::Services {
            self.workflows.sdk_services.leave();
        }
        if self.section != section {
            self.workflows.knowledge.sources.close_sync_reviews();
            if self.section == Section::ScheduledTasks {
                self.workflows.scheduled_tasks.close_observation();
            }
            self.pending = None;
            self.mutation_result = None;
            self.section = section;
        }
    }
    pub fn choose_item(&mut self, id: String) {
        if self.page().selected_id == id {
            return;
        }
        self.cancel_read();
        let page = self.page_mut();
        page.selected_id = id;
        page.detail = Value::Null;
        page.detail_view.clear();
        self.pending = None;
    }

    pub fn read(
        &mut self,
        request: ApiRequest,
        detail: Option<String>,
        runtime: &tokio::runtime::Runtime,
        ctx: &eframe::egui::Context,
    ) {
        if request.is_mutation() {
            self.status = "Read operation cannot submit a change".into();
            return;
        }
        self.launch(request, detail, false, runtime, ctx);
    }
    pub fn refresh(&mut self, runtime: &tokio::runtime::Runtime, ctx: &eframe::egui::Context) {
        self.read(self.section.request(), None, runtime, ctx);
    }
    pub fn prepare(&mut self, label: impl Into<String>, request: ApiRequest) {
        if self.mutation_busy {
            return;
        }
        self.pending = Some(PendingMutation {
            section: self.section,
            label: label.into(),
            body_text: request
                .body
                .as_ref()
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                .unwrap_or_default(),
            request,
            confirmation: false,
        });
    }
    pub fn prepare_operation(&mut self, operation: Operation) {
        self.prepare(operation.label, operation.request());
    }
    pub fn submit(&mut self, runtime: &tokio::runtime::Runtime, ctx: &eframe::egui::Context) {
        if self.mutation_busy {
            return;
        }
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        if !pending.confirmation || pending.section != self.section {
            self.pending = Some(pending);
            return;
        }
        if pending.request.body.is_some() {
            match serde_json::from_str::<Value>(&pending.body_text) {
                Ok(body) if body.is_object() || body.is_array() => {
                    pending.request.body = Some(body)
                }
                _ => {
                    self.status = "Enter a valid JSON request body".into();
                    self.pending = Some(pending);
                    return;
                }
            }
        }
        self.launch(pending.request, None, true, runtime, ctx);
    }
    fn launch(
        &mut self,
        request: ApiRequest,
        detail: Option<String>,
        mutation: bool,
        runtime: &tokio::runtime::Runtime,
        ctx: &eframe::egui::Context,
    ) {
        let Some(api) = self.api.clone() else {
            self.status = "Sign in first".into();
            return;
        };
        let section = self.section;
        self.next_request = self.next_request.wrapping_add(1);
        let request_id = self.next_request;
        let epoch = self.epoch;
        if mutation {
            self.mutation_busy = true;
            self.mutation_result = None;
            self.status = "Submitting one request".into();
        } else {
            if let Some(task) = self.reads.remove(&section) {
                task.abort();
            }
            self.active_reads.insert(section, request_id);
            self.pages.get_mut(&section).unwrap().state = LoadState::Loading;
        }
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let task = runtime.spawn(async move {
            let result = api.execute(request).await;
            let _ = sender.send(Completion {
                epoch,
                request_id,
                section,
                detail,
                mutation,
                result,
            });
            ctx.request_repaint();
        });
        if !mutation {
            self.reads.insert(section, task.abort_handle());
        }
    }
    pub fn poll(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            if message.epoch != self.epoch {
                continue;
            }
            if !message.mutation
                && self.active_reads.get(&message.section) != Some(&message.request_id)
            {
                continue;
            }
            if message.mutation {
                self.mutation_busy = false;
            } else {
                self.active_reads.remove(&message.section);
                self.reads.remove(&message.section);
            }
            match message.result {
                Ok(value) => {
                    if message.mutation {
                        if message.section == Section::LocalNodes
                            && value.get("challenge_id").is_some()
                        {
                            self.pairing = Some(value.clone());
                        }
                        self.mutation_result = Some(value);
                        self.status = "Change accepted. Refresh to read the current state.".into();
                    } else {
                        let page = self.pages.get_mut(&message.section).unwrap();
                        if let Some(view) = message.detail {
                            page.detail_view = view;
                            page.detail = value;
                        } else {
                            page.data = value;
                        }
                        page.state = LoadState::Ready;
                        page.revision = page.revision.wrapping_add(1);
                    }
                }
                Err(ApiError::Status(401)) => {
                    self.disconnect();
                    self.expired = true;
                    self.status = "Session expired. Sign in again.".into();
                }
                Err(error) => {
                    let page = self.pages.get_mut(&message.section).unwrap();
                    if message.mutation
                        && matches!(
                            error,
                            ApiError::Transport
                                | ApiError::Response
                                | ApiError::Status(408 | 500..=599)
                        )
                    {
                        page.state = LoadState::OutcomeUnknown;
                        self.status="Change outcome is unknown. Refresh or inspect the affected record before trying again.".into();
                    } else {
                        page.state = LoadState::Failed(error.to_string());
                        self.status = error.to_string();
                    }
                }
            }
        }
    }
    pub fn cancel_read(&mut self) {
        if let Some(task) = self.reads.remove(&self.section) {
            task.abort();
        }
        self.active_reads.remove(&self.section);
        self.page_mut().state = LoadState::Idle;
    }
}
/// Merge only strictly increasing event cursors and deduplicate exact event IDs.
pub fn append_local_events(
    existing: &mut Vec<Value>,
    incoming: Vec<Value>,
    after: u64,
) -> Result<u64, ApiError> {
    let mut cursor = after;
    let mut seen: HashSet<String> = existing
        .iter()
        .filter_map(|v| v["event_id"].as_str().map(str::to_owned))
        .collect();
    let mut staged = Vec::new();
    for event in incoming {
        let sequence = event["sequence"].as_u64().ok_or(ApiError::Response)?;
        let id = event["event_id"].as_str().ok_or(ApiError::Response)?;
        if seen.contains(id) {
            continue;
        }
        if sequence <= cursor {
            return Err(ApiError::Response);
        }
        cursor = sequence;
        seen.insert(id.into());
        staged.push(event);
    }
    existing.extend(staged);
    Ok(cursor)
}
