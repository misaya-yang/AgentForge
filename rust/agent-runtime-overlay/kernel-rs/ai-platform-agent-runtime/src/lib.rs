//! AI Platform's production host for the Agent agent kernel.
//!
//! This crate deliberately owns no model/tool loop. It supplies platform
//! persistence and typed extensions to the in-process App Server and exposes
//! the upstream request/event lifecycle to the private service boundary.

mod approval_control;
pub mod capability_execution;
pub mod capability_plane;
pub mod capability_worker;
pub mod http_service;
mod platform_lifecycle;
mod postgres_store;
pub mod readonly_capabilities;
mod readonly_extension;
pub mod tool_lifecycle;
mod tool_policy;
mod trace_context;
mod v1_projector;

pub use postgres_store::PlatformThreadIdentity;
pub use postgres_store::PostgresThreadStore;
pub use readonly_capabilities::CapabilityAllowlistEntry;
pub use readonly_capabilities::CapabilityDescriptor;
pub use readonly_capabilities::CapabilityItem;
pub use readonly_capabilities::ConnectorBinding;
pub use readonly_capabilities::MetadataFilter;
pub use readonly_capabilities::ReadonlyCapabilityError;
pub use readonly_capabilities::ReadonlyItemKind;
pub use readonly_capabilities::RuntimeCapabilityScope;
pub use readonly_capabilities::discover_readonly;
pub use readonly_capabilities::project_artifact;
pub use readonly_capabilities::project_attachment;
pub use readonly_capabilities::project_citation;
pub use readonly_capabilities::project_item;
pub use readonly_capabilities::project_knowledge;
pub use readonly_capabilities::project_office_read;
pub use readonly_capabilities::render_turn_input;
pub use readonly_capabilities::resolve_dynamic_capability;
pub use readonly_capabilities::resolve_dynamic_tool;
pub use readonly_capabilities::validate_platform_config;
pub use readonly_extension::ReadonlyTurnContext;
pub use readonly_extension::install_readonly_contributors;
pub use v1_projector::ASSISTANT_TURN_CONTRACT_V1;
pub use v1_projector::AssistantTurnEventV1;
pub use v1_projector::SequencedAssistantTurnEventV1;
pub use v1_projector::V1ProjectionContext;
pub use v1_projector::project_server_notification;
pub use v1_projector::server_notification_thread_id;

use std::io::Result as IoResult;
use std::sync::Arc;

use codex_app_server::host_runtime::AppServerExtensionInstaller;
use codex_app_server::host_runtime::AppServerHostRuntime;
use codex_app_server::host_runtime::AppServerThreadStartOptions;
use codex_app_server::host_runtime::AppServerTurnStartOptions;
use codex_app_server::in_process::InProcessServerEvent;
use codex_app_server_client::InProcessAppServerClient;
use codex_app_server_client::InProcessAppServerRequestHandle;
use codex_app_server_client::InProcessClientStartArgs;
use codex_app_server_client::RequestResult;
use codex_app_server_protocol::ClientRequest;
use codex_core::config::Config;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_protocol::ThreadId;
use codex_thread_store::ThreadStore;

/// Composite installer for platform-owned Agent extension contributors.
///
/// Contributors are installed in declaration order after Codex's built-ins.
/// Tool/schema collisions are still rejected by the upstream registry.
#[derive(Default)]
pub struct AiPlatformExtensionRegistry {
    installers: Vec<Arc<dyn AppServerExtensionInstaller>>,
    platform_store: Option<Arc<PostgresThreadStore>>,
}

impl AiPlatformExtensionRegistry {
    /// Creates an empty platform extension set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one typed extension installer.
    pub fn with_installer(mut self, installer: Arc<dyn AppServerExtensionInstaller>) -> Self {
        self.installers.push(installer);
        self
    }

    /// Binds platform lifecycle hooks to the durable Runtime Item Store.
    /// Hosts without a PostgreSQL-backed Runtime can omit this for isolated
    /// upstream unit tests.
    pub fn with_thread_store(mut self, store: Arc<PostgresThreadStore>) -> Self {
        self.platform_store = Some(store);
        self
    }
}

