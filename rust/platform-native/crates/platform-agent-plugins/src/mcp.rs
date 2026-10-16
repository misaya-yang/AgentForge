use crate::*;
use serde_json::json;
use std::{collections::BTreeSet, net::IpAddr, path::Path};
pub const MCP_SCHEMA_V1: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
#[derive(Debug, Clone, Serialize)]
pub struct AgentPluginMCPParseResult {
    pub present: bool,
    pub servers: Vec<AgentPluginMCPServer>,
    pub diagnostics: Vec<AgentPluginDiagnostic>,
}
fn failed(code: impl Into<String>) -> AgentPluginMCPParseResult {
    AgentPluginMCPParseResult {
        present: true,
        servers: vec![],
        diagnostics: vec![AgentPluginDiagnostic::new(code, "mcp.json")],
    }
}
fn contained(root: &Path, path: &str, directory: bool) -> bool {
    root.join(path.strip_prefix("./").unwrap_or(path))
        .canonicalize()
        .is_ok_and(|p| p.starts_with(root) && if directory { p.is_dir() } else { p.is_file() })
}
fn cwd(root: &Path, s: &str) -> bool {
    if s.contains('\0') {
        return false;
    }
    if s.is_empty() {
        return true;
    }
    if s.starts_with("./") {
        return contained(root, s, true);
    }
    if s == "${PLUGIN_ROOT}" || s.starts_with("${PLUGIN_ROOT}/") {
        let relative = s
            .trim_start_matches("${PLUGIN_ROOT}")
            .trim_start_matches('/');
        return relative.is_empty() || contained(root, relative, true);
    }
    if s == "${PLUGIN_DATA}" || s.starts_with("${PLUGIN_DATA}/") {
        return !s
            .trim_start_matches("${PLUGIN_DATA}")
            .split('/')
            .any(|p| p == "..");
    }
    false
}
fn strings(value: &Value) -> Option<BTreeMap<String, String>> {
    value
        .as_object()?
        .iter()
        .map(|(k, v)| Some((k.clone(), v.as_str()?.into())))
        .collect()
}
fn parse(root: &Path, name: &str, raw: &Value) -> Option<AgentPluginMCPServer> {
    let map = raw.as_object()?;
    let transport = raw["type"].as_str()?;
    let mut server = AgentPluginMCPServer {
        name: name.into(),
        transport: transport.into(),
        url: String::new(),
        headers: BTreeMap::new(),
        command: String::new(),
        args: vec![],
        env: BTreeMap::new(),
        cwd: String::new(),
    };
    match transport {
        "stdio" => {
            if map
                .keys()
                .any(|k| !["type", "command", "args", "env", "cwd"].contains(&k.as_str()))
            {
                return None;
            }
            let command = raw["command"].as_str()?;
            if command.is_empty() || command.contains('\0') {
                return None;
            }
            if command.starts_with("./") {
                if !contained(root, command, false) {
                    return None;
                }
            } else if command.contains(['/', '\\']) || [".", ".."].contains(&command) {
                return None;
            }
            server.command = command.into();
            let empty = json!([]);
            server.args = raw
                .get("args")
                .unwrap_or(&empty)
                .as_array()?
                .iter()
                .map(|v| v.as_str().filter(|s| !s.contains('\0')).map(str::to_owned))
                .collect::<Option<_>>()?;
            let empty = json!({});
            server.env = strings(raw.get("env").unwrap_or(&empty))?;
            let re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
            if server.env.iter().any(|(k, v)| {
                !re.is_match(k)
                    || ["PLUGIN_ROOT", "PLUGIN_DATA"].contains(&k.as_str())
                    || v.contains('\0')
            }) {
                return None;
            }
            server.cwd = raw.get("cwd").unwrap_or(&json!("")).as_str()?.into();
            if !cwd(root, &server.cwd) {
                return None;
            }
        }
        "streamable-http" => {
            if map
                .keys()
                .any(|k| !["type", "url", "headers"].contains(&k.as_str()))
            {
                return None;
            }
            let raw_url = raw["url"].as_str()?;
            // url::Url normalizes an empty userinfo marker; source rejects it too.
            if raw_url
                .split_once("://")
                .map(|(_, tail)| {
                    tail.split(['/', '?', '#'])
                        .next()
                        .unwrap_or("")
                        .contains('@')
                })
                .unwrap_or(false)
            {
                return None;
            }
            let url = url::Url::parse(raw_url).ok()?;
            let host = url.host_str()?;
            if !["http", "https"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some_and(|s| !s.is_empty())
                || url.port() == Some(0)
            {
                return None;
            }
            if url.scheme() == "http"
                && host.to_lowercase() != "localhost"
                && !host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            {
                return None;
            }
            server.url = raw_url.into();
            if !raw["headers"].is_null() {
                server.headers = strings(&raw["headers"])?;
            }
            let re = regex::Regex::new(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$").unwrap();
            let mut names = BTreeSet::new();
            if server.headers.iter().any(|(k, v)| {
                !re.is_match(k)
                    || !names.insert(k.to_lowercase())
                    || v.chars().any(|c| (c < ' ' && c != '\t') || c == '\u{7f}')
            }) {
                return None;
            }
        }
        _ => return None,
    };
    Some(server)
}
pub fn load_agent_plugin_mcp(root: &Path) -> AgentPluginMCPParseResult {
    let path = root.join("mcp.json");
    if !path.exists() {
        return AgentPluginMCPParseResult {
            present: false,
            servers: vec![],
            diagnostics: vec![],
        };
    }
    let text = match loader::read(&path, root, 65536, "AGENT_PLUGIN_MCP_COMPONENT_INVALID") {
        Ok(t) => t,
        Err(e) => return failed(e.code),
    };
    let raw = match unique::json(&text) {
        Ok(v) => v,
        Err(_) => return failed("AGENT_PLUGIN_MCP_JSON_INVALID"),
    };
    if !raw
        .as_object()
        .is_some_and(|o| o.len() == 2 && o.contains_key("$schema") && o.contains_key("mcpServers"))
    {
        return failed("AGENT_PLUGIN_MCP_TOP_LEVEL_INVALID");
    }
    if raw["$schema"] != MCP_SCHEMA_V1 {
        return failed("AGENT_PLUGIN_MCP_SCHEMA_UNSUPPORTED");
    }
    let Some(servers) = raw["mcpServers"].as_object() else {
        return failed("AGENT_PLUGIN_MCP_TOP_LEVEL_INVALID");
    };
    let mut result = AgentPluginMCPParseResult {
        present: true,
        servers: vec![],
        diagnostics: vec![],
    };
    for (name, raw) in servers {
        match parse(root, name, raw) {
            Some(server) => result.servers.push(server),
            None => result.diagnostics.push(AgentPluginDiagnostic::new(
                if raw["type"] == "sse" {
                    "AGENT_PLUGIN_MCP_TRANSPORT_UNSUPPORTED"
                } else {
                    "AGENT_PLUGIN_MCP_SERVER_INVALID"
                },
                name,
            )),
        }
    }
    result
}
