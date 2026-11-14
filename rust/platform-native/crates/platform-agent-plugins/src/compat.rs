//! Readers for Codex, Claude, marketplace, and Agent Skills declarations.
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatEntry {
    pub kind: String,
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSkill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
}

pub fn validate_agent_skill(markdown: &str) -> Result<AgentSkill, &'static str> {
    let front = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))
        .ok_or("skill frontmatter")?;
    let end = front.find("\n---").ok_or("skill frontmatter")?;
    let yaml: SkillFront = serde_yaml::from_str(&front[..end]).map_err(|_| "skill frontmatter")?;
    validate_skill_name(&yaml.name)?;
    let description = yaml.description.trim();
    if description.is_empty() || description.chars().count() > 1024 {
        return Err("skill description");
    }
    Ok(AgentSkill {
        name: yaml.name,
        description: description.to_owned(),
        allowed_tools: yaml.allowed_tools.unwrap_or_default(),
    })
}

fn validate_skill_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
    {
        return Err("skill name");
    }
    Ok(())
}

#[derive(Deserialize)]
struct SkillFront {
    name: String,
    description: String,
    #[serde(default, rename = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
}

pub fn discover_compat(root: &Path) -> Vec<CompatEntry> {
    let mut entries = Vec::new();
    for relative in [
        ".codex-plugin/plugin.json",
        ".claude-plugin/plugin.json",
        ".agents/plugins/marketplace.json",
        ".mcp.json",
    ] {
        let path = root.join(relative);
        if let Ok(bytes) = fs::read_to_string(&path) {
            entries.extend(parse_compat(relative, &bytes));
        }
    }
    if let Ok(skills) = fs::read_dir(root.join("skills")) {
        for skill in skills.flatten() {
            let file = skill.path().join("SKILL.md");
            if let Ok(markdown) = fs::read_to_string(file) {
                if let Ok(parsed) = validate_agent_skill(&markdown) {
                    entries.push(CompatEntry {
                        kind: "skill".into(),
                        name: parsed.name,
                        description: parsed.description,
                        source: "skill".into(),
                    });
                }
            }
        }
    }
    entries
}

fn parse_compat(source: &str, bytes: &str) -> Vec<CompatEntry> {
    let Ok(value) = serde_json::from_str::<Value>(bytes) else {
        return Vec::new();
    };
    if source.ends_with("marketplace.json") {
        return value
            .get("plugins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|plugin| named(plugin, "plugin", source))
            .collect();
    }
    if source.ends_with(".mcp.json") {
        return value
            .get("mcpServers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, server)| CompatEntry {
                kind: "mcp_server".into(),
                name: name.clone(),
                description: server
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP server")
                    .to_owned(),
                source: source.into(),
            })
            .collect();
    }
    named(&value, "plugin", source).into_iter().collect()
}

fn named(value: &Value, kind: &str, source: &str) -> Option<CompatEntry> {
    Some(CompatEntry {
        kind: kind.into(),
        name: value.get("name")?.as_str()?.to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        source: source.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_reads_codex_claude_marketplace_mcp_and_skill() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../agent-plugins/spaces");
        let skill = fs::read_to_string(root.join("skills/space-writer/SKILL.md")).unwrap();
        let parsed = validate_agent_skill(&skill).unwrap();
        assert_eq!(parsed.name, "space-writer");
        assert!(parsed.allowed_tools.iter().any(|tool| tool == "space_create_page"));
        let temp = std::env::temp_dir().join(format!("agentforge-compat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join(".codex-plugin")).unwrap();
        fs::create_dir_all(temp.join(".claude-plugin")).unwrap();
        fs::create_dir_all(temp.join(".agents/plugins")).unwrap();
        fs::write(
            temp.join(".codex-plugin/plugin.json"),
            r#"{"name":"codex-demo","description":"Codex plugin"}"#,
        )
        .unwrap();
        fs::write(
            temp.join(".claude-plugin/plugin.json"),
            r#"{"name":"claude-demo","description":"Claude plugin"}"#,
        )
        .unwrap();
        fs::write(
            temp.join(".agents/plugins/marketplace.json"),
            r#"{"plugins":[{"name":"market-demo","description":"Marketplace plugin"}]}"#,
        )
        .unwrap();
        fs::write(
            temp.join(".mcp.json"),
            r#"{"mcpServers":{"docs":{"description":"Docs server"}}}"#,
        )
        .unwrap();
        let found = discover_compat(&temp);
        assert!(found.iter().any(|entry| entry.name == "codex-demo" && entry.kind == "plugin"));
        assert!(found.iter().any(|entry| entry.name == "claude-demo"));
        assert!(found.iter().any(|entry| entry.name == "market-demo"));
        assert!(found.iter().any(|entry| entry.name == "docs" && entry.kind == "mcp_server"));
        let _ = fs::remove_dir_all(temp);
        assert!(validate_agent_skill("---\nname: Bad Name\ndescription: x\n---\n").is_err());
    }
}
