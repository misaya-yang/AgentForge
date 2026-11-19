use crate::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
fn list(value: &Value, max: usize, categories: bool) -> Result<Vec<String>> {
    let values = value
        .as_array()
        .ok_or("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")?;
    let regex = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$").unwrap();
    let mut seen = BTreeSet::new();
    let mut result = vec![];
    if values.len() > max {
        return Err("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID".into());
    }
    for v in values {
        let s = v
            .as_str()
            .ok_or("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")?;
        if !seen.insert(s)
            || if categories {
                ![
                    "retrieval",
                    "generation",
                    "analysis",
                    "integration",
                    "utility",
                    "skill",
                    "mcp",
                ]
                .contains(&s)
            } else {
                !regex.is_match(s)
            }
        {
            return Err("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID".into());
        }
        result.push(s.into());
    }
    Ok(result)
}
fn limit(m: &Value, key: &str, legacy: &str, default: i64, min: i64, max: i64) -> Result<i64> {
    if key != legacy && m.get(key).is_some() && m.get(legacy).is_some() && m[key] != m[legacy] {
        return Err("AGENT_PLUGIN_AGENT_LIMIT_INVALID".into());
    }
    let value = m.get(key).or_else(|| m.get(legacy));
    match value {
        None => Ok(default),
        Some(v) => v
            .as_i64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| "AGENT_PLUGIN_AGENT_LIMIT_INVALID".into()),
    }
}
fn parse(
    content: &str,
    plugin: &AgentPluginManifest,
    namespace: &str,
    path: &str,
) -> Result<AgentPluginAgent> {
    let (front, instructions) = loader::frontmatter(content, "AGENT")?;
    let metadata = unique::yaml(&front).map_err(|e| {
        AgentPluginLoadError::from(if e.contains("DUPLICATE_KEY") {
            "AGENT_PLUGIN_AGENT_FRONTMATTER_DUPLICATE_KEY"
        } else {
            "AGENT_PLUGIN_AGENT_FRONTMATTER_INVALID"
        })
    })?;
    if !metadata.is_object() {
        return Err("AGENT_PLUGIN_AGENT_FRONTMATTER_INVALID".into());
    }
    let copilot = namespace == "com.github.awesome-copilot";
    let derived = if metadata["id"].is_null() && copilot {
        let name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let stem = name.strip_suffix(".md").unwrap_or(name);
        let stem = stem.strip_suffix(".agent").unwrap_or(stem).to_lowercase();
        regex::Regex::new("[^a-z0-9-]+")
            .unwrap()
            .replace_all(&stem, "-")
            .trim_matches('-')
            .to_owned()
    } else {
        String::new()
    };
    let id = metadata["id"].as_str().unwrap_or(&derived);
    if !loader::valid_name(id, false) {
        return Err("AGENT_PLUGIN_AGENT_ID_INVALID".into());
    }
    let name = metadata["name"]
        .as_str()
        .filter(|s| (1..=128).contains(&s.chars().count()))
        .ok_or("AGENT_PLUGIN_AGENT_NAME_INVALID")?;
    let description = metadata["description"]
        .as_str()
        .filter(|s| (1..=2048).contains(&s.chars().count()))
        .ok_or("AGENT_PLUGIN_AGENT_DESCRIPTION_INVALID")?;
    let base =
        metadata["base_type"]
            .as_str()
            .unwrap_or(if metadata["base_type"].is_null() && copilot {
                "explore"
            } else {
                ""
            });
    if !["explore", "task", "plan"].contains(&base) {
        return Err("AGENT_PLUGIN_AGENT_BASE_TYPE_INVALID".into());
    }
    let empty = json!([]);
    let tools = metadata
        .get("allowed_tools")
        .filter(|v| !v.is_null())
        .or_else(|| if copilot { metadata.get("tools") } else { None })
        .unwrap_or(&empty);
    let allowed_tools = list(tools, 64, false)?;
    let allowed_tool_categories = list(
        metadata.get("allowed_tool_categories").unwrap_or(&empty),
        7,
        true,
    )?;
    let limits = AgentPluginAgentLimits {
        max_turns: limit(&metadata, "initial_max_turns", "max_turns", 6, 1, 30)?,
        max_tool_calls: limit(
            &metadata,
            "initial_max_tool_calls",
            "max_tool_calls",
            10,
            1,
            60,
        )?,
        max_tokens: limit(
            &metadata,
            "recommended_max_tokens",
            "max_tokens",
            4096,
            256,
            8192,
        )?,
        timeout_seconds: limit(
            &metadata,
            "initial_timeout_seconds",
            "timeout_seconds",
            120,
            5,
            600,
        )?,
        idle_timeout_seconds: limit(
            &metadata,
            "idle_timeout_seconds",
            "idle_timeout_seconds",
            120,
            5,
            600,
        )?,
    };
    if instructions.is_empty() {
        return Err("AGENT_PLUGIN_AGENT_INSTRUCTIONS_REQUIRED".into());
    }
    Ok(AgentPluginAgent {
        qualified_id: format!("{}:{id}", plugin.name),
        plugin: plugin.name.clone(),
        id: id.into(),
        name: name.into(),
        description: description.into(),
        instructions,
        base_type: base.into(),
        allowed_tools,
        allowed_tool_categories,
        limits,
        source_namespace: namespace.into(),
        source_path: path.into(),
        sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
    })
}
fn read_reference(
    root: &Path,
    plugin: &AgentPluginManifest,
    namespace: &str,
    reference: &Value,
) -> Result<AgentPluginAgent> {
    let reference = reference
        .as_str()
        .ok_or("AGENT_PLUGIN_AGENT_PATH_INVALID")?;
    if reference.contains(['\0', '\\']) || !reference.starts_with("./agents/") {
        return Err("AGENT_PLUGIN_AGENT_PATH_INVALID".into());
    }
    let path = &reference[2..];
    let parts: Vec<_> = Path::new(path).components().collect();
    let name = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if parts.len() != 2 || !name.ends_with(".md") || [".md", "..md"].contains(&name) {
        return Err("AGENT_PLUGIN_AGENT_PATH_INVALID".into());
    }
    let normalized_path = Path::new(path).components().map(|p|p.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    let path = normalized_path.as_str();
    let candidate = root.join(path);
    if root.join("agents").is_symlink() || candidate.is_symlink() {
        return Err("AGENT_PLUGIN_AGENT_PATH_INVALID".into());
    }
    let content = loader::read(&candidate, root, 50000, "AGENT_PLUGIN_AGENT_INVALID")?;
    parse(&content, plugin, namespace, path)
}
pub(crate) fn load(
    root: &Path,
    plugin: &AgentPluginManifest,
) -> (Vec<AgentPluginAgent>, Vec<AgentPluginDiagnostic>) {
    let mut definitions = vec![];
    let mut diagnostics = vec![];
    for namespace in ["com.misaya.ai-gateway", "com.github.awesome-copilot"] {
        let Some(references) = plugin
            .extensions
            .get(namespace)
            .and_then(|v| v.get("agents"))
        else {
            continue;
        };
        let component = format!("extensions.{namespace}.agents");
        let Some(references) = references.as_array().filter(|v| v.len() <= 64) else {
            diagnostics.push(AgentPluginDiagnostic::new(
                "AGENT_PLUGIN_AGENTS_COMPONENT_INVALID",
                component,
            ));
            continue;
        };
        for (index, reference) in references.iter().enumerate() {
            match read_reference(root, plugin, namespace, reference) {
                Ok(def) => definitions.push(def),
                Err(e) => diagnostics.push(AgentPluginDiagnostic::new(
                    e.code,
                    format!("{component}[{index}]"),
                )),
            }
        }
    }
    let mut counts = BTreeMap::new();
    for definition in &definitions {
        *counts.entry(definition.id.clone()).or_insert(0) += 1;
    }
    for (id, count) in &counts {
        if *count > 1 {
            diagnostics.push(AgentPluginDiagnostic::new(
                "AGENT_PLUGIN_AGENT_ID_CONFLICT",
                id,
            ));
        }
    }
    definitions.retain(|d| counts[&d.id] == 1);
    (definitions, diagnostics)
}
