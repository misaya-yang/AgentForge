//! Gateway-owned frozen memory cleanup. Only SQL-proven source files and vector
//! point inventories are eligible; no path supplied by a deletion request is used.
use super::{AgentError, deletion_contract as contract, text};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
fn err(code: &str) -> AgentError {
    AgentError::code(503, code)
}
fn hash(s: &str) -> String {
    super::spec::hex(&Sha256::digest(s.as_bytes()))
}
fn legacy(s: &str) -> String {
    let mut value = String::new();
    let mut run = false;
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() || "_.-".contains(c) {
            value.push(c);
            run = false;
        } else if !run {
            value.push('_');
            run = true;
        }
    }
    let v = value.trim_matches(['.', '_']);
    if v.is_empty() {
        "unknown".into()
    } else {
        v.into()
    }
}
fn current(s: &str) -> String {
    format!(
        "~{}-{}",
        legacy(s).chars().take(80).collect::<String>(),
        hash(s.trim())
    )
}
#[derive(Clone)]
pub struct MemoryFiles {
    pub base: PathBuf,
}
impl MemoryFiles {
    fn roots(&self, tenant: &str, user: &str) -> [PathBuf; 2] {
        [
            self.base.join(current(tenant)).join(current(user)),
            self.base.join(legacy(tenant)).join(legacy(user)),
        ]
    }
    fn candidate(&self, tenant: &str, user: &str, stored: &str) -> Option<PathBuf> {
        let p = PathBuf::from(stored);
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return None;
        }
        for root in self.roots(tenant, user) {
            let resolved = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            if let Ok(relative) = resolved.strip_prefix(&root) {
                if !relative.as_os_str().is_empty()
                    && relative
                        .components()
                        .all(|c| matches!(c, Component::Normal(_)))
                {
                    return Some(root.join(relative));
                }
            }
        }
        let parts: Vec<_> = p
            .components()
            .filter_map(|c| {
                if let Component::Normal(v) = c {
                    v.to_str()
                } else {
                    None
                }
            })
            .collect();
        let pairs = [
            (legacy(tenant), legacy(user)),
            (current(tenant), current(user)),
        ];
        let matches: Vec<_> = parts
            .windows(2)
            .enumerate()
            .filter(|(_, v)| pairs.iter().any(|(t, u)| v[0] == t && v[1] == u))
            .map(|(i, _)| i)
            .collect();
        if matches.len() != 1 || matches[0] + 2 >= parts.len() {
            return None;
        }
        let mut p = self.base.clone();
        for part in &parts[matches[0]..] {
            if [".", "..", ""].contains(part) {
                return None;
            }
            p.push(part);
        }
        Some(p)
    }
    fn safe(path: &Path, root: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let Ok(meta) = std::fs::symlink_metadata(root) else {
            return false;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return false;
        }
        let parts: Vec<_> = relative.components().collect();
        if parts.iter().any(|c| !matches!(c, Component::Normal(_))) {
            return false;
        }
        let mut p = root.to_path_buf();
        for (i, part) in parts.iter().enumerate() {
            p.push(part.as_os_str());
            match std::fs::symlink_metadata(&p) {
                Ok(m) => {
                    if m.file_type().is_symlink()
                        || if i + 1 == parts.len() {
                            !m.is_file()
                        } else {
                            !m.is_dir()
                        }
                    {
                        return false;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && i + 1 == parts.len() => {}
                Err(_) => return false,
            }
        }
        true
    }
    #[cfg(unix)]
    fn generation(path: &Path, relative: &Path, old: bool) -> Result<String, AgentError> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).map_err(|_| err("memory_source_file_missing"))?;
        let bytes = std::fs::read(path).map_err(|_| err("memory_source_file_missing"))?;
        let content = super::spec::hex(&Sha256::digest(bytes));
        let generation = format!(
            "{}:{}:{}:{}:{}:{content}",
            meta.dev(),
            meta.ino(),
            meta.size(),
            i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec())
        );
        Ok(format!(
            "memsrc_{}",
            &hash(&format!(
                "{}{}\0{generation}",
                if old { "legacy/" } else { "" },
                relative.to_string_lossy().replace('\\', "/")
            ))[..32]
        ))
    }
    #[cfg(not(unix))]
    fn generation(_path: &Path, _relative: &Path, _old: bool) -> Result<String, AgentError> {
        Err(err("memory_source_file_generation_unavailable"))
    }
    pub fn resolve(
        &self,
        tenant: &str,
        user: &str,
        stored: &str,
        handle: &str,
        allow_absent: bool,
    ) -> Result<Value, AgentError> {
        let p = self
            .candidate(tenant, user, stored)
            .ok_or_else(|| err("memory_source_file_scope_unproven"))?;
        let roots = self.roots(tenant, user);
        let root = roots
            .iter()
            .find(|r| p.starts_with(r))
            .ok_or_else(|| err("memory_source_file_scope_unproven"))?;
        if !Self::safe(&p, root) {
            return Err(err("memory_source_file_unsafe"));
        }
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| err("memory_source_file_invalid"))?;
        let parent = p
            .parent()
            .ok_or_else(|| err("memory_source_file_invalid"))?;
        let markers = [
            parent.join(format!(".{name}.{handle}.deleting")),
            parent.join(format!(".{name}.{handle}.finalizing")),
        ];
        if !p.exists() {
            if markers
                .iter()
                .any(|m| std::fs::symlink_metadata(m).is_ok_and(|s| s.file_type().is_symlink()))
            {
                return Err(err("memory_source_file_unsafe"));
            }
            let present: Vec<_> = markers.iter().filter(|m| m.is_file()).collect();
            if present.len() > 1 {
                return Err(err("memory_source_file_unsafe"));
            }
            if let Some(marker) = present.first() {
                if !Self::safe(marker, root) {
                    return Err(err("memory_source_file_unsafe"));
                }
                let actual = Self::generation(
                    marker,
                    p.strip_prefix(root).expect("scoped"),
                    root == &roots[1],
                )?;
                if actual != handle {
                    return Err(err("memory_source_file_changed_since_prepare"));
                }
                return Ok(
                    json!({"path":marker.to_string_lossy(),"generation":actual,"absent":false}),
                );
            }
            if !allow_absent {
                return Err(err("memory_source_file_missing"));
            }
            return Ok(json!({"path":p.to_string_lossy(),"generation":null,"absent":true}));
        }
        if markers.iter().any(|m| {
            m.exists() && !std::fs::symlink_metadata(m).is_ok_and(|s| s.file_type().is_symlink())
        }) {
            return Err(err("memory_source_file_unsafe"));
        }
        if p.extension()
            .and_then(|s| s.to_str())
            .is_none_or(|s| !s.eq_ignore_ascii_case("md"))
        {
            return Err(err("memory_source_file_invalid"));
        }
        let actual =
            Self::generation(&p, p.strip_prefix(root).expect("scoped"), root == &roots[1])?;
        if actual != handle {
            return Err(err("memory_source_file_changed_since_prepare"));
        }
        Ok(json!({"path":p.to_string_lossy(),"generation":actual,"absent":false}))
    }
    pub fn delete_verify(&self, item: &Value) -> Result<bool, AgentError> {
        let p = Path::new(text(item, "path"));
        if item["absent"] != true {
            if !Self::safe(p, &self.base) {
                return Err(err("memory_source_file_unsafe"));
            }
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(err("memory_source_file_delete_failed")),
            }
        }
        Ok(!p.exists() && !std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
    }
}
#[derive(Clone)]
pub struct RuntimeMemoryCleanup {
    pub pool: PgPool,
    pub files: MemoryFiles,
    pub vector_url: Option<reqwest::Url>,
    pub vector_headers: reqwest::header::HeaderMap,
    pub client: reqwest::Client,
}
impl RuntimeMemoryCleanup {
    pub async fn vector_delete_verify(
        &self,
        collection: &str,
        ids: &[String],
        tenant: &str,
        user: &str,
    ) -> Result<bool, AgentError> {
        if ids.is_empty() {
            return Ok(true);
        }
        let mut url = self
            .vector_url
            .clone()
            .ok_or_else(|| err("memory_vector_cleanup_unavailable"))?;
        url.path_segments_mut()
            .map_err(|_| err("memory_vector_cleanup_unavailable"))?
            .pop_if_empty()
            .push("collections")
            .push(collection)
            .push("points")
            .push("delete");
        let response=self.client.post(url.clone()).query(&[("wait","true")]).headers(self.vector_headers.clone()).json(&json!({"filter":{"must":[{"key":"tenant_id","match":{"value":tenant}},{"key":"user_id","match":{"value":user}},{"has_id":ids}]}})).send().await.map_err(|_|err("memory_vector_delete_failed"))?;
        if response.status().as_u16() == 404 {
            return Ok(true);
        }
        if !response.status().is_success() {
            return Err(err("memory_vector_delete_failed"));
        }
        url.path_segments_mut()
            .map_err(|_| err("memory_vector_cleanup_unavailable"))?
            .pop();
        let response = self
            .client
            .post(url)
            .headers(self.vector_headers.clone())
            .json(&json!({"ids":ids,"with_payload":false,"with_vector":false}))
            .send()
            .await
            .map_err(|_| err("memory_vector_delete_failed"))?;
        if response.status().as_u16() == 404 {
            return Ok(true);
        }
        if !response.status().is_success() {
            return Err(err("memory_vector_delete_failed"));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| err("memory_vector_delete_failed"))?;
        let result = if value["result"].is_object() {
            &value["result"]["points"]
        } else {
            &value["result"]
        };
        Ok(!result
            .as_array()
            .into_iter()
            .flatten()
            .any(Value::is_object))
    }
    async fn manifests(
        &self,
        tenant: &str,
        principals: &[String],
        cutoff: Option<DateTime<Utc>>,
    ) -> Result<BTreeMap<String, Vec<Value>>, AgentError> {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(s) FROM assistant_memory_sources s WHERE tenant_id=$1 AND user_id=ANY($2::varchar[]) AND LOWER(COALESCE(metadata->>'deletion_completed','false'))<>'true' ORDER BY user_id,source_id").bind(tenant).bind(principals).fetch_all(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, tenant)).await.map_err(|_|err("memory_cleanup_manifest_unavailable"))?;
        let ids: Vec<_> = rows
            .iter()
            .map(|r| text(r, "source_id").to_owned())
            .collect();
        let chunks:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source_id',source_id,'vector_id',vector_id) FROM assistant_memory_chunks WHERE tenant_id=$1 AND source_id=ANY($2::text[]::uuid[]) AND vector_id IS NOT NULL ORDER BY source_id,vector_id").bind(tenant).bind(ids).fetch_all(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, tenant)).await.map_err(|_|err("memory_cleanup_manifest_unavailable"))?;
        let mut vectors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for c in chunks {
            let id = text(&c, "vector_id");
            if !id.is_empty() {
                vectors
                    .entry(text(&c, "source_id").into())
                    .or_default()
                    .insert(id.into());
            }
        }
        let mut manifests: BTreeMap<String, Vec<Value>> =
            principals.iter().map(|s| (s.clone(), vec![])).collect();
        for row in rows {
            let user = text(&row, "user_id");
            if !manifests.contains_key(user) {
                continue;
            }
            let at = DateTime::parse_from_rfc3339(text(&row, "updated_at"))
                .ok()
                .map(|v| v.with_timezone(&Utc));
            if cutoff.is_some_and(|c| at.is_none_or(|t| t > c)) {
                continue;
            }
            let path = text(&row, "source_path").replace('\\', "/");
            let label = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            let kind = row["source_type"].as_str().unwrap_or("unknown");
            if label.is_empty()
                || matches!(label, "." | "..")
                || kind.is_empty()
                || kind.len() > 128
                || !kind.as_bytes()[0].is_ascii_lowercase()
                || !kind
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            {
                return Err(err("memory_cleanup_manifest_invalid"));
            }
            let metadata = if let Some(s) = row["metadata"].as_str() {
                serde_json::from_str::<Value>(s)
                    .map_err(|_| err("memory_cleanup_manifest_invalid"))?
            } else {
                row["metadata"].clone()
            };
            let handle = metadata["source_handle"]
                .as_str()
                .or_else(|| metadata["deletion_source_handle"].as_str())
                .unwrap_or("");
            if !handle
                .strip_prefix("memsrc_")
                .is_some_and(|v| v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(err("memory_cleanup_source_handle_unavailable"));
            }
            let allow = ["deletion_pending", "deletion_completed"].iter().any(|k| {
                metadata[*k] == true
                    || metadata[*k]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case("true"))
            });
            let files = self.files.clone();
            let t = tenant.to_owned();
            let u = user.to_owned();
            let stored = text(&row, "source_path").to_owned();
            let h = handle.to_owned();
            let file =
                tokio::task::spawn_blocking(move || files.resolve(&t, &u, &stored, &h, allow))
                    .await
                    .map_err(|_| err("memory_cleanup_manifest_unavailable"))??;
            let version = contract::digest(
                &json!({"source_handle":handle,"source_type":kind,"content_hash":row["content_hash"].as_str().unwrap_or(""),"updated_at":at.map(|v|v.to_rfc3339()).unwrap_or_else(||text(&row,"updated_at").into()),"file_generation":file["generation"]}),
            );
            let collections = metadata
                .get("vector_collections")
                .cloned()
                .unwrap_or(json!([]));
            let collections = collections
                .as_array()
                .ok_or_else(|| err("memory_vector_collection_inventory_invalid"))?;
            let point_ids = vectors
                .get(text(&row, "source_id"))
                .cloned()
                .unwrap_or_default();
            if !point_ids.is_empty() && collections.is_empty() {
                return Err(err("memory_vector_collection_inventory_invalid"));
            }
            let collections: BTreeSet<_> = collections
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .collect();
            let vector_sets:Vec<_>=collections.into_iter().map(|collection|{let points:Vec<_>=point_ids.iter().map(|id|json!({"point_id":id,"version_digest":contract::digest(&json!({"point_id":id,"source_version":version}))})).collect();json!({"collection_kind":"current","collection_handle":format!("memvec_{}",&hash(collection)[..32]),"point_count":points.len(),"points":points,"_collection":collection})}).collect();
            manifests.get_mut(user).expect("principal").push(json!({"logical_source_scope_handle":format!("memscope_{}",&hash(&format!("{tenant}\0{user}\0{label}"))[..32]),"source_handle":handle,"version_digest":version,"source_type":kind,"_source_id":row["source_id"],"_file":file,"_vector_sets":vector_sets}));
        }
        Ok(manifests)
    }
    pub async fn inspect(&self, plan: &Value) -> Result<Value, AgentError> {
        contract::validate_plan(plan)?;
        let principals: Vec<String> = plan["principal_handles"]
            .as_array()
            .expect("validated")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if principals.len() > 10000 {
            return Err(err("memory_cleanup_scope_too_large"));
        }
        let cutoff = DateTime::parse_from_rfc3339(text(plan, "cutoff_at"))
            .map_err(|_| err("memory_cleanup_cutoff_invalid"))?
            .with_timezone(&Utc);
        let manifests = self
            .manifests(text(plan, "tenant_id"), &principals, Some(cutoff))
            .await?;
        let mut rows = Vec::new();
        let (mut source_count, mut vector_count) = (0, 0);
        for principal in principals {
            let sources = &manifests[&principal];
            let mut public:Vec<_>=sources.iter().map(|s|json!({"logical_source_scope_handle":s["logical_source_scope_handle"],"source_handle":s["source_handle"],"version_digest":s["version_digest"],"source_type":s["source_type"]})).collect();
            public.sort_by_key(|v| text(v, "logical_source_scope_handle").to_owned());
            let vectors = merge_vectors(sources, false);
            let count: usize = vectors
                .iter()
                .map(|v| v["point_count"].as_u64().unwrap_or(0) as usize)
                .sum();
            source_count += public.len();
            vector_count += count;
            if source_count > 20000 || vector_count > 400000 {
                return Err(err("memory_cleanup_scope_too_large"));
            }
            rows.push(json!({"principal_id":principal,"source_count":public.len(),"sources":public,"vector_count":count,"vector_sets":vectors}));
        }
        let mut result = json!({"schema_version":"agent-runtime-cleanup-inventory/v1","deletion_id":plan["deletion_id"],"tenant_id":plan["tenant_id"],"agent_id":plan["agent_id"],"plan_digest":plan["plan_digest"],"cutoff_at":plan["cutoff_at"],"principal_count":rows.len(),"source_count":source_count,"vector_count":vector_count,"principals":rows});
        result["inventory_digest"] = contract::digest(&result).into();
        contract::validate_inventory(&result, plan)?;
        Ok(result)
    }
    pub async fn execute(&self, plan: &Value, inventory: &Value) -> Result<Value, AgentError> {
        contract::validate_inventory(inventory, plan)?;
        let principals: Vec<String> = plan["principal_handles"]
            .as_array()
            .expect("validated")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let current = self
            .manifests(text(plan, "tenant_id"), &principals, None)
            .await?;
        let mut receipts = Vec::new();
        for frozen in inventory["principals"].as_array().expect("validated") {
            let principal = text(frozen, "principal_id");
            let current_sources: &[Value] = &current[principal];
            let vectors = merge_vectors(current_sources, true);
            let (mut deleted_sources, mut absent_sources, mut deleted_vectors, mut absent_vectors) =
                (0_u64, 0_u64, 0_u64, 0_u64);
            let mut errors = Vec::<String>::new();
            let mut vector_failed = false;
            for set in frozen["vector_sets"].as_array().expect("validated") {
                let Some(now) = vectors
                    .iter()
                    .find(|v| v["collection_handle"] == set["collection_handle"])
                else {
                    absent_vectors += set["point_count"].as_u64().unwrap_or(0);
                    continue;
                };
                let mut ids = Vec::new();
                for point in set["points"].as_array().expect("validated") {
                    let current = now["points"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|p| p["point_id"] == point["point_id"]);
                    match current {
                        None => absent_vectors += 1,
                        Some(p) if p["version_digest"] != point["version_digest"] => {
                            errors.push("memory_vector_changed_since_prepare".into());
                            vector_failed = true;
                        }
                        Some(_) => ids.push(text(point, "point_id").to_owned()),
                    }
                }
                if !ids.is_empty() && !vector_failed {
                    match self
                        .vector_delete_verify(
                            text(now, "_collection"),
                            &ids,
                            text(plan, "tenant_id"),
                            principal,
                        )
                        .await
                    {
                        Ok(true) => deleted_vectors += ids.len() as u64,
                        Ok(false) => {
                            vector_failed = true;
                            errors.push("memory_vector_delete_readback_failed".into());
                        }
                        Err(e) => {
                            vector_failed = true;
                            errors.push(text(&e.detail, "code").into());
                        }
                    }
                }
            }
            for source in frozen["sources"].as_array().expect("validated") {
                let Some(now) = current_sources
                    .iter()
                    .find(|v| v["source_handle"] == source["source_handle"])
                else {
                    absent_sources += 1;
                    continue;
                };
                if now["version_digest"] != source["version_digest"] {
                    errors.push("memory_source_changed_since_prepare".into());
                    continue;
                }
                if vector_failed {
                    continue;
                }
                let files = self.files.clone();
                let item = now["_file"].clone();
                match tokio::task::spawn_blocking(move || files.delete_verify(&item)).await {
                    Ok(Ok(true)) => {}
                    Ok(Err(e)) => {
                        errors.push(text(&e.detail, "code").into());
                        continue;
                    }
                    _ => {
                        errors.push("memory_source_file_delete_readback_failed".into());
                        continue;
                    }
                }
                let deleted=sqlx::query("DELETE FROM assistant_memory_sources WHERE tenant_id=$1 AND user_id=$2 AND source_id=$3::uuid AND(metadata->>'source_handle'=$4 OR metadata->>'deletion_source_handle'=$4)").bind(text(plan,"tenant_id")).bind(principal).bind(text(now,"_source_id")).bind(text(source,"source_handle")).execute(&self.pool).await;
                if deleted.is_err() {
                    errors.push("memory_source_delete_failed".into());
                    continue;
                }
                let remaining=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM assistant_memory_sources WHERE tenant_id=$1 AND user_id=$2 AND source_id=$3::uuid)").bind(text(plan,"tenant_id")).bind(principal).bind(text(now,"_source_id")).fetch_one(&self.pool).await;
                match remaining {
                    Ok(false) => deleted_sources += 1,
                    _ => errors.push("memory_source_delete_readback_failed".into()),
                }
            }
            let mut seen = BTreeSet::new();
            errors.retain(|e| seen.insert(e.clone()));
            let done = errors.is_empty()
                && deleted_sources + absent_sources == frozen["source_count"].as_u64().unwrap_or(0)
                && deleted_vectors + absent_vectors == frozen["vector_count"].as_u64().unwrap_or(0);
            if !done && errors.is_empty() {
                errors.push("memory_cleanup_incomplete".into());
            }
            receipts.push(json!({"principal_id":principal,"status":if done{"completed"}else{"partial"},"completed":done,"retryable":!done,"source_count":frozen["source_count"],"deleted_source_count":deleted_sources,"vector_count":frozen["vector_count"],"deleted_vector_count":deleted_vectors,"idempotent_absent_count":absent_sources,"idempotent_absent_vector_count":absent_vectors,"errors":errors}));
        }
        let done = receipts.iter().all(|r| r["completed"] == true);
        let errors: BTreeSet<_> = receipts
            .iter()
            .flat_map(|v| {
                v["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
            })
            .collect();
        let mut receipt = json!({"schema_version":"agent-runtime-cleanup-receipt/v1","deletion_id":plan["deletion_id"],"tenant_id":plan["tenant_id"],"agent_id":plan["agent_id"],"plan_digest":plan["plan_digest"],"inventory_digest":inventory["inventory_digest"],"status":if done{"completed"}else{"partial"},"completed":done,"retryable":!done,"principals":receipts,"errors":errors});
        receipt["receipt_digest"] = contract::digest(&receipt).into();
        contract::validate_receipt(&receipt, plan, inventory)?;
        Ok(receipt)
    }
}
fn merge_vectors(sources: &[Value], internal: bool) -> Vec<Value> {
    let mut sets: BTreeMap<String, Value> = BTreeMap::new();
    for source in sources {
        for set in source["_vector_sets"].as_array().into_iter().flatten() {
            let target=sets.entry(text(set,"collection_handle").into()).or_insert_with(||{let mut v=json!({"collection_kind":set["collection_kind"],"collection_handle":set["collection_handle"],"point_count":0,"points":[]});if internal{v["_collection"]=set["_collection"].clone();}v});
            for point in set["points"].as_array().into_iter().flatten() {
                if !target["points"]
                    .as_array()
                    .expect("points")
                    .iter()
                    .any(|p| p["point_id"] == point["point_id"])
                {
                    target["points"]
                        .as_array_mut()
                        .expect("points")
                        .push(point.clone());
                }
            }
        }
    }
    sets.into_values()
        .map(|mut v| {
            v["points"]
                .as_array_mut()
                .expect("points")
                .sort_by_key(|p| text(p, "point_id").to_owned());
            v["point_count"] = v["points"].as_array().expect("points").len().into();
            v
        })
        .collect()
}
