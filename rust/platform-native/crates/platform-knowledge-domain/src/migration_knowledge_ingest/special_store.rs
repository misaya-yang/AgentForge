//! Durable special-publication state machine. Every mutator runs inside its
//! caller's PostgreSQL transaction, including negative-revision binding.
use super::{IngestError, Result, integer, object, store::KnowledgeWriteStore, string};
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::collections::BTreeSet;
const SAVE: &str = "UPDATE knowledge.document_pipeline_executions SET manifest=jsonb_set(COALESCE(manifest,'{}'::jsonb),'{special_publication}',$2::jsonb,TRUE) WHERE execution_id=$1 AND status='running'";
fn fail() -> IngestError {
    IngestError::GenerationChanged
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn validate_plan(collections: &Value, objects: &Value) -> Result<()> {
    let Some(collections) = collections.as_object() else {
        return Err(fail());
    };
    let Some(objects) = objects.as_object() else {
        return Err(fail());
    };
    for (name, entry) in collections {
        if name.trim().is_empty() {
            return Err(fail());
        }
        for field in ["candidate_point_ids", "old_point_ids"] {
            let Some(ids) = entry[field].as_array() else {
                return Err(fail());
            };
            let mut seen = BTreeSet::new();
            for id in ids {
                let Some(id) = id.as_str().filter(|s| !s.is_empty()) else {
                    return Err(fail());
                };
                if !seen.insert(id) {
                    return Err(fail());
                }
            }
        }
    }
    for (key, digest) in objects {
        if key.is_empty() || !digest.as_str().is_some_and(hash) {
            return Err(fail());
        }
    }
    Ok(())
}
async fn owner(
    conn: &mut PgConnection,
    execution: &str,
    document: &str,
    dataset: &str,
) -> Result<(Value, Value)> {
    let doc:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.documents d WHERE document_id=$1 AND dataset_id=$2 FOR UPDATE").bind(document).bind(dataset).fetch_optional(crate::database::connection(&mut *conn)).await?.ok_or_else(fail)?;
    let row:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM knowledge.document_pipeline_executions e WHERE execution_id=$1 AND document_id=$2 AND dataset_id=$3 FOR UPDATE").bind(execution).bind(document).bind(dataset).fetch_optional(crate::database::connection(&mut *conn)).await?.ok_or_else(fail)?;
    Ok((doc, row))
}
impl KnowledgeWriteStore {
    pub async fn record_special(
        conn: &mut PgConnection,
        execution: &str,
        document: &str,
        dataset: &str,
        generation: &str,
        source_hash: &str,
        planned_keys: &[String],
    ) -> Result<Value> {
        if !hash(source_hash) || uuid::Uuid::parse_str(generation).is_err() {
            return Err(fail());
        }
        let mut keys = planned_keys.to_vec();
        keys.sort();
        keys.dedup();
        if keys.iter().any(|s| s.trim().is_empty()) {
            return Err(fail());
        }
        let (doc, row) = owner(conn, execution, document, dataset).await?;
        if string(&row, "status") != "running" {
            return Err(fail());
        }
        let meta = object(&doc["metadata"]);
        let existing = object(&row["manifest"]["special_publication"]);
        let marked = string(&meta, "_special_publication_generation_id");
        if !marked.is_empty() && marked != generation {
            return Err(fail());
        }
        if existing.as_object().is_some_and(|m| !m.is_empty()) {
            if string(&existing, "generation_id") != generation
                || marked != generation
                || string(&existing, "source_hash") != source_hash
                || existing["planned_object_keys"] != json!(keys)
            {
                return Err(fail());
            }
            if !["committed", "aborted"].contains(&string(&existing, "phase"))
                && string(&meta, "_document_pipeline_execution_id") != execution
            {
                return Err(fail());
            }
            return Ok(existing);
        }
        if string(&meta, "_document_pipeline_execution_id") != execution
            || ["completed", "error"].contains(&string(&doc, "status"))
        {
            return Err(fail());
        }
        let special = json!({"schema_version":1,"generation_id":generation,"source_hash":source_hash,"plan_hash":null,"planned_object_keys":keys,"phase":"preparing","collections":{},"objects":{}});
        sqlx::query(SAVE)
            .bind(execution)
            .bind(&special)
            .execute(crate::database::connection(&mut *conn))
            .await?;
        sqlx::query("UPDATE knowledge.documents SET metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{_special_publication_generation_id}',to_jsonb($3::text),TRUE) WHERE document_id=$1 AND dataset_id=$2").bind(document).bind(dataset).bind(generation).execute(crate::database::connection(&mut *conn)).await?;
        Ok(special)
    }
    pub async fn get_special(conn: &mut PgConnection, execution: &str) -> Result<Option<Value>> {
        let row:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(e) FROM knowledge.document_pipeline_executions e WHERE execution_id=$1").bind(execution).fetch_optional(crate::database::connection(&mut *conn)).await?;
        let Some(row) = row else { return Ok(None) };
        let mut special = object(&row["manifest"]["special_publication"]);
        if special.as_object().is_some_and(|m| m.is_empty()) {
            return Ok(None);
        }
        for field in ["execution_id", "document_id", "dataset_id"] {
            special[field] = row[field].clone()
        }
        special["execution_status"] = row["status"].clone();
        Ok(Some(special))
    }
    pub async fn bind_special(
        conn: &mut PgConnection,
        execution: &str,
        dataset: &str,
        revision: i64,
    ) -> Result<()> {
        if revision >= 0 {
            return Err(fail());
        }
        let actual:i64=sqlx::query_scalar("SELECT content_revision FROM knowledge.datasets WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE").bind(dataset).fetch_one(crate::database::connection(&mut *conn)).await?;
        if actual != revision {
            return Err(fail());
        }
        let document:String=sqlx::query_scalar("SELECT document_id FROM knowledge.document_pipeline_executions WHERE execution_id=$1 AND dataset_id=$2").bind(execution).bind(dataset).fetch_optional(crate::database::connection(&mut *conn)).await?.ok_or_else(fail)?;
        let (doc, row) = owner(conn, execution, &document, dataset).await?;
        let mut special = object(&row["manifest"]["special_publication"]);
        if string(&row, "status") != "running"
            || string(&special, "phase") != "preparing"
            || string(&special, "generation_id").is_empty()
            || doc["metadata"]["_special_publication_generation_id"] != special["generation_id"]
            || doc["metadata"]["_document_pipeline_execution_id"] != execution
        {
            return Err(fail());
        }
        if special
            .get("publication_revision")
            .is_some_and(|r| r.as_i64() != Some(revision.abs()))
        {
            return Err(fail());
        }
        let conflict:Option<String>=sqlx::query_scalar("SELECT execution_id FROM knowledge.document_pipeline_executions WHERE dataset_id=$1 AND execution_id<>$2 AND status='running' AND manifest#>>'{special_publication,publication_revision}'=$3 AND manifest#>>'{special_publication,phase}' IN ('preparing','prepared','points_written','committed') LIMIT 1").bind(dataset).bind(execution).bind(revision.abs().to_string()).fetch_optional(crate::database::connection(&mut *conn)).await?;
        if conflict.is_some() {
            return Err(fail());
        }
        special["publication_revision"] = json!(revision.abs());
        sqlx::query(SAVE)
            .bind(execution)
            .bind(special)
            .execute(crate::database::connection(&mut *conn))
            .await?;
        Ok(())
    }
    pub async fn advance_special(
        conn: &mut PgConnection,
        execution: &str,
        document: &str,
        dataset: &str,
        generation: &str,
        source_hash: &str,
        plan_hash: Option<&str>,
        expected: &str,
        next: &str,
        collections: Option<&Value>,
        objects: Option<&Value>,
    ) -> Result<Value> {
        if !matches!(
            (expected, next),
            ("preparing", "preparing" | "prepared" | "aborted")
                | ("prepared", "points_written" | "aborted")
                | ("points_written", "committed" | "aborted")
        ) {
            return Err(fail());
        }
        if !hash(source_hash)
            || (["prepared", "points_written", "committed"].contains(&next)
                && !plan_hash.is_some_and(hash))
        {
            return Err(fail());
        }
        let (doc, row) = owner(conn, execution, document, dataset).await?;
        let mut s = object(&row["manifest"]["special_publication"]);
        if string(&row, "status") != "running"
            || doc["metadata"]["_special_publication_generation_id"] != generation
            || string(&s, "generation_id") != generation
            || string(&s, "source_hash") != source_hash
        {
            return Err(fail());
        }
        if let Some(hash) = plan_hash {
            if !s["plan_hash"].is_null() && s["plan_hash"] != hash {
                return Err(fail());
            }
        }
        if string(&s, "phase") == next && expected != next {
            if collections.is_some_and(|v| v != &s["collections"])
                || objects.is_some_and(|v| v != &s["objects"])
            {
                return Err(fail());
            }
            return Ok(s);
        }
        if string(&s, "phase") != expected {
            return Err(fail());
        }
        let mut cols = object(&s["collections"]);
        let mut objs = object(&s["objects"]);
        if expected == "preparing" {
            for (target, addition) in [(&mut cols, collections), (&mut objs, objects)] {
                if let Some(addition) = addition {
                    for (k, v) in addition.as_object().ok_or_else(fail)? {
                        if target.get(k).is_some_and(|old| old != v) {
                            return Err(fail());
                        }
                        target[k] = v.clone()
                    }
                }
            }
        } else if collections.is_some_and(|v| v != &cols) || objects.is_some_and(|v| v != &objs) {
            return Err(fail());
        }
        validate_plan(&cols, &objs)?;
        if next == "prepared" {
            let keys: Vec<_> = objs
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if (cols.as_object().unwrap().is_empty() && keys.is_empty())
                || s["planned_object_keys"] != json!(keys)
            {
                return Err(fail());
            }
        }
        if next == "committed" {
            if string(&doc, "status") != "completed" {
                return Err(fail());
            }
            let revision:i64=sqlx::query_scalar("SELECT content_revision FROM knowledge.datasets WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE").bind(dataset).fetch_one(crate::database::connection(&mut *conn)).await?;
            let bound = integer(&s, "publication_revision");
            if revision >= 0 || bound <= 0 || revision.abs() > bound {
                return Err(fail());
            }
            s["publication_revision"] = json!(revision.abs())
        }
        s["phase"] = json!(next);
        s["collections"] = cols;
        s["objects"] = objs;
        if let Some(hash) = plan_hash {
            s["plan_hash"] = json!(hash)
        }
        sqlx::query(SAVE)
            .bind(execution)
            .bind(&s)
            .execute(crate::database::connection(&mut *conn))
            .await?;
        Ok(s)
    }
    pub async fn active_special(conn: &mut PgConnection, dataset: &str) -> Result<Value> {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('document_metadata',d.metadata,'content_revision',ds.content_revision,'tenant_id',ds.tenant_id) FROM knowledge.document_pipeline_executions e JOIN knowledge.documents d ON d.document_id=e.document_id AND d.dataset_id=e.dataset_id JOIN knowledge.datasets ds ON ds.dataset_id=e.dataset_id WHERE e.dataset_id=$1 AND e.status='running' AND ds.content_revision<0 AND ds.is_deleted=FALSE AND COALESCE(e.manifest,'{}'::jsonb)?'special_publication' AND (e.manifest->'special_publication')?'publication_revision' AND e.manifest#>>'{special_publication,phase}' IN ('preparing','prepared','points_written','committed') ORDER BY e.created_at DESC,e.execution_id DESC LIMIT 2").bind(dataset).fetch_all(crate::database::connection(&mut *conn)).await?;
        if rows.len() != 1 {
            return Err(fail());
        }
        let row = &rows[0];
        let mut s = object(&row["manifest"]["special_publication"]);
        if integer(&s, "publication_revision") <= 0
            || integer(row, "content_revision").abs() > integer(&s, "publication_revision")
            || !hash(string(&s, "source_hash"))
            || string(row, "tenant_id").trim().is_empty()
            || row["document_metadata"]["_special_publication_generation_id"] != s["generation_id"]
            || row["document_metadata"]["_document_pipeline_execution_id"] != row["execution_id"]
        {
            return Err(fail());
        }
        if string(&s, "phase") != "preparing" && !hash(string(&s, "plan_hash")) {
            return Err(fail());
        }
        let keys = s["planned_object_keys"].as_array().ok_or_else(fail)?;
        let sorted: BTreeSet<_> = keys.iter().filter_map(Value::as_str).collect();
        if sorted.len() != keys.len() || json!(sorted) != s["planned_object_keys"] {
            return Err(fail());
        }
        for field in ["execution_id", "document_id", "dataset_id", "tenant_id"] {
            s[field] = row[field].clone()
        }
        s["execution_status"] = row["status"].clone();
        Ok(s)
    }
    pub async fn reconcile_special(
        conn: &mut PgConnection,
        execution: &str,
        document: &str,
        dataset: &str,
        generation: &str,
    ) -> Result<&'static str> {
        let (doc, row) = owner(conn, execution, document, dataset).await?;
        let s = object(&row["manifest"]["special_publication"]);
        if string(&s, "generation_id") != generation {
            return Ok("unproven");
        }
        let (terminal, error) = match (string(&s, "phase"), string(&doc, "status")) {
            ("committed", "completed") => ("completed", None),
            ("aborted", "error") => ("error", Some("special publication aborted after cleanup")),
            _ => return Ok("unproven"),
        };
        if string(&row, "status") == terminal {
            return Ok("already_terminal");
        }
        if string(&row, "status") != "running"
            || doc["metadata"]["_special_publication_generation_id"] != generation
        {
            return Ok("unproven");
        }
        if !Self::complete_execution(conn, execution, terminal, error, &json!({})).await? {
            return Err(fail());
        }
        sqlx::query("UPDATE knowledge.documents SET metadata=COALESCE(metadata,'{}'::jsonb)-'_special_publication_generation_id' WHERE document_id=$1 AND dataset_id=$2 AND metadata->>'_special_publication_generation_id'=$3").bind(document).bind(dataset).bind(generation).execute(crate::database::connection(&mut *conn)).await?;
        Ok("reconciled")
    }
    pub async fn resume_special(conn: &mut PgConnection, owner_manifest: &Value) -> Result<()> {
        let execution = string(owner_manifest, "execution_id");
        let document = string(owner_manifest, "document_id");
        let dataset = string(owner_manifest, "dataset_id");
        let (doc, row) = owner(conn, execution, document, dataset).await?;
        let special = &row["manifest"]["special_publication"];
        if string(&row, "status") != "running"
            || string(special, "phase") != "preparing"
            || special["generation_id"] != owner_manifest["generation_id"]
            || special["source_hash"] != owner_manifest["source_hash"]
            || doc["metadata"]["_special_publication_generation_id"] != special["generation_id"]
            || doc["metadata"]["_document_pipeline_execution_id"] != execution
            || !matches!(
                string(&doc, "status"),
                "waiting" | "parsing" | "splitting" | "indexing"
            )
            || !row["input_snapshot"].is_object()
            || string(&row, "process_rule_id").is_empty()
        {
            return Err(fail());
        }
        sqlx::query("UPDATE knowledge.document_pipeline_executions SET manifest=(COALESCE(manifest,'{}'::jsonb)-'special_publication')||jsonb_build_object('preparing_replayed_after_restart',TRUE) WHERE execution_id=$1 AND status='running'").bind(execution).execute(crate::database::connection(&mut *conn)).await?;
        let changed=sqlx::query("UPDATE knowledge.documents SET status='waiting',progress=0,error=NULL,process_rule_id=$5,started_at=NULL,parsing_started_at=NULL,splitting_started_at=NULL,indexing_started_at=NULL,updated_at=NOW(),metadata=COALESCE(metadata,'{}'::jsonb)-$3::text WHERE document_id=$1 AND dataset_id=$2 AND metadata->>$3::text=$4").bind(document).bind(dataset).bind("_special_publication_generation_id").bind(string(special,"generation_id")).bind(string(&row,"process_rule_id")).execute(crate::database::connection(&mut *conn)).await?.rows_affected();
        if changed != 1 {
            return Err(fail());
        }
        Ok(())
    }
}