impl AppServerExtensionInstaller for AiPlatformExtensionRegistry {
    fn install(&self, builder: &mut ExtensionRegistryBuilder<Config>) {
        readonly_extension::install_readonly_contributors(builder);
        if let Some(store) = &self.platform_store {
            let lifecycle = Arc::new(platform_lifecycle::PlatformLifecycleContributor::new(
                Arc::clone(store),
            ));
            let thread_lifecycle: Arc<dyn codex_extension_api::ThreadLifecycleContributor<Config>> =
                lifecycle.clone();
            let turn_lifecycle: Arc<dyn codex_extension_api::TurnLifecycleContributor> =
                lifecycle.clone();
            builder.thread_lifecycle_contributor(thread_lifecycle);
            builder.turn_lifecycle_contributor(turn_lifecycle);
            builder.tool_lifecycle_contributor(lifecycle);
        }
        for installer in &self.installers {
            installer.install(builder);
        }
    }
}

/// Running Agent App Server kernel owned by the AI Platform runtime process.
pub struct AgentKernel {
    client: InProcessAppServerClient,
}

impl AgentKernel {
    /// Starts one process-scoped Agent kernel with mandatory platform storage.
    pub async fn start(
        args: InProcessClientStartArgs,
        thread_store: Arc<dyn ThreadStore>,
        extensions: AiPlatformExtensionRegistry,
    ) -> IoResult<Self> {
        let host_runtime = AppServerHostRuntime::new()
            .with_thread_store(thread_store)
            .with_extension_installer(Arc::new(extensions));
        let client = InProcessAppServerClient::start_with_host(args, host_runtime).await?;
        Ok(Self { client })
    }

    /// Returns a cloneable bounded request handle for HTTP request workers.
    pub fn request_handle(&self) -> InProcessAppServerRequestHandle {
        self.client.request_handle()
    }

    /// Sends a typed App Server request through the upstream kernel.
    pub async fn request(&self, request: ClientRequest) -> IoResult<RequestResult> {
        self.client.request(request).await
    }

    /// Starts a root thread whose platform ownership was persisted first.
    pub async fn request_thread_start(
        &self,
        request: ClientRequest,
        reserved_thread_id: ThreadId,
    ) -> IoResult<RequestResult> {
        self.client
            .request_thread_start(
                request,
                AppServerThreadStartOptions::new(reserved_thread_id),
            )
            .await
    }

    /// Starts one turn whose platform run id was persisted before dispatch.
    pub async fn request_turn_start(
        &self,
        request: ClientRequest,
        reserved_turn_id: String,
    ) -> IoResult<RequestResult> {
        self.client
            .request_turn_start(request, AppServerTurnStartOptions::new(reserved_turn_id))
            .await
    }

    /// Receives the next ordered App Server event.
    pub async fn next_event(&mut self) -> Option<InProcessServerEvent> {
        self.client.next_event().await
    }

    /// Rejects a server request that the platform host cannot satisfy.
    pub async fn reject_server_request(
        &self,
        request_id: codex_app_server_protocol::RequestId,
        error: codex_app_server_protocol::JSONRPCErrorError,
    ) -> IoResult<()> {
        self.client.reject_server_request(request_id, error).await
    }

    /// Resolves a server request after a platform capability-plane call.
    pub async fn respond_server_request(
        &self,
        request_id: codex_app_server_protocol::RequestId,
        result: serde_json::Value,
    ) -> IoResult<()> {
        self.client.resolve_server_request(request_id, result).await
    }

    /// Flushes and shuts down the upstream kernel.
    pub async fn shutdown(self) -> IoResult<()> {
        self.client.shutdown().await
    }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "postgres_store_contract_tests.rs"]
mod postgres_store_contract_tests;

#[cfg(test)]
#[path = "v1_projector_tests.rs"]
mod v1_projector_tests;
