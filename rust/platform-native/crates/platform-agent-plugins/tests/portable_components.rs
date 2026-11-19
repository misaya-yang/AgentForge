use ai_platform_agent_plugins::*;
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "native-portable-plugin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plugin.json"),json!({"$schema":PLUGIN_SCHEMA_V1,"name":"portable-plugin","version":"1.2","keywords":["test"],"author":{"name":"Author"}}).to_string()).unwrap();
        Self(root)
    }
    fn mcp(&self, servers: serde_json::Value) {
        fs::write(
            self.0.join("mcp.json"),
            json!({"$schema":MCP_SCHEMA_V1,"mcpServers":servers}).to_string(),
        )
        .unwrap();
    }
    fn load(&self) -> LoadedAgentPlugin {
        load_agent_plugin(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn skills_are_instruction_artifacts_without_tool_grants() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("skills/research")).unwrap();
    fs::write(f.0.join("skills/research/SKILL.md"),"---\nname: research\ndescription: Find evidence.\nallowed-tools: fs_read admin_destroy\nmetadata:\n  discovery_delegate_tool: web_search\n---\nRead source carefully.\n").unwrap();
    let p = f.load();
    assert!(p.diagnostics.is_empty());
    let s = &p.skills[0];
    assert_eq!(s.entrypoint, "md://agent-plugin/portable-plugin/research");
    assert!(s.permissions.is_empty());
    assert_eq!(s.artifact_type, "agent_plugin_instruction");
    assert_eq!(s.instructions, "Read source carefully.");
    assert_eq!(s.config["declared_allowed_tools"], "fs_read admin_destroy");
    assert_eq!(s.version, "1.2");
    assert_eq!(s.source, "marketplace");
    assert!(!p.mcp_present);
}
#[test]
fn manifest_duplicate_keys_are_fatal_content_free() {
    let f = Fixture::new();
    fs::write(
        f.0.join("plugin.json"),
        format!(r#"{{"$schema":"{PLUGIN_SCHEMA_V1}","name":"one","name":"two"}}"#),
    )
    .unwrap();
    assert_eq!(
        load_agent_plugin(&f.0).unwrap_err().code,
        "AGENT_PLUGIN_JSON_DUPLICATE_KEY"
    );
}
#[test]
fn mcp_supports_declared_transports_and_isolates_invalid_servers() {
    let f = Fixture::new();
    f.mcp(json!({"remote":{"type":"streamable-http","url":"https://example.invalid/mcp","headers":{"Authorization":"Bearer fixture"}},"local":{"type":"stdio","command":"node","args":["server.js"],"env":{"MODE":"safe"}},"legacy":{"type":"sse","url":"https://example.invalid/sse"},"unsafe":{"type":"streamable-http","url":"http://example.invalid/mcp"}}));
    let p = f.load();
    assert!(p.mcp_present);
    assert_eq!(p.mcp_servers.len(), 2);
    assert_eq!(p.diagnostics.len(), 2);
    assert!(
        p.diagnostics
            .iter()
            .any(|d| d.code == "AGENT_PLUGIN_MCP_TRANSPORT_UNSUPPORTED")
    );
    let payload = p.validation_payload();
    assert!(!payload.to_string().contains("Bearer fixture"));
    assert_eq!(payload["client_mode"], "skills+stdio+streamable-http-mcp");
}
#[test]
fn remote_url_security_and_duplicate_headers() {
    for (url, headers) in [
        ("http://public.example/mcp", json!({})),
        ("https://user:password@example.invalid/mcp", json!({})),
        ("https://example.invalid/mcp#fragment", json!({})),
        (
            "https://example.invalid/mcp",
            json!({"Authorization":"a","authorization":"b"}),
        ),
        (
            "https://example.invalid/mcp",
            json!({"X-Test":"bad\r\nheader"}),
        ),
    ] {
        let f = Fixture::new();
        f.mcp(json!({"invalid":{"type":"streamable-http","url":url,"headers":headers}}));
        let p = f.load();
        assert!(p.mcp_servers.is_empty());
        assert_eq!(p.diagnostics[0].code, "AGENT_PLUGIN_MCP_SERVER_INVALID");
    }
}
#[test]
fn http_loopback_and_https_allowed() {
    for url in [
        "http://localhost/mcp",
        "http://127.0.0.1:8080/mcp",
        "http://[::1]/mcp",
        "https://example.invalid/mcp",
    ] {
        let f = Fixture::new();
        f.mcp(json!({"server":{"type":"streamable-http","url":url}}));
        assert_eq!(f.load().mcp_servers.len(), 1, "{url}");
    }
}
#[test]
fn stdio_never_accepts_external_path_or_reserved_environment() {
    for server in [
        json!({"type":"stdio","command":"/bin/sh"}),
        json!({"type":"stdio","command":"../runner"}),
        json!({"type":"stdio","command":"node","env":{"PLUGIN_ROOT":"forged"}}),
        json!({"type":"stdio","command":"node","cwd":"/tmp"}),
        json!({"type":"stdio","command":"node","cwd":"${PLUGIN_DATA}/../escape"}),
    ] {
        let f = Fixture::new();
        f.mcp(json!({"invalid":server}));
        assert!(f.load().mcp_servers.is_empty());
    }
}
#[test]
fn malformed_mcp_is_nonfatal_component_diagnostic() {
    let f = Fixture::new();
    fs::write(
        f.0.join("mcp.json"),
        "{\"mcpServers\":{},\"mcpServers\":{}}",
    )
    .unwrap();
    let p = f.load();
    assert!(p.mcp_present);
    assert!(p.mcp_servers.is_empty());
    assert_eq!(p.diagnostics[0].code, "AGENT_PLUGIN_MCP_JSON_INVALID");
}
