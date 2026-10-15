//! Canonical console destinations from web/src/api, without a parallel Agent loop.
use super::api::{ApiError, ApiRequest};
use reqwest::Method;
use serde_json::Value;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Conversation,
    Capabilities,
    LocalNodes,
    Sessions,
    Memory,
    Skills,
    Connectors,
    Mcp,
    Agents,
    Knowledge,
    Models,
    Providers,
    Services,
    Tasks,
    ScheduledTasks,
    Usage,
    Users,
    Roles,
    ApiKeys,
    Settings,
    Architecture,
    Evaluations,
}
impl Section {
    pub const ALL: &'static [Self] = &[
        Self::Conversation,
        Self::Capabilities,
        Self::LocalNodes,
        Self::Sessions,
        Self::Memory,
        Self::Skills,
        Self::Connectors,
        Self::Mcp,
        Self::Agents,
        Self::Knowledge,
        Self::Models,
        Self::Providers,
        Self::Services,
        Self::Tasks,
        Self::ScheduledTasks,
        Self::Usage,
        Self::Users,
        Self::Roles,
        Self::ApiKeys,
        Self::Settings,
        Self::Architecture,
        Self::Evaluations,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Capabilities => "Capabilities",
            Self::LocalNodes => "Local Nodes",
            Self::Sessions => "Sessions",
            Self::Memory => "Assistant memory",
            Self::Skills => "Skills",
            Self::Connectors => "Connectors",
            Self::Mcp => "MCP servers",
            Self::Agents => "Agents",
            Self::Knowledge => "Knowledge",
            Self::Models => "Models",
            Self::Providers => "Providers",
            Self::Services => "Services",
            Self::Tasks => "Tasks",
            Self::ScheduledTasks => "Scheduled tasks",
            Self::Usage => "Usage",
            Self::Users => "Users",
            Self::Roles => "Roles",
            Self::ApiKeys => "API keys",
            Self::Settings => "Settings",
            Self::Architecture => "Architecture",
            Self::Evaluations => "Evaluations",
        }
    }
    pub fn request(self) -> ApiRequest {
        let path = match self {
            Self::Conversation => vec!["sessions"],
            Self::Capabilities => vec!["assistant", "tools"],
            Self::LocalNodes => vec!["local-nodes"],
            Self::Sessions => vec!["sessions"],
            Self::Memory => vec!["assistant", "memory"],
            Self::Skills => vec!["skills"],
            Self::Connectors => vec!["connectors", "available"],
            Self::Mcp => vec!["mcp", "servers"],
            Self::Agents => vec!["agents"],
            Self::Knowledge => vec!["knowledge", "datasets"],
            Self::Models => vec!["models"],
            Self::Providers => vec!["providers"],
            Self::Services => vec!["services"],
            Self::Tasks => vec!["tasks"],
            Self::ScheduledTasks => vec!["scheduled-tasks"],
            Self::Usage => vec!["usage", "summary"],
            Self::Users => vec!["users"],
            Self::Roles => vec!["roles"],
            Self::ApiKeys => vec!["api-keys"],
            Self::Settings => vec!["config", "status"],
            Self::Architecture => vec!["admin", "architecture-status"],
            Self::Evaluations => vec!["eval", "datasets"],
        };
        let mut segments = vec!["api", "v1"];
        segments.extend(path);
        let request = ApiRequest::get(&segments);
        match self {
            Self::Skills => request.query("enabled_only", false),
            Self::Models => request.query("include_disabled", true),
            _ => request,
        }
    }
    pub fn identity_fields(self) -> &'static [&'static str] {
        match self {
            Self::Capabilities => &["name", "id"],
            Self::LocalNodes => &["device_id"],
            Self::Sessions => &["session_id"],
            Self::Memory => &["key"],
            Self::Skills => &["name"],
            Self::Connectors => &["provider"],
            Self::Mcp => &["server_id"],
            Self::Agents => &["agent_id", "id"],
            Self::Knowledge => &["id", "dataset_id"],
            Self::Models => &["id", "model_id"],
            Self::Providers => &["provider_id", "id"],
            Self::Services => &["service_id", "id"],
            Self::Tasks | Self::ScheduledTasks => &["task_id", "id"],
            Self::Users => &["user_id", "id"],
            Self::Roles => &["role_name", "role_id", "id"],
            Self::ApiKeys => &["key_id", "id"],
            _ => &["id"],
        }
    }
}
#[derive(Clone)]
pub struct Operation {
    pub label: &'static str,
    pub method: Method,
    pub path: Vec<String>,
    pub body: Option<Value>,
}
impl Operation {
    pub fn request(&self) -> ApiRequest {
        ApiRequest {
            method: self.method.clone(),
            segments: self.path.clone(),
            query: Vec::new(),
            body: self.body.clone(),
            headers: Vec::new(),
        }
    }
}
/// An explicit per-resource operation list. No arbitrary URL or verb is accepted.
pub fn detail_request(section: Section, id: &str, view: &str) -> Result<ApiRequest, ApiError> {
    let parts: Vec<&str> = match section {
        Section::LocalNodes
            if ["status", "capabilities", "doctor", "grants", "events"].contains(&view) =>
        {
            vec!["local-nodes", id, view]
        }
        Section::Sessions if ["history", "tools"].contains(&view) => {
            if view == "tools" {
                vec!["assistant", "sessions", id, "tools"]
            } else {
                vec!["sessions", id, "history"]
            }
        }
        Section::Knowledge
            if [
                "documents",
                "segments",
                "queries",
                "feedback",
                "config",
                "debug",
                "sources",
                "metadata-schema",
            ]
            .contains(&view) =>
        {
            vec!["knowledge", id, view]
        }
        Section::Agents
            if [
                "draft",
                "versions",
                "evals",
                "analytics",
                "audit-events",
                "governance",
                "publications",
                "publish-events",
            ]
            .contains(&view) =>
        {
            vec!["agents", id, view]
        }
        Section::Mcp if ["tools", "connections"].contains(&view) => {
            vec!["mcp", "servers", id, view]
        }
        Section::Connectors if view == "principals" => vec!["connectors", id, "principals"],
        Section::Tasks if view == "detail" => vec!["tasks", id],
        Section::Tasks if view == "result" => vec!["tasks", id, "result"],
        Section::Providers if view == "detail" => vec!["providers", id],
        Section::Services if view == "detail" => vec!["services", id],
        Section::Users if view == "detail" => vec!["users", id],
        _ => return Err(ApiError::Request),
    };
    let mut path = vec!["api", "v1"];
    path.extend(parts);
    let request = ApiRequest::get(&path);
    Ok(if section == Section::LocalNodes && view == "events" {
        request.query("after_sequence", 0).query("limit", 100)
    } else {
        request
    })
}
pub fn detail_views(section: Section) -> &'static [&'static str] {
    match section {
        Section::LocalNodes => &["status", "capabilities", "doctor", "grants", "events"],
        Section::Sessions => &["history", "tools"],
        Section::Knowledge => &[
            "documents",
            "segments",
            "queries",
            "feedback",
            "config",
            "debug",
            "sources",
            "metadata-schema",
        ],
        Section::Agents => &[
            "draft",
            "versions",
            "evals",
            "analytics",
            "audit-events",
            "governance",
            "publications",
            "publish-events",
        ],
        Section::Mcp => &["tools", "connections"],
        Section::Connectors => &["principals"],
        Section::Tasks => &["detail", "result"],
        Section::Providers | Section::Services | Section::Users => &["detail"],
        _ => &[],
    }
}
