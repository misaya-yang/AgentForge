use crate::*;
use serde_json::json;
use std::{fs, path::Path};
pub(crate) fn read(path: &Path, root: &Path, limit: u64, code: &str) -> Result<String> {
    let resolved = path
        .canonicalize()
        .map_err(|_| AgentPluginLoadError::from(code))?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(code.into());
    }
    let meta = fs::metadata(&resolved).map_err(|_| AgentPluginLoadError::from(code))?;
    if meta.len() > limit {
        return Err(format!("{code}_TOO_LARGE").into());
    }
    let bytes = fs::read(resolved).map_err(|_| AgentPluginLoadError::from(code))?;
    String::from_utf8(bytes)
        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
        .map_err(|_| format!("{code}_ENCODING").into())
}
pub(crate) fn valid_name(name: &str, dots: bool) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || dots && b == b'.')
        && !name.contains("--")
        && !name.contains("..")
}
fn manifest(raw: Value) -> Result<(AgentPluginManifest, Vec<AgentPluginDiagnostic>)> {
    let map = raw.as_object().ok_or("AGENT_PLUGIN_MANIFEST_NOT_OBJECT")?;
    let fields = [
        "$schema",
        "name",
        "version",
        "description",
        "author",
        "homepage",
        "repository",
        "license",
        "keywords",
        "extensions",
    ];
    let mut unknown: Vec<_> = map
        .keys()
        .filter(|k| !fields.contains(&k.as_str()))
        .collect();
    unknown.sort();
    let mut diagnostics = unknown
        .into_iter()
        .map(|k| AgentPluginDiagnostic::new("AGENT_PLUGIN_MANIFEST_UNKNOWN_FIELD", k))
        .collect::<Vec<_>>();
    if raw["$schema"] != PLUGIN_SCHEMA_V1 {
        return Err("AGENT_PLUGIN_SCHEMA_UNSUPPORTED".into());
    }
    let name = raw["name"]
        .as_str()
        .filter(|v| valid_name(v, true))
        .ok_or("AGENT_PLUGIN_NAME_INVALID")?;
    let string = |key: &str| -> Result<String> {
        match raw.get(key) {
            None => Ok(String::new()),
            Some(v) => v
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "AGENT_PLUGIN_MANIFEST_FIELD_INVALID".into()),
        }
    };
    let empty = json!({});
    let author = raw
        .get("author")
        .unwrap_or(&empty)
        .as_object()
        .ok_or("AGENT_PLUGIN_AUTHOR_INVALID")?;
    if author
        .iter()
        .any(|(k, v)| !["name", "email", "url"].contains(&k.as_str()) || !v.is_string())
    {
        return Err("AGENT_PLUGIN_AUTHOR_INVALID".into());
    }
    let empty_list = json!([]);
    let keywords = raw
        .get("keywords")
        .unwrap_or(&empty_list)
        .as_array()
        .ok_or("AGENT_PLUGIN_KEYWORDS_INVALID")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("AGENT_PLUGIN_KEYWORDS_INVALID")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let extensions = match raw.get("extensions").unwrap_or(&empty).as_object() {
        Some(v) => {
            if v.values().any(|v| !v.is_object()) {
                return Err("AGENT_PLUGIN_EXTENSIONS_INVALID".into());
            }
            v.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
        None => {
            diagnostics.push(AgentPluginDiagnostic::new(
                "AGENT_PLUGIN_EXTENSIONS_IGNORED",
                "extensions",
            ));
            BTreeMap::new()
        }
    };
    Ok((
        AgentPluginManifest {
            name: name.into(),
            version: string("version")?,
            description: string("description")?,
            homepage: string("homepage")?,
            repository: string("repository")?,
            license: string("license")?,
            author: author
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().into()))
                .collect(),
            keywords,
            extensions,
        },
        diagnostics,
    ))
}
pub(crate) fn frontmatter(content: &str, kind: &str) -> Result<(String, String)> {
    if content.contains('\0') {
        return Err(format!("AGENT_PLUGIN_{kind}_INVALID").into());
    }
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let regex = regex::Regex::new(r"(?s)^---\s*\n(.*?)\n---\s*\n?(.*)").unwrap();
    let captures = regex.captures(&normalized).ok_or_else(|| {
        AgentPluginLoadError::from(format!("AGENT_PLUGIN_{kind}_FRONTMATTER_REQUIRED"))
    })?;
    Ok((captures[1].to_owned(), captures[2].trim().to_owned()))
}
fn skill(content: &str, directory: &str, plugin: &AgentPluginManifest) -> Result<SkillManifest> {
    let (front, instructions) = frontmatter(content, "SKILL")?;
    let normalized = crate::yaml11::normalize(&front)
        .map_err(|_| AgentPluginLoadError::from("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID"))?;
    let metadata: Value = serde_yaml::from_str(&normalized)
        .map_err(|_| AgentPluginLoadError::from("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID"))?;
    if !metadata.is_object() {
        return Err("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID".into());
    }
    let name = metadata["name"]
        .as_str()
        .filter(|s| valid_name(s, false) && *s == directory)
        .ok_or("AGENT_PLUGIN_SKILL_NAME_INVALID")?;
    let description = metadata["description"]
        .as_str()
        .filter(|s| (1..=1024).contains(&s.chars().count()))
        .ok_or("AGENT_PLUGIN_SKILL_DESCRIPTION_INVALID")?;
    for key in ["license", "compatibility", "allowed-tools"] {
        if metadata
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID".into());
        }
    }
    if metadata["compatibility"]
        .as_str()
        .is_some_and(|s| !(1..=500).contains(&s.chars().count()))
    {
        return Err("AGENT_PLUGIN_SKILL_COMPATIBILITY_INVALID".into());
    }
    let empty = json!({});
    let custom = metadata
        .get("metadata")
        .unwrap_or(&empty)
        .as_object()
        .ok_or("AGENT_PLUGIN_SKILL_METADATA_INVALID")?;
    if custom.values().any(|v| !v.is_string()) {
        return Err("AGENT_PLUGIN_SKILL_METADATA_INVALID".into());
    }
    let delegate = custom
        .get("discovery_delegate_tool")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !delegate.is_empty()
        && !regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_.:-]{0,127}$")
            .unwrap()
            .is_match(delegate)
    {
        return Err("AGENT_PLUGIN_SKILL_METADATA_INVALID".into());
    }
    Ok(SkillManifest {
        name: name.into(),
        title: name.into(),
        description: description.into(),
        entrypoint: format!("md://agent-plugin/{}/{name}", plugin.name),
        summary: description.chars().take(180).collect(),
        version: if plugin.version.is_empty() {
            custom
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("1.0.0")
                .into()
        } else {
            plugin.version.clone()
        },
        tags: plugin.keywords.clone(),
        permissions: vec![],
        enabled: true,
        skill_id: None,
        version_id: None,
        content_hash: None,
        artifact_type: "agent_plugin_instruction".into(),
        instructions,
        trigger: None,
        config: json!({"agent_plugin":plugin.name,"declared_allowed_tools":metadata.get("allowed-tools").cloned().unwrap_or(json!("")),"discovery_delegate_tool":delegate}),
        source: "marketplace".into(),
        tool_schema: None,
        max_context_tokens: 2000,
        author: plugin.author.get("name").cloned().unwrap_or_default(),
        generated: false,
        lifecycle_status: "active".into(),
        review: json!({}),
        evaluation: json!({}),
        rollback: json!({}),
    })
}
pub fn load_agent_plugin(path: impl AsRef<Path>) -> Result<LoadedAgentPlugin> {
    let path = path.as_ref();
    let expanded = path
        .to_str()
        .and_then(|s| s.strip_prefix("~/"))
        .and_then(|s| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(s)));
    let root = expanded
        .as_deref()
        .unwrap_or(path)
        .canonicalize()
        .map_err(|_| AgentPluginLoadError::from("AGENT_PLUGIN_ROOT_INVALID"))?;
    if !root.is_dir() {
        return Err("AGENT_PLUGIN_ROOT_INVALID".into());
    }
    let text = read(
        &root.join("plugin.json"),
        &root,
        65536,
        "AGENT_PLUGIN_MANIFEST_INVALID",
    )?;
    let raw = crate::unique::json(&text).map_err(|e| {
        AgentPluginLoadError::from(if e.contains("DUPLICATE_KEY") {
            "AGENT_PLUGIN_JSON_DUPLICATE_KEY"
        } else {
            "AGENT_PLUGIN_MANIFEST_JSON_INVALID"
        })
    })?;
    let (manifest, mut diagnostics) = manifest(raw)?;
    let mut skills = vec![];
    let skills_path = root.join("skills");
    if skills_path.exists() {
        let resolved = skills_path.canonicalize().unwrap_or(skills_path);
        if !resolved.starts_with(&root) || !resolved.is_dir() {
            diagnostics.push(AgentPluginDiagnostic::new(
                "AGENT_PLUGIN_SKILLS_COMPONENT_INVALID",
                "skills",
            ));
        } else {
            let mut children = fs::read_dir(resolved)
                .map_err(|_| AgentPluginLoadError::from("AGENT_PLUGIN_SKILLS_COMPONENT_INVALID"))?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            children.sort_by_key(|c| c.file_name());
            for child in children {
                let Ok(resolved) = child.path().canonicalize() else {
                    continue;
                };
                let name = child.file_name().to_string_lossy().into_owned();
                if !resolved.starts_with(&root) || !resolved.is_dir() {
                    diagnostics.push(AgentPluginDiagnostic::new(
                        "AGENT_PLUGIN_SKILL_PATH_INVALID",
                        name,
                    ));
                    continue;
                }
                let file = resolved.join("SKILL.md");
                if !file.exists() {
                    continue;
                }
                match read(&file, &root, 50000, "AGENT_PLUGIN_SKILL_INVALID")
                    .and_then(|v| skill(&v, &name, &manifest))
                {
                    Ok(s) => skills.push(s),
                    Err(e) => diagnostics.push(AgentPluginDiagnostic::new(e.code, name)),
                }
            }
        }
    }
    let (agents, agent_diagnostics) = crate::agents::load(&root, &manifest);
    diagnostics.extend(agent_diagnostics);
    let mcp = load_agent_plugin_mcp(&root);
    diagnostics.extend(mcp.diagnostics);
    Ok(LoadedAgentPlugin {
        root,
        manifest,
        skills,
        agents,
        mcp_servers: mcp.servers,
        diagnostics,
        mcp_present: mcp.present,
    })
}
