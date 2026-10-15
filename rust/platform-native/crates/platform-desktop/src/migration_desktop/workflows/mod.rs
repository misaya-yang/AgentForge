pub mod admin;
pub mod agent;
pub mod agent_bindings;
pub mod attachments;
pub mod common;
pub mod connectors;
pub mod conversation;
pub mod documents;
pub mod embedding;
pub mod eval_store;
pub mod governance;
pub mod knowledge;
pub mod knowledge_eval;
pub mod knowledge_sources;
mod knowledge_source_sync;
mod knowledge_connections;
pub mod mcp;
pub mod model_capabilities;
pub mod models;
pub mod node_events;
pub mod provider;
mod sdk_service_editor;
pub mod release;
pub mod retrieval_presets;
pub mod scheduled_notifications;
pub mod scheduled_tasks;
pub mod skills;
pub mod source_preview;
pub mod stream_panel;
pub mod tasks;
pub mod usage;
pub mod version_preview;
#[derive(Default)]
pub struct Workflows {
    pub tasks_updates: bool,
    pub scheduled_notifications: scheduled_notifications::ScheduledNotifications,
    pub scheduled_tasks: scheduled_tasks::ScheduledTaskEditor,
    pub tasks: tasks::TaskExplorer,
    pub node_events: node_events::NodeEvents,
    pub agent: agent::AgentEditor,
    pub conversation: conversation::ConversationEditor,
    pub knowledge: knowledge::KnowledgeEditor,
    pub provider: provider::ProviderEditor,
    pub sdk_services: sdk_service_editor::SdkServiceEditor,
    pub admin: admin::AdminEditor,
    pub connectors: connectors::ConnectorEditor,
    pub mcp: mcp::McpEditor,
    pub models: models::ModelEditor,
    pub skills: skills::SkillEditor,
    pub usage: usage::UsageExplorer,
}

mod provider_catalog;
