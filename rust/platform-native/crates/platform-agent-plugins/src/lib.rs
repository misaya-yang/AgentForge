//! Agent Plugins 1.0.0 inert directory loader. Declarations never grant runtime authority.
mod agents;
mod loader;
mod mcp;
mod unique;
mod yaml11;
pub use loader::load_agent_plugin;
pub use mcp::{AgentPluginMCPParseResult, MCP_SCHEMA_V1, load_agent_plugin_mcp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
pub const PLUGIN_SCHEMA_V1: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPluginLoadError {
    pub code: String,
}
impl std::fmt::Display for AgentPluginLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}
impl std::error::Error for AgentPluginLoadError {}
impl From<&str> for AgentPluginLoadError {
    fn from(code: &str) -> Self {
        Self { code: code.into() }
    }
}
impl From<String> for AgentPluginLoadError {
    fn from(code: String) -> Self {
        Self { code }
    }
}
type Result<T> = std::result::Result<T, AgentPluginLoadError>;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPluginDiagnostic {
    pub code: String,
    pub component: String,
    pub severity: String,
}
impl AgentPluginDiagnostic {
    fn new(code: impl Into<String>, component: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            component: component.into(),
            severity: "warning".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: BTreeMap<String, String>,
    pub homepage: String,
    pub repository: String,
    pub license: String,
    pub keywords: Vec<String>,
    pub extensions: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPluginMCPServer {
    pub name: String,
    pub transport: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPluginAgentLimits {
    pub max_turns: i64,
    pub max_tool_calls: i64,
    pub max_tokens: i64,
    pub timeout_seconds: i64,
    pub idle_timeout_seconds: i64,
}
impl AgentPluginAgentLimits {
    pub fn initial_max_turns(&self) -> i64 {
        self.max_turns
    }
    pub fn initial_max_tool_calls(&self) -> i64 {
        self.max_tool_calls
    }
    pub fn recommended_max_tokens(&self) -> i64 {
        self.max_tokens
    }
    pub fn initial_timeout_seconds(&self) -> i64 {
        self.timeout_seconds
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPluginAgent {
    pub qualified_id: String,
    pub plugin: String,
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub base_type: String,
    pub allowed_tools: Vec<String>,
    pub allowed_tool_categories: Vec<String>,
    pub limits: AgentPluginAgentLimits,
    pub source_namespace: String,
    pub source_path: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub title: String,
    pub description: String,
    pub entrypoint: String,
    pub summary: String,
    pub version: String,
    pub tags: Vec<String>,
    pub permissions: Vec<String>,
    pub enabled: bool,
    pub skill_id: Option<String>,
    pub version_id: Option<String>,
    pub content_hash: Option<String>,
    pub artifact_type: String,
    pub instructions: String,
    pub trigger: Option<Value>,
    pub config: Value,
    pub source: String,
    pub tool_schema: Option<Value>,
    pub max_context_tokens: u32,
    pub author: String,
    pub generated: bool,
    pub lifecycle_status: String,
    pub review: Value,
    pub evaluation: Value,
    pub rollback: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedAgentPlugin {
    pub root: PathBuf,
    pub manifest: AgentPluginManifest,
    pub skills: Vec<SkillManifest>,
    pub mcp_servers: Vec<AgentPluginMCPServer>,
    pub diagnostics: Vec<AgentPluginDiagnostic>,
    pub mcp_present: bool,
    pub agents: Vec<AgentPluginAgent>,
}
impl LoadedAgentPlugin {
    pub fn validation_payload(&self) -> Value {
        serde_json::json!({"valid":true,"spec":"1.0.0","client_mode":"skills+stdio+streamable-http-mcp","plugin":self.manifest.name,"skills":self.skills.iter().map(|s|&s.name).collect::<Vec<_>>(),"mcp_present":self.mcp_present,"mcp_supported":!self.mcp_servers.is_empty(),"mcp_servers":self.mcp_servers.iter().map(|s|if s.transport=="streamable-http"{serde_json::json!({"name":s.name,"transport":s.transport,"url":s.url})}else{serde_json::json!({"name":s.name,"transport":s.transport,"command":s.command,"args":s.args})}).collect::<Vec<_>>(),"diagnostics":self.diagnostics})
    }
}
impl AgentPluginAgent {
    pub fn max_turns(&self) -> i64 {
        self.limits.max_turns
    }
    pub fn max_tool_calls(&self) -> i64 {
        self.limits.max_tool_calls
    }
    pub fn max_tokens(&self) -> i64 {
        self.limits.max_tokens
    }
    pub fn timeout_seconds(&self) -> i64 {
        self.limits.timeout_seconds
    }
    pub fn content_sha256(&self) -> &str {
        &self.sha256
    }
}
