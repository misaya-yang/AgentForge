//! Version-pinned golden-case storage and import, matching kb_eval_golden_store.
use super::{Result, fail};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;
pub fn resolve_version(cases: &[Value], requested: Option<&str>) -> Result<String> {
    let mut versions = BTreeSet::<String>::new();
    for case in cases {
        let value = &case["metadata"]["version"];
        match value {
            Value::Null => {}
            Value::String(value) if !value.is_empty() => {
                versions.insert(value.clone());
            }
            Value::Number(value) if value.as_f64().is_some_and(|number| number != 0.0) => {
                versions.insert(value.to_string());
            }
            Value::Bool(true) => {
                versions.insert("True".into());
            }
            Value::Bool(false) => {}
            Value::Array(values) if values.is_empty() => {}
            Value::Object(values) if values.is_empty() => {}
            Value::Array(_) | Value::Object(_) => {
                return fail("metadata.version must be a scalar value");
            }
            Value::String(_) | Value::Number(_) => {}
        }
    }
    if let Some(requested) = requested.filter(|v| !v.is_empty()) {
        if versions.iter().any(|v| v.as_str() != requested) {
            return fail("requested version contradicts row metadata versions");
        }
        return Ok(requested.into());
    }
    if versions.len() != 1 {
        return fail("rows must carry one metadata version, or specify --version");
    }
    Ok(versions.into_iter().next().unwrap())
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn clean_version(version: &str) -> Result<&str> {
    let v = version.trim();
    if v.is_empty() {
        return fail("version must be non-empty");
    }
    Ok(v)
}
fn split_valid(split: &str) -> bool {
    matches!(split, "frozen" | "growth")
}
pub fn case_to_row(
    case: &Value,
    version: &str,
    default_split: &str,
    provenance: Option<&str>,
) -> Result<Value> {
    if !case.is_object() {
        return fail("golden case must be an object");
    }
    let id = case["case_id"].as_str().unwrap_or("").trim();
    let track = case["track"].as_str().unwrap_or("");
    let query = case["query"].as_str().unwrap_or("").trim();
    if id.is_empty() || query.is_empty() || !matches!(track, "retrieval_only" | "answer_aware") {
        return fail("golden case identity, track or query invalid");
    }
    let Some(relevance) = case["relevance"].as_object() else {
        return fail("golden relevance must be an object");
    };
    if relevance.is_empty() || relevance.values().any(|v| !v.is_number()) {
        return fail("golden relevance requires numeric grades");
    }
    let metadata = case
        .get("metadata")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        return fail("golden metadata must be an object");
    }
    let split = match metadata.get("split") {
        Some(value) if !truthy(value) => default_split,
        Some(Value::String(value)) => value,
        Some(_) => return fail("golden split must be a string"),
        None => default_split,
    };
    if !split_valid(split) {
        return fail("golden split must be frozen or growth");
    }
    let reference_answer = match case.get("reference_answer") {
        None | Some(Value::Null) => Value::Null,
        Some(value @ Value::String(_)) => value.clone(),
        Some(_) => return fail("golden reference_answer must be a string or null"),
    };
    let provenance = provenance
        .unwrap_or_else(|| metadata["provenance"].as_str().unwrap_or(""))
        .chars()
        .take(100)
        .collect::<String>();
    Ok(json!({"case_id":id,"version":clean_version(version)?,
            "track":track,"query":query,"relevance":relevance,"reference_answer":reference_answer,
            "split":split,"metadata":metadata,"provenance":provenance}))
}
pub fn row_to_case(row: &Value) -> Value {
    let mut metadata = row["metadata"].as_object().cloned().unwrap_or_default();
    metadata.insert("split".into(), row["split"].clone());
    if row["provenance"].as_str().is_some_and(|v| !v.is_empty()) {
        metadata
            .entry("provenance")
            .or_insert_with(|| row["provenance"].clone());
    }
    let mut case = json!({"case_id":row["case_id"],"track":row["track"],"query":row["query"],"relevance":row["relevance"],"metadata":metadata});
    if row["reference_answer"]
        .as_str()
        .is_some_and(|v| !v.is_empty())
    {
        case["reference_answer"] = row["reference_answer"].clone();
    }
    case
}
#[derive(Clone)]
pub struct GoldenStore {
    pub pool: PgPool,
}
impl GoldenStore {
    pub async fn import_cases(
        &self,
        cases: &[Value],
        version: &str,
        default_split: &str,
        provenance: Option<&str>,
    ) -> Result<Value> {
        if cases.is_empty() {
            return fail("import_cases requires at least one case");
        }
        let rows: Vec<_> = cases
            .iter()
            .map(|c| case_to_row(c, version, default_split, provenance))
            .collect::<Result<_>>()?;
        let mut seen = BTreeSet::new();
        for row in &rows {
            if !seen.insert(row["case_id"].as_str().unwrap()) {
                return fail("duplicate case_id in one import");
            }
        }
        let mut tx = self.pool.begin().await?;
        for row in &rows {
            sqlx::query("INSERT INTO kb_eval_golden (case_id,version,track,query,relevance,reference_answer,split,metadata,provenance) VALUES ($1,$2,$3,$4,$5::jsonb,$6,$7,$8::jsonb,$9) ON CONFLICT (version,case_id) DO UPDATE SET track=EXCLUDED.track,query=EXCLUDED.query,relevance=EXCLUDED.relevance,reference_answer=EXCLUDED.reference_answer,split=EXCLUDED.split,metadata=EXCLUDED.metadata,provenance=EXCLUDED.provenance,updated_at=NOW()")
            .bind(row["case_id"].as_str()).bind(row["version"].as_str()).bind(row["track"].as_str()).bind(row["query"].as_str()).bind(&row["relevance"]).bind(row["reference_answer"].as_str()).bind(row["split"].as_str()).bind(&row["metadata"]).bind(row["provenance"].as_str()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(
            json!({"imported":rows.len(),"frozen":rows.iter().filter(|r|r["split"]=="frozen").count(),"growth":rows.iter().filter(|r|r["split"]=="growth").count()}),
        )
    }
    pub async fn get_release(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT version FROM kb_eval_golden_release WHERE release_key=$1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
    pub async fn count_cases(&self, version: &str, split: Option<&str>) -> Result<i64> {
        if split.is_some_and(|s| !split_valid(s)) {
            return fail("invalid split");
        }
        Ok(sqlx::query_scalar("SELECT count(*) FROM kb_eval_golden WHERE version=$1 AND ($2::text IS NULL OR split=$2)").bind(clean_version(version)?).bind(split).fetch_one(&self.pool).await?)
    }
    pub async fn pin_release(&self, version: &str, key: &str, note: &str) -> Result<()> {
        let version = clean_version(version)?;
        if key.trim().is_empty() || self.count_cases(version, None).await? == 0 {
            return fail("cannot pin empty golden version or release key");
        }
        sqlx::query("INSERT INTO kb_eval_golden_release (release_key,version,note,set_at) VALUES ($1,$2,$3,NOW()) ON CONFLICT (release_key) DO UPDATE SET version=EXCLUDED.version,note=EXCLUDED.note,set_at=NOW()").bind(key.trim()).bind(version).bind(note).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn list_cases(
        &self,
        version: Option<&str>,
        split: Option<&str>,
        track: Option<&str>,
        release_key: &str,
    ) -> Result<Vec<Value>> {
        if split.is_some_and(|s| !split_valid(s))
            || track.is_some_and(|t| !matches!(t, "retrieval_only" | "answer_aware"))
        {
            return fail("invalid golden split or track");
        }
        let version = match version {
            Some(v) => clean_version(v)?.to_owned(),
            None => self
                .get_release(release_key)
                .await?
                .ok_or_else(|| super::error("no golden version pinned"))?,
        };
        let rows=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(g) FROM kb_eval_golden g WHERE version=$1 AND ($2::text IS NULL OR split=$2) AND ($3::text IS NULL OR track=$3) ORDER BY case_id").bind(version).bind(split).bind(track).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(row_to_case).collect())
    }
    pub async fn set_split(
        &self,
        version: &str,
        case_ids: &[String],
        split: &str,
    ) -> Result<usize> {
        if !split_valid(split) {
            return fail("invalid golden split");
        }
        let version = clean_version(version)?;
        let ids: Vec<String> = case_ids
            .iter()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if ids.is_empty() {
            return fail("set_split requires case ids");
        }
        let mut tx = self.pool.begin().await?;
        let present = sqlx::query(
            "SELECT case_id FROM kb_eval_golden WHERE version=$1 AND case_id=ANY($2::text[])",
        )
        .bind(version)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        let present: BTreeSet<String> = present.iter().map(|r| r.get("case_id")).collect();
        if ids.iter().any(|id| !present.contains(id)) {
            return fail("set_split case ids absent from version");
        }
        sqlx::query("UPDATE kb_eval_golden SET split=$3,updated_at=NOW() WHERE version=$1 AND case_id=ANY($2::text[])").bind(version).bind(&ids).bind(split).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ids.len())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_fail_closed() {
        let cases = vec![json!({"metadata":{"version":"v1"}})];
        assert_eq!(resolve_version(&cases, None).unwrap(), "v1");
        assert!(resolve_version(&cases, Some("v2")).is_err());
        assert!(resolve_version(&[], None).is_err());
    }
    #[test]
    fn mapping_roundtrips_promoted_metadata() {
        let case = json!({"case_id":"x","track":"answer_aware","query":" query ","relevance":{"a":3},"reference_answer":"answer","metadata":{"split":"frozen","provenance":"human"}});
        let row = case_to_row(&case, "v1", "growth", None).unwrap();
        let back = row_to_case(&row);
        assert_eq!(back["metadata"], case["metadata"]);
        assert_eq!(back["query"], "query");
        assert_eq!(row["split"], "frozen");
    }
    #[test]
    fn boolean_grades_are_rejected() {
        assert!(
            case_to_row(
                &json!({"case_id":"x","track":"retrieval_only","query":"q","relevance":{"a":true}}),
                "v1",
                "growth",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn numeric_metadata_version_is_not_ignored_during_resolution() {
        let cases = vec![json!({"metadata":{"version":7}})];
        assert_eq!(resolve_version(&cases, None).unwrap(), "7");
        assert!(resolve_version(&cases, Some("v1")).is_err());
    }

    #[test]
    fn malformed_split_and_reference_answer_are_not_silently_projected() {
        let base = json!({
            "case_id":"x",
            "track":"retrieval_only",
            "query":"q",
            "relevance":{"a":1},
            "metadata":{}
        });
        let mut bad_split = base.clone();
        bad_split["metadata"]["split"] = json!(1);
        assert!(case_to_row(&bad_split, "v1", "growth", None).is_err());

        let mut bad_reference = base;
        bad_reference["reference_answer"] = json!({"answer":"a"});
        assert!(case_to_row(&bad_reference, "v1", "growth", None).is_err());
    }
}
