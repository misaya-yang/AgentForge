//! AssistantClient module facade, corresponding to the published Python SDK surface.
use crate::{Client, Error, sdk_models::*, sdk_stream::SdkEvent};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
#[derive(Clone)]
pub struct AuthConfig {
    pub api_key: String,
    pub tenant_id: String,
    pub base_url: String,
    pub user_id: Option<String>,
    pub timeout: std::time::Duration,
    pub max_retries: u8,
}
impl AuthConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            tenant_id: String::new(),
            base_url: "http://127.0.0.1:8080".into(),
            user_id: None,
            timeout: std::time::Duration::from_secs(30),
            max_retries: 3,
        }
    }
}
pub struct AssistantClient {
    pub chat: ChatModule,
    pub sessions: SessionModule,
    pub knowledge: KnowledgeModule,
    pub images: ImageModule,
    pub artifacts: ArtifactModule,
    pub tools: ToolModule,
    pub skills: SkillModule,
    pub runs: RunModule,
    pub threads: ThreadModule,
    pub mcp: crate::mcp::McpManager,
}
macro_rules! module{($($name:ident),*)=>{$(#[derive(Clone)]pub struct $name{client:Client})*};}
module!(
    ChatModule,
    SessionModule,
    KnowledgeModule,
    ImageModule,
    ArtifactModule,
    ToolModule,
    SkillModule,
    RunModule,
    ThreadModule
);
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::Response)
}
impl AssistantClient {
    pub fn new(config: AuthConfig) -> Result<Self, Error> {
        Self::new_with_stream_limits(config, crate::StreamLimits::default())
    }
    pub fn new_with_stream_limits(
        config: AuthConfig,
        limits: crate::StreamLimits,
    ) -> Result<Self, Error> {
        let c = Client::new(&config.base_url)?
            .authenticated_api_key(config.api_key, config.tenant_id, config.user_id)?
            .with_transport_options(config.timeout, config.max_retries)?
            .with_stream_limits(limits);
        Ok(Self {
            chat: ChatModule { client: c.clone() },
            sessions: SessionModule { client: c.clone() },
            knowledge: KnowledgeModule { client: c.clone() },
            images: ImageModule { client: c.clone() },
            artifacts: ArtifactModule { client: c.clone() },
            tools: ToolModule { client: c.clone() },
            skills: SkillModule { client: c.clone() },
            runs: RunModule { client: c.clone() },
            threads: ThreadModule { client: c },
            mcp: Default::default(),
        })
    }
    pub async fn send(&self, request: ChatRequest) -> Result<ChatResponse, Error> {
        self.chat.send(request).await
    }
    pub async fn stream(
        &self,
        request: ChatRequest,
        sender: mpsc::Sender<Result<SdkEvent, Error>>,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        self.chat.stream(request, sender, cancel).await
    }
    pub async fn close(&mut self) -> Result<(), Error> {
        self.mcp.disconnect(None).await
    }
}
impl ChatModule {
    pub async fn send(&self, request: ChatRequest) -> Result<ChatResponse, Error> {
        self.client.send(request).await
    }
    pub async fn stream(
        &self,
        request: ChatRequest,
        sender: mpsc::Sender<Result<SdkEvent, Error>>,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        self.client
            .assistant_stream(request.to_value()?, sender, cancel)
            .await
    }
    pub async fn cancel(&self, task: &str, reason: Option<&str>) -> Result<Value, Error> {
        self.client.assistant_cancel_task(task, reason).await
    }
}
impl SessionModule {
    pub async fn create(&self, request: SessionCreateRequest) -> Result<Session, Error> {
        self.client.create_session(request).await
    }
    pub async fn list(&self, limit: u64, offset: u64) -> Result<Vec<Session>, Error> {
        self.client.list_sessions(limit, offset).await
    }
    pub async fn get(&self, id: &str) -> Result<Session, Error> {
        decode(self.client.assistant_session(id).await?)
    }
    pub async fn delete(&self, id: &str) -> Result<(), Error> {
        self.client.assistant_delete_session(id).await
    }
    pub async fn history(&self, id: &str, limit: u64) -> Result<Vec<Message>, Error> {
        self.client.session_history(id, limit).await
    }
}
impl KnowledgeModule {
    pub async fn list_datasets(&self) -> Result<Vec<Dataset>, Error> {
        let v = self.client.datasets().await?;
        decode(v.get("datasets").unwrap_or(&v).clone())
    }
    pub async fn get_source(
        &self,
        dataset: &str,
        document: &str,
        version: u32,
        hash: &str,
    ) -> Result<Value, Error> {
        self.client
            .immutable_source(dataset, document, version, hash)
            .await
    }
    pub async fn ask(
        &self,
        question: &str,
        datasets: Vec<String>,
        mut options: ChatRequest,
    ) -> Result<ChatResponse, Error> {
        if options.kb_mode.is_none() {
            options.kb_mode = Some("auto".into());
        }
        self.client.ask(question, datasets, options).await
    }
    pub async fn ask_stream(
        &self,
        question: &str,
        datasets: Vec<String>,
        mut options: ChatRequest,
        sender: mpsc::Sender<Result<SdkEvent, Error>>,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        options.message = question.into();
        options.kb_dataset_ids = Some(datasets);
        if options.kb_mode.is_none() {
            options.kb_mode = Some("auto".into());
        }
        self.client
            .assistant_stream(options.to_value()?, sender, cancel)
            .await
    }
}
impl ImageModule {
    pub async fn generate(&self, request: ImageRequest) -> Result<ImageResult, Error> {
        self.client.generate_image(request).await
    }
    pub async fn generate_async(&self, request: ImageRequest) -> Result<Value, Error> {
        self.client
            .assistant_image(
                serde_json::to_value(request).map_err(|_| Error::Request)?,
                true,
            )
            .await
    }
    pub async fn get_task(&self, id: &str) -> Result<Value, Error> {
        self.client.assistant_image_task(id).await
    }
    pub async fn cancel(&self, _id: &str, _reason: Option<&str>) -> Result<Value, Error> {
        Err(Error::Contract(
            "Image task cancellation is not supported by the Gateway",
        ))
    }
}
impl ArtifactModule {
    pub async fn create(&self, request: CreateArtifactRequest) -> Result<Artifact, Error> {
        decode(
            self.client
                .assistant_create_artifact(
                    serde_json::to_value(request).map_err(|_| Error::Request)?,
                )
                .await?,
        )
    }
    pub async fn get(&self, id: &str) -> Result<Artifact, Error> {
        decode(self.client.assistant_artifact(id).await?)
    }
    pub async fn download(&self, id: &str) -> Result<Vec<u8>, Error> {
        self.client
            .assistant_download_artifact(id, 64 * 1024 * 1024)
            .await
    }
    pub fn download_url(&self, id: &str) -> Result<url::Url, Error> {
        self.client.assistant_artifact_url(id)
    }
    pub async fn list_by_session(&self, id: &str) -> Result<Vec<Artifact>, Error> {
        let v = self.client.assistant_session_artifacts(id).await?;
        decode(v.get("artifacts").unwrap_or(&v).clone())
    }
    pub async fn delete(&self, id: &str) -> Result<(), Error> {
        self.client.assistant_delete_artifact(id).await
    }
}
impl SkillModule {
    pub async fn list(&self, enabled_only: bool) -> Result<Value, Error> {
        self.client.instruction_skills(enabled_only).await
    }
    pub async fn get(&self, name: &str) -> Result<Value, Error> {
        self.client.instruction_skill(name).await
    }
    pub async fn test(&self, name: &str, input: &str) -> Result<Value, Error> {
        self.client.test_instruction_skill(name, input).await
    }
}
impl ToolModule {
    pub async fn list_tools(&self, _limit: u64, _offset: u64) -> Result<Vec<ToolInfo>, Error> {
        let v = self.client.assistant_tools().await?;
        decode(v.get("tools").unwrap_or(&v).clone())
    }
    pub async fn get(&self, name: &str) -> Result<ToolInfo, Error> {
        decode(self.client.instruction_skill(name).await?)
    }
    pub async fn enable(&self, name: &str) -> Result<Value, Error> {
        self.client.set_instruction_enabled(name, true).await
    }
    pub async fn disable(&self, name: &str) -> Result<Value, Error> {
        self.client.set_instruction_enabled(name, false).await
    }
    pub async fn test(&self, name: &str, input: Option<Value>) -> Result<Value, Error> {
        let text = match input {
            None => String::new(),
            Some(Value::String(s)) => s,
            Some(v @ Value::Object(_)) => serde_json::to_string(&v).map_err(|_| Error::Request)?,
            _ => return Err(Error::Request),
        };
        self.client.test_instruction_skill(name, &text).await
    }
    pub async fn approve(&self, id: &str, approved: bool) -> Result<Value, Error> {
        self.client
            .post(
                &["api", "v1", "assistant", "approvals", id],
                json!({"approved":approved}),
            )
            .await
    }
}
impl RunModule {
    pub async fn get(&self, id: &str) -> Result<Value, Error> {
        self.client.assistant_run(id).await
    }
}
impl ThreadModule {
    pub async fn create(&self, session: Option<&str>) -> Result<Value, Error> {
        self.client.create_thread(session, None).await
    }
    pub async fn get(&self, id: &str) -> Result<Value, Error> {
        self.client.assistant_thread(id).await
    }
    pub async fn turn(&self, id: &str, body: Value) -> Result<Value, Error> {
        self.client.assistant_thread_turn(id, body).await
    }
    pub async fn interrupt(&self, thread: &str, turn: &str, reason: &str) -> Result<Value, Error> {
        self.client
            .post(
                &[
                    "api",
                    "v2",
                    "agent",
                    "threads",
                    thread,
                    "turns",
                    &format!("{turn}:interrupt"),
                ],
                json!({"reason":reason}),
            )
            .await
    }
    pub async fn get_approval(&self, thread: &str, id: &str) -> Result<Value, Error> {
        self.client.assistant_approval(thread, id).await
    }
    pub async fn decide_approval(
        &self,
        thread: &str,
        id: &str,
        approved: bool,
        reason: Option<&str>,
    ) -> Result<Value, Error> {
        self.client
            .assistant_decide_approval(thread, id, approved, reason)
            .await
    }
    pub async fn events(
        &self,
        thread: &str,
        turn: Option<&Value>,
        turn_id: Option<&str>,
        after_sequence: u64,
        limit: u32,
        sender: mpsc::Sender<Result<SdkEvent, Error>>,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        self.client
            .sdk_thread_events(thread, turn, turn_id, after_sequence, limit, sender, cancel)
            .await
    }
}
