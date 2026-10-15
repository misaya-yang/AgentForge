use std::sync::Arc;

use codex_core::config::Config;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ToolPolicy;
use codex_protocol::ThreadId;
use codex_thread_store::ThreadStore;

/// Host-only options for one in-process `thread/start` request.
///
/// These values are deliberately not part of the public App Server protocol.
/// A multi-tenant host can reserve and authorize its durable root identity
/// before Codex starts the thread, avoiding a create-then-attach ownership gap.
#[derive(Clone, Debug)]
pub struct AppServerThreadStartOptions {
    reserved_thread_id: ThreadId,
    tool_policy: Option<ToolPolicy>,
}

impl AppServerThreadStartOptions {
    /// Creates host options for one pre-authorized root thread identity.
    pub fn new(reserved_thread_id: ThreadId) -> Self {
        Self {
            reserved_thread_id,
            tool_policy: None,
        }
    }

    /// Captures the authenticated host tool ceiling before the thread starts.
    pub fn with_tool_policy(mut self, tool_policy: ToolPolicy) -> Self {
        self.tool_policy = Some(tool_policy);
        self
    }

    pub(crate) fn tool_policy(&self) -> Option<&ToolPolicy> {
        self.tool_policy.as_ref()
    }

    pub(crate) fn reserved_thread_id(&self) -> ThreadId {
        self.reserved_thread_id
    }
}

/// Host-only tool ceiling supplied again for a `thread/resume` request.
#[derive(Clone, Debug)]
pub struct AppServerThreadResumeOptions {
    tool_policy: ToolPolicy,
}

impl AppServerThreadResumeOptions {
    pub fn new(tool_policy: ToolPolicy) -> Self {
        Self { tool_policy }
    }

    pub(crate) fn tool_policy(&self) -> &ToolPolicy {
        &self.tool_policy
    }
}

/// Host-only options for one in-process `turn/start` request.
///
/// Reserving the turn id lets a multi-tenant host persist its immutable run
/// snapshot and signed provider lease before Codex can issue the first model
/// request. The option is never exposed through the public App Server schema.
#[derive(Clone, Debug)]
pub struct AppServerTurnStartOptions {
    reserved_turn_id: String,
    recovery: bool,
}

impl AppServerTurnStartOptions {
    /// Creates host options for one pre-authorized platform run identity.
    pub fn new(reserved_turn_id: String) -> Self {
        Self {
            reserved_turn_id,
            recovery: false,
        }
    }

    /// Recovers an authorized unfinished turn; never adds new user input.
    pub fn recovery(reserved_turn_id: String) -> Self {
        Self {
            reserved_turn_id,
            recovery: true,
        }
    }

    pub(crate) fn is_recovery(&self) -> bool {
        self.recovery
    }

    pub(crate) fn reserved_turn_id(&self) -> &str {
        &self.reserved_turn_id
    }
}

/// Installs host-owned contributors into Codex's existing extension registry.
///
/// Implementations must only register platform capabilities. They must not run
/// a second model/tool loop or mutate app-server protocol state outside the
/// contributor contracts exposed by [`ExtensionRegistryBuilder`].
pub trait AppServerExtensionInstaller: Send + Sync {
    /// Adds host-owned contributors after the built-in Codex extensions.
    fn install(&self, builder: &mut ExtensionRegistryBuilder<Config>);
}

/// Process-scoped dependencies supplied by an in-process app-server host.
///
/// The default leaves Codex behavior unchanged. A host can replace durable
/// thread persistence and add typed extensions while continuing to use the
/// upstream Thread/Turn/Item lifecycle, tool loop, compaction, and interrupt
/// implementation.
#[derive(Clone, Default)]
pub struct AppServerHostRuntime {
    thread_store: Option<Arc<dyn ThreadStore>>,
    extension_installer: Option<Arc<dyn AppServerExtensionInstaller>>,
}

impl AppServerHostRuntime {
    /// Creates an empty host runtime override.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses the supplied process-scoped thread store for every thread.
    pub fn with_thread_store(mut self, thread_store: Arc<dyn ThreadStore>) -> Self {
        self.thread_store = Some(thread_store);
        self
    }

    /// Adds host contributors to Codex's immutable extension registry.
    pub fn with_extension_installer(
        mut self,
        extension_installer: Arc<dyn AppServerExtensionInstaller>,
    ) -> Self {
        self.extension_installer = Some(extension_installer);
        self
    }

    pub(crate) fn thread_store(&self) -> Option<Arc<dyn ThreadStore>> {
        self.thread_store.clone()
    }

    pub(crate) fn extension_installer(&self) -> Option<Arc<dyn AppServerExtensionInstaller>> {
        self.extension_installer.clone()
    }
}
