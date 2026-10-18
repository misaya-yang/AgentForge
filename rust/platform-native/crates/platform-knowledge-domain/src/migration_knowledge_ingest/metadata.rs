//! Typed document metadata registry and source-owned-field-preserving patches.
use super::{Actor, IngestError, IngestionService, KnowledgeWriteStore, Result, object, string};
use serde_json::{Value, json};
use sqlx::Connection;
use std::collections::{BTreeMap, BTreeSet};
pub const REVISION_CONFLICT: &str = "metadata schema changed; refresh and retry";
fn invalid(s: impl Into<String>) -> IngestError {
    IngestError::Validation(s.into())
}
fn bounded(value: &Value, what: &str) -> Result<()> {
    if serde_json::to_vec(value)?.len() > 16 * 1024 {
        return Err(invalid(format!("{what} exceeds 16 KiB")));
    }
    Ok(())
}
pub fn normalize_registry(config: &Value) -> Result<Value> {
    let value = config
        .get("document_metadata_registry")
        .unwrap_or(&Value::Null);
    if value.is_null() {
        return Ok(json!({"version":1,"revision":0,"fields":[]}));
    }
    if !value.is_object() {
        return Err(invalid("document metadata registry is malformed"));
    }
    if value.get("version").is_some_and(|v| v != 1)
        || value.get("revision").is_some_and(|v| v.as_i64().is_none())
    {
        return Err(invalid("document metadata registry version is malformed"));
    }
    let revision = value["revision"].as_i64().unwrap_or(0);
    let fields = value.get("fields").cloned().unwrap_or(json!([]));
    if revision < 0 || !fields.is_array() {
        return Err(invalid("document metadata registry is malformed"));
    }
    Ok(json!({"version":1,"revision":revision,"fields":fields}))
}
pub fn validate_fields(fields: &[Value], previous: &[Value]) -> Result<Vec<Value>> {
    let previous: BTreeMap<_, _> = previous
        .iter()
        .map(|f| (string(f, "name"), string(f, "type")))
        .collect();
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for raw in fields {
        let name = string(raw, "name").trim();
        let label = raw["label"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(name)
            .trim();
        let kind = string(raw, "type").trim().to_lowercase();
        let desc = string(raw, "description").trim();
        if name.is_empty()
            || name.len() > 64
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        {
            return Err(invalid(
                "metadata field names must use lower_snake_case and be at most 64 characters",
            ));
        }
        if super::source_metadata_key(name)
            || [
                "tenant_id",
                "dataset_id",
                "document_id",
                "segment_id",
                "status",
                "enabled",
                "content_revision",
                "source_type",
                "language",
            ]
            .contains(&name)
        {
            return Err(invalid(format!("metadata field '{name}' is reserved")));
        }
        if !seen.insert(name.to_owned()) {
            return Err(invalid(format!("metadata field '{name}' is duplicated")));
        }
        if !["string", "number", "datetime"].contains(&kind.as_str()) {
            return Err(invalid(format!(
                "metadata field '{name}' has an unsupported type"
            )));
        }
        if previous.get(name).is_some_and(|old| *old != kind) {
            return Err(invalid(format!(
                "metadata field '{name}' type is immutable; create a new field"
            )));
        }
        if label.is_empty() || label.chars().count() > 128 || desc.chars().count() > 512 {
            return Err(invalid("metadata field label or description is too long"));
        }
        let mut field = json!({"name":name,"label":label,"type":kind});
        if !desc.is_empty() {
            field["description"] = json!(desc)
        }
        out.push(field);
    }
    if out.len() > 32 {
        return Err(invalid("metadata registry supports at most 32 fields"));
    }
    bounded(&json!(out), "metadata registry")?;
    Ok(out)
}
pub fn normalize_patch(
    registry: &Value,
    patch: &Value,
    remove: &[String],
) -> Result<(Value, Vec<String>)> {
    let definitions: BTreeMap<_, _> = registry["fields"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|v| v.is_object())
        .map(|v| (string(v, "name"), string(v, "type")))
        .collect();
    let patch = patch
        .as_object()
        .ok_or_else(|| invalid("metadata patch must be an object"))?;
    let mut seen = BTreeSet::new();
    let remove: Vec<_> = remove
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| seen.insert(s.clone()))
        .collect();
    if remove.iter().any(String::is_empty) {
        return Err(invalid("metadata_remove contains an empty field"));
    }
    let overlap: Vec<_> = patch
        .keys()
        .filter(|k| remove.contains(k))
        .cloned()
        .collect();
    if !overlap.is_empty() {
        return Err(invalid(format!(
            "metadata_patch and metadata_remove overlap: {}",
            overlap.join(", ")
        )));
    }
    let unknown: BTreeSet<_> = patch
        .keys()
        .chain(remove.iter())
        .filter(|k| !definitions.contains_key(k.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(format!(
            "unknown metadata fields: {}",
            unknown.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    let mut result = json!({});
    for (name, value) in patch {
        result[name] = match definitions[name.as_str()] {
            "string" => {
                if !value.as_str().is_some_and(|s| s.chars().count() <= 2048) {
                    return Err(invalid(format!(
                        "metadata field '{name}' must be a string up to 2048 characters"
                    )));
                }
                value.clone()
            }
            "number" => {
                if !value.is_number() {
                    return Err(invalid(format!("metadata field '{name}' must be a number")));
                }
                if !value.as_f64().is_some_and(f64::is_finite) {
                    return Err(invalid(format!("metadata field '{name}' must be finite")));
                }
                value.clone()
            }
            "datetime" => {
                let raw = value
                    .as_str()
                    .ok_or_else(|| {
                        invalid(format!(
                            "metadata field '{name}' must be an RFC3339 datetime"
                        ))
                    })?
                    .trim();
                let date = super::iso_datetime::normalize(raw).map_err(|error| match error {
                    super::iso_datetime::Error::Naive => invalid(format!(
                        "metadata field '{name}' datetime must include a timezone"
                    )),
                    super::iso_datetime::Error::Invalid => invalid(format!(
                        "metadata field '{name}' must be an RFC3339 datetime"
                    )),
                })?;
                json!(date)
            }
            _ => {
                return Err(invalid(format!(
                    "metadata field '{name}' has an unsupported type"
                )));
            }
        }
    }
    bounded(&result, "metadata patch")?;
    Ok((result, remove))
}
impl IngestionService {
    pub async fn get_metadata_registry(&self, actor: &Actor, dataset: &str) -> Result<Value> {
        self.store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let d = self.store.get_dataset(dataset).await?;
        normalize_registry(&d["index_config"])
    }
    pub async fn update_metadata_registry(
        &self,
        actor: &Actor,
        dataset: &str,
        revision: i64,
        fields: &[Value],
    ) -> Result<Value> {
        self.store
            .require_dataset_access(actor, dataset, "owner")
            .await?;
        let current = normalize_registry(&self.store.get_dataset(dataset).await?["index_config"])?;
        if current["revision"] != revision {
            return Err(invalid(REVISION_CONFLICT));
        }
        let fields = validate_fields(fields, current["fields"].as_array().unwrap())?;
        let mut tx = self.store.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let config:Value=sqlx::query_scalar("SELECT COALESCE(index_config,'null'::jsonb) FROM knowledge.datasets WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE").bind(dataset).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;
        if normalize_registry(&config)?["revision"] != revision {
            return Err(invalid(REVISION_CONFLICT));
        }
        let updated = json!({"version":1,"revision":revision.checked_add(1).ok_or_else(||invalid("metadata registry revision overflow"))?,"fields":fields});
        sqlx::query("UPDATE knowledge.datasets SET index_config=jsonb_set(CASE WHEN jsonb_typeof(index_config)='object' THEN index_config ELSE '{}'::jsonb END,$2::text[],$3::jsonb,TRUE),updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE").bind(dataset).bind(vec!["document_metadata_registry"]).bind(&updated).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(updated)
    }
    async fn patch_metadata_authorized(
        &self,
        dataset: &str,
        document: &str,
        revision: i64,
        patch: &Value,
        remove: &[String],
    ) -> Result<Value> {
        let mut lease = self.store.document_lease(dataset, document).await?;
        let result=async{let mut tx=lease.connection().begin().await?;
        crate::database::bind_current(&mut tx).await?;
            let config:Value=sqlx::query_scalar("SELECT COALESCE(index_config,'null'::jsonb) FROM knowledge.datasets WHERE dataset_id=$1 AND is_deleted=FALSE FOR SHARE").bind(dataset).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;let registry=normalize_registry(&config)?;if registry["revision"]!=revision{return Err(invalid(REVISION_CONFLICT))}
            let(patch,remove)=normalize_patch(&registry,patch,remove)?;let doc=KnowledgeWriteStore::document_on(&mut tx,dataset,document).await?;let mut metadata=if doc["metadata"].is_null(){json!({})}else{doc["metadata"].clone()};if !metadata.is_object(){return Err(invalid("document metadata is malformed"))}
            let fields=metadata.as_object_mut().unwrap();fields.retain(|key,_|!super::source_metadata_key(key));for name in remove{fields.remove(&name);}fields.extend(patch.as_object().unwrap().clone());bounded(&metadata,"document user metadata")?;
            super::document_writes::document_fields(&mut tx,document,&json!({"metadata":metadata}),false).await?;let updated=KnowledgeWriteStore::document_on(&mut tx,dataset,document).await?;tx.commit().await?;Ok(updated)
        }.await;
        lease.release().await?;
        result
    }
    pub async fn patch_document_metadata(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        revision: i64,
        patch: &Value,
        remove: &[String],
    ) -> Result<Value> {
        self.store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        self.patch_metadata_authorized(dataset, document, revision, patch, remove)
            .await
    }
    pub async fn patch_documents_metadata(
        &self,
        actor: &Actor,
        dataset: &str,
        documents: &[String],
        revision: i64,
        patch: &Value,
        remove: &[String],
    ) -> Result<Value> {
        self.store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        let (mut seen, mut failed, mut errors, mut success) =
            (BTreeSet::new(), vec![], json!({}), 0);
        for id in documents.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if !seen.insert(id) {
                continue;
            }
            match self
                .patch_metadata_authorized(dataset, id, revision, patch, remove)
                .await
            {
                Ok(_) => success += 1,
                Err(IngestError::Validation(message)) if message == REVISION_CONFLICT => {
                    return Err(invalid(message));
                }
                Err(error) => {
                    failed.push(id);
                    errors[id] = json!(match error {
                        IngestError::NotFound => "not_found".into(),
                        IngestError::Validation(message) =>
                            message.chars().take(500).collect::<String>(),
                        _ => "update_failed".into(),
                    });
                }
            }
        }
        Ok(
            json!({"success_count":success,"failed_count":failed.len(),"failed_ids":failed,"errors":errors,"metadata_schema_revision":revision}),
        )
    }
}
