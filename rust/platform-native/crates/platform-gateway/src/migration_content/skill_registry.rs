//! Source-equivalent in-memory and optional immutable-artifact skill registry.
//! Scoped cache entries never mutate the process-wide platform name registry.
use super::{
    ContentError,
    skills::{SkillRepository, manifest_from_artifact, review_required, serialize_user_skill_md},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn validate_manifest(m: &Value) -> Vec<String> {
    ["name", "entrypoint", "description"]
        .into_iter()
        .filter(|key| {
            text(m, key)
                .trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
                .is_empty()
        })
        .map(|key| format!("{key} is required"))
        .collect()
}
fn fail(message: impl Into<String>) -> ContentError {
    ContentError::new(422, json!(message.into()))
}
fn safe(mut m: Value) -> Value {
    if review_required(&m) {
        m["enabled"] = json!(false);
        m["lifecycle_status"] = json!("proposed");
    } else if m["generated"] == true && m["enabled"] == true {
        m["lifecycle_status"] = json!("active");
    }
    m
}
#[derive(Clone, Debug)]
pub struct SkillSelection {
    pub skill: Value,
    pub score: f64,
}
#[derive(Default)]
pub struct SkillRegistry {
    pub database: Option<Arc<SkillRepository>>,
    skills: BTreeMap<String, Value>,
    scoped_skills: BTreeMap<(String, String, String), Value>,
}
impl SkillRegistry {
    pub fn new(database: Option<Arc<SkillRepository>>) -> Self {
        Self {
            database,
            ..Self::default()
        }
    }
    pub fn register(&mut self, manifest: Value) -> Result<(), ContentError> {
        let m = safe(manifest);
        let errors = validate_manifest(&m);
        if !errors.is_empty() {
            return Err(fail(format!(
                "Invalid skill manifest: {}",
                errors.join("; ")
            )));
        }
        self.skills.insert(text(&m, "name").into(), m);
        Ok(())
    }
    pub fn unregister(&mut self, name: &str) -> bool {
        self.skills.remove(name).is_some()
    }
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.skills.get(name)
    }
    pub fn get_scoped(
        &self,
        name: &str,
        tenant: &str,
        user: &str,
        version: Option<&str>,
    ) -> Option<&Value> {
        self.scoped_skills
            .iter()
            .filter(|((t, u, v), m)| {
                t == tenant
                    && u == user
                    && text(m, "name") == name
                    && version.is_none_or(|wanted| v == wanted)
            })
            .map(|(_, m)| m)
            .max_by_key(|m| text(m, "version_id"))
            .or_else(|| {
                if version.is_none() {
                    self.skills.get(name)
                } else {
                    None
                }
            })
    }
    pub fn fork_runtime_view(&self) -> Self {
        Self {
            database: self.database.clone(),
            skills: self.skills.clone(),
            scoped_skills: BTreeMap::new(),
        }
    }
    pub fn register_scoped(
        &mut self,
        manifest: Value,
        tenant: &str,
        user: &str,
    ) -> Result<(), ContentError> {
        let m = safe(manifest);
        if !validate_manifest(&m).is_empty() || text(&m, "version_id").is_empty() {
            return Err(fail("Scoped Skill requires a valid immutable version"));
        }
        self.scoped_skills.insert(
            (tenant.into(), user.into(), text(&m, "version_id").into()),
            m,
        );
        Ok(())
    }
    pub fn list(
        &self,
        enabled_only: bool,
        allowed_names: Option<&BTreeSet<String>>,
        scope: Option<(&str, &str)>,
        allowed_versions: Option<&BTreeMap<String, String>>,
    ) -> Vec<Value> {
        let mut by_id: BTreeMap<(String, String), &Value> = self
            .skills
            .values()
            .map(|m| {
                (
                    (
                        text(m, "name").into(),
                        if text(m, "version_id").is_empty() {
                            "platform"
                        } else {
                            text(m, "version_id")
                        }
                        .into(),
                    ),
                    m,
                )
            })
            .collect();
        if let Some((tenant, user)) = scope {
            for ((t, u, v), m) in &self.scoped_skills {
                if t == tenant && u == user {
                    by_id.insert((text(m, "name").into(), v.clone()), m);
                }
            }
        }
        let mut out = by_id
            .into_values()
            .filter(|m| {
                (!enabled_only || m["enabled"].as_bool().unwrap_or(true))
                    && allowed_names.is_none_or(|n| n.contains(text(m, "name")))
                    && allowed_versions.is_none_or(|v| {
                        v.get(text(m, "name"))
                            .is_some_and(|v| v == text(m, "version_id"))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        out.sort_by(|a, b| {
            (text(a, "name"), text(a, "version_id")).cmp(&(text(b, "name"), text(b, "version_id")))
        });
        out
    }
    pub fn metadata_for_prompt(
        &self,
        enabled_only: bool,
        allowed_names: Option<&BTreeSet<String>>,
        scope: Option<(&str, &str)>,
        allowed_versions: Option<&BTreeMap<String, String>>,
    ) -> Vec<Value> {
        self.list(enabled_only,allowed_names,scope,allowed_versions).iter().map(|m|json!({"name":m["name"],"title":m["title"],"summary":if text(m,"summary").is_empty(){text(m,"description")}else{text(m,"summary")},"version":m.get("version").cloned().unwrap_or(json!("1.0.0")),"tags":m.get("tags").cloned().unwrap_or(json!([]))})).collect()
    }
    pub fn select_for_query(
        &self,
        query: &str,
        max_skills: Option<i64>,
        allowed_names: Option<&BTreeSet<String>>,
        scope: Option<(&str, &str)>,
        allowed_versions: Option<&BTreeMap<String, String>>,
    ) -> Vec<SkillSelection> {
        let normalized = query.to_lowercase();
        let terms = normalized
            .split(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        if terms.is_empty() {
            return vec![];
        }
        let mut scored = vec![];
        for m in self.list(true, allowed_names, scope, allowed_versions) {
            let tags = m["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let bag = format!("{} {} {}", text(&m, "title"), text(&m, "description"), tags)
                .to_lowercase();
            let score = terms.iter().filter(|t| bag.contains(**t)).count() as f64;
            if score > 0.0 {
                scored.push(SkillSelection { skill: m, score })
            }
        }
        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        if let Some(n) = max_skills {
            scored.truncate(n.max(0) as usize)
        }
        scored
    }
    pub async fn load_from_database(
        &mut self,
        tenant: &str,
        user: &str,
        allowed_names: Option<&BTreeSet<String>>,
    ) -> Result<usize, ContentError> {
        let Some(repo) = self.database.clone() else {
            return Ok(0);
        };
        if allowed_names.is_some_and(BTreeSet::is_empty) {
            return Ok(0);
        }
        let rows = repo.list_for_actor(tenant, user, true).await?;
        let mut loaded = 0;
        for row in rows {
            if allowed_names.is_some_and(|names| !names.contains(text(&row, "name"))) {
                continue;
            }
            let m = manifest_from_artifact(&row)?;
            if !text(&m, "version_id").is_empty() {
                self.register_scoped(m, tenant, user)?;
                loaded += 1
            }
        }
        Ok(loaded)
    }
    pub async fn load_versions_from_database(
        &mut self,
        tenant: &str,
        user: &str,
        allowed_versions: &BTreeMap<String, String>,
    ) -> Result<usize, ContentError> {
        let Some(repo) = self.database.clone() else {
            return Ok(0);
        };
        if allowed_versions.is_empty() {
            return Ok(0);
        }
        let manifests = repo
            .load_versions(tenant, user, &allowed_versions.values().cloned().collect())
            .await?;
        let mut loaded = 0;
        for m in manifests {
            if !allowed_versions
                .get(text(&m, "name"))
                .is_some_and(|v| !v.is_empty() && v == text(&m, "version_id"))
            {
                return Err(fail("Resolved Skill name/version binding mismatch"));
            }
            self.register_scoped(m, tenant, user)?;
            loaded += 1
        }
        Ok(loaded)
    }
    pub async fn save_manifest(
        &mut self,
        tenant: &str,
        user: &str,
        manifest: Value,
        created_by: &str,
    ) -> Result<String, ContentError> {
        let mut m = safe(manifest);
        let Some(repo) = self.database.clone() else {
            self.register(m)?;
            return Ok("in-memory".into());
        };
        if text(&m, "source") != "user" {
            return Err(fail(
                "Only user Skills can be persisted as tenant artifacts",
            ));
        }
        m["entrypoint"] = json!("db://pending/pending");
        m["source"] = json!("user");
        for k in ["skill_id", "version_id", "content_hash"] {
            m[k] = Value::Null;
        }
        m["artifact_type"] = json!("tenant_instruction");
        let content = serialize_user_skill_md(&m)?;
        let record = repo
            .create_version(tenant, user, &content, m, created_by)
            .await?;
        self.register_scoped(manifest_from_artifact(&record)?, tenant, user)?;
        Ok(text(&record, "skill_id").into())
    }
}
