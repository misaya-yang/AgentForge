//! Exact, resumable shadow BM25 v2 backfill. PostgreSQL owns the manifest;
//! Qdrant mutates only bm25_v2, _lexical and the completion receipt.
use super::{Args, Result, error, fail};
use ai_platform_eval::canonical::{digest, sha256};
use ai_platform_knowledge_service::migration_knowledge_query::lexical_v2::{
    LexicalProfile, validate_collection_profile,
};
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, Row};
use std::collections::BTreeSet;
const AUTHORITY: &str = "postgres-segments-enabled-l3-text-base-v2";
const IDS_ALGORITHM: &str = "sha256(sorted-point-id-newline-v1)";
const TEXT_ALGORITHM: &str = "sha256(sorted-point-id-text-sha256-null-newline-v1)";
const RECEIPT: &str = "knowledge_bm25_v2_backfill";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub collection_name: String,
    pub tenant_id: String,
    pub dataset_id: String,
    pub bm25_v2_schema_fingerprint: String,
    pub filtering_profile_fingerprint: String,
    pub lexical_metadata_sha256: String,
    pub authority_kind: String,
    pub authority_content_revision: u64,
    pub point_count: usize,
    pub point_ids_sha256: String,
    pub source_text_sha256: String,
    pub manifest_sha256: String,
    pub schema_version: u64,
    pub kind: String,
    pub manifest_algorithm: String,
    pub source_text_algorithm: String,
}
impl Manifest {
    fn unsigned(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap();
        value.as_object_mut().unwrap().remove("manifest_sha256");
        value
    }
    fn expected_hash(&self) -> String {
        format!("sha256:{}", digest(&self.unsigned()))
    }
    pub fn validate(&self) -> Result<()> {
        for v in [&self.collection_name, &self.tenant_id, &self.dataset_id] {
            if v.trim().is_empty() || v.trim() != v {
                return fail("manifest scope must be nonempty without surrounding whitespace");
            }
        }
        let hex = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        for v in [
            &self.bm25_v2_schema_fingerprint,
            &self.filtering_profile_fingerprint,
            &self.lexical_metadata_sha256,
            &self.manifest_sha256,
        ] {
            if !v.strip_prefix("sha256:").is_some_and(hex) {
                return fail("manifest fingerprint invalid");
            }
        }
        if !hex(&self.point_ids_sha256)
            || !hex(&self.source_text_sha256)
            || self.schema_version != 2
            || self.kind != "knowledge-bm25-v2-shadow-backfill"
            || self.authority_kind != AUTHORITY
            || self.manifest_algorithm != IDS_ALGORITHM
            || self.source_text_algorithm != TEXT_ALGORITHM
            || self.manifest_sha256 != self.expected_hash()
        {
            return fail("manifest schema or signature invalid");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    revision: u64,
    count: usize,
    ids: String,
    text: String,
}
#[derive(Clone)]
struct Scan {
    snapshot: Snapshot,
    complete: usize,
}
fn hashes(entries: &[(String, String)], revision: u64) -> Snapshot {
    let mut entries = entries.to_vec();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let ids = entries
        .iter()
        .map(|e| format!("{}\n", e.0))
        .collect::<String>();
    let text = entries
        .iter()
        .map(|e| format!("{}\0{}\n", e.0, sha256(e.1.as_bytes())))
        .collect::<String>();
    Snapshot {
        revision,
        count: entries.len(),
        ids: sha256(ids.as_bytes()),
        text: sha256(text.as_bytes()),
    }
}
async fn authority(dsn: &str, collection: &str, tenant: &str, dataset: &str) -> Result<Snapshot> {
    let result:Result<Snapshot>=async{let mut conn=sqlx::PgConnection::connect(dsn).await?;let mut tx=conn.begin().await?;sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
 let row=sqlx::query("SELECT tenant_id,collection_name,content_revision,index_config FROM datasets WHERE dataset_id=$1 AND is_deleted=FALSE").bind(dataset).fetch_optional(&mut *tx).await?.ok_or_else(||error("authoritative dataset missing or deleted"))?;
 if row.try_get::<Option<String>,_>("tenant_id")?.as_deref()!=Some(tenant)||row.try_get::<Option<String>,_>("collection_name")?.as_deref()!=Some(collection){return fail("authoritative dataset scope mismatch");}
 let config:Option<Value>=row.try_get("index_config")?;if config.as_ref().and_then(|v|v.get("retrieval")).and_then(|v|v.get("_index_deletion_fence")).is_some(){return fail("authoritative dataset deletion is pending");}
 let revision=row.try_get::<Option<i64>,_>("content_revision")?.unwrap_or(0);if revision<0{return fail("invalid authority content revision");}
 let rows=sqlx::query(include_str!("backfill_authority.sql")).bind(dataset).bind(tenant).bind(collection).fetch_all(&mut *tx).await?;let mut entries=vec![];let mut seen=BTreeSet::new();for row in rows{let id:String=row.try_get("point_id")?;if !seen.insert(id.clone()){return fail("authoritative segments have duplicate vector IDs");}entries.push((id,row.try_get::<Option<String>,_>("text")?.unwrap_or_default()));}tx.commit().await?;Ok(hashes(&entries,revision as u64))}.await;
    result.map_err(|_| error("PostgreSQL authority query or scope validation failed"))
}
struct Remote {
    client: Client,
    url: Url,
    key: Option<String>,
    collection: String,
    tenant: String,
    dataset: String,
    batch: usize,
}
impl Remote {
    async fn call(
        &self,
        method: Method,
        path: &[&str],
        body: Option<&Value>,
        wait: bool,
    ) -> Result<Value> {
        let mut url = self.url.clone();
        {
            let mut parts = url
                .path_segments_mut()
                .map_err(|_| error("invalid Qdrant URL"))?;
            parts
                .pop_if_empty()
                .push("collections")
                .push(&self.collection);
            for p in path {
                parts.push(p);
            }
        }
        if wait {
            url.query_pairs_mut().append_pair("wait", "true");
        }
        let mut request = self.client.request(method, url);
        if let Some(key) = &self.key {
            request = request.header("api-key", key);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| error("Qdrant request failed"))?
            .error_for_status()
            .map_err(|_| error("Qdrant rejected request"))?;
        let value: Value = response
            .json()
            .await
            .map_err(|_| error("Qdrant response invalid"))?;
        value
            .get("result")
            .cloned()
            .ok_or_else(|| error("Qdrant result missing"))
    }
    async fn info(&self) -> Result<Value> {
        self.call(Method::GET, &[], None, false).await
    }
    fn scope(&self) -> Value {
        json!({"must":[{"key":"tenant_id","match":{"value":self.tenant}},{"key":"dataset_id","match":{"value":self.dataset}}]})
    }
    fn lexical_scope(&self) -> Value {
        let mut scope = self.scope();
        for (key, value) in [
            ("content_type", json!("text")),
            ("level", json!(3)),
            ("enabled", json!(true)),
        ] {
            scope["must"].as_array_mut().unwrap().push(
                json!({"should":[{"key":key,"match":{"value":value}},{"is_empty":{"key":key}}]}),
            );
        }
        scope
    }
    fn profile(&self, info: &Value, expected: Option<&Manifest>) -> Result<LexicalProfile> {
        let metadata = &info["config"]["metadata"];
        if metadata["knowledge_scope"]
            != json!({"schema_version":1,"tenant_id":self.tenant,"dataset_id":self.dataset})
        {
            return fail("collection immutable scope mismatch");
        }
        let raw = &metadata["knowledge_lexical"];
        validate_collection_profile(raw)?;
        let profile = LexicalProfile::from_collection(raw)?;
        if profile.active || !profile.shadow {
            return fail("backfill requires lexical_v1 serving and shadow writes enabled");
        }
        if info["config"]["params"]["sparse_vectors"]["bm25_v2"]["modifier"] != "idf" {
            return fail("bm25_v2 sparse field requires IDF");
        }
        for field in ["tenant_id", "dataset_id"] {
            let index = &info["payload_schema"][field];
            if index["data_type"] != "keyword" && index != "keyword" {
                return fail("required keyword payload index absent");
            }
            if field == "tenant_id" && index["params"]["is_tenant"] != true {
                return fail("tenant index requires tenant partitioning");
            }
        }
        if let Some(expected) = expected {
            if expected.bm25_v2_schema_fingerprint != profile.schema_fingerprint()
                || expected.filtering_profile_fingerprint != profile.filtering_fingerprint()
                || expected.lexical_metadata_sha256 != format!("sha256:{}", digest(raw))
            {
                return fail("collection profile no longer matches manifest");
            }
        }
        Ok(profile)
    }
    async fn probe(&self, profile: &LexicalProfile) -> Result<()> {
        self.call(Method::POST,&["points","query"],Some(&json!({"query":profile.document("bm25 v2 read-only capability probe"),"using":"bm25_v2","filter":self.scope(),"limit":1,"with_payload":false})),false).await?;
        Ok(())
    }
    fn point(&self, record: &Value, profile: &LexicalProfile) -> Result<(String, bool)> {
        let p = &record["payload"];
        if p["tenant_id"] != self.tenant || p["dataset_id"] != self.dataset {
            return fail("point escaped requested scope");
        }
        for (key, expected) in [
            ("content_type", json!("text")),
            ("level", json!(3)),
            ("enabled", json!(true)),
        ] {
            if !p[key].is_null() && p[key] != expected {
                return fail("point escaped enabled L3 text scope");
            }
        }
        let text = p["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| error("point text missing or empty"))?;
        let receipt = &p["_lexical"];
        for (key, expected) in [
            ("bm25_v2_schema_fingerprint", profile.schema_fingerprint()),
            (
                "filtering_profile_fingerprint",
                profile.filtering_fingerprint(),
            ),
        ] {
            if !receipt[key].is_null() && receipt[key] != expected {
                return fail("point has conflicting lexical fingerprint");
            }
        }
        let versions = receipt["versions"].as_array();
        let complete = record["vector"].get("bm25_v2").is_some()
            && receipt["bm25_v2_schema_fingerprint"] == profile.schema_fingerprint()
            && receipt["filtering_profile_fingerprint"] == profile.filtering_fingerprint()
            && receipt["source_text_sha256"] == sha256(text.as_bytes())
            && versions
                .is_some_and(|v| v.contains(&json!("lexical_v1")) && v.contains(&json!("bm25_v2")));
        Ok((text.into(), complete))
    }
    async fn page(&self, offset: &Value) -> Result<(Vec<Value>, Value)> {
        let result=self.call(Method::POST,&["points","scroll"],Some(&json!({"filter":self.lexical_scope(),"limit":self.batch,"offset":offset,"with_payload":["tenant_id","dataset_id","content_type","level","enabled","text","_lexical"],"with_vector":["bm25_v2"]})),false).await?;
        Ok((
            result["points"]
                .as_array()
                .cloned()
                .ok_or_else(|| error("scroll points missing"))?,
            result["next_page_offset"].clone(),
        ))
    }
    async fn scan(&self, profile: &LexicalProfile, revision: u64) -> Result<Scan> {
        let mut offset = Value::Null;
        let mut entries = vec![];
        let mut seen = BTreeSet::new();
        let mut offsets = BTreeSet::new();
        let mut complete = 0;
        loop {
            let (records, next) = self.page(&offset).await?;
            for record in &records {
                let raw = &record["id"];
                let id = raw
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| raw.as_u64().map(|v| v.to_string()))
                    .ok_or_else(|| error("point ID invalid"))?;
                if !seen.insert(raw.to_string()) {
                    return fail("duplicate point ID in scroll");
                }
                let (text, done) = self.point(record, profile)?;
                entries.push((id, text));
                complete += usize::from(done);
            }
            if next.is_null() {
                break;
            }
            if records.is_empty() || next == offset || !offsets.insert(next.to_string()) {
                return fail("scroll failed to make progress");
            }
            offset = next;
        }
        Ok(Scan {
            snapshot: hashes(&entries, revision),
            complete,
        })
    }
    async fn count(&self, filter: Option<Value>) -> Result<u64> {
        let mut body = json!({"exact":true});
        if let Some(filter) = filter {
            body["filter"] = filter;
        }
        self.call(Method::POST, &["points", "count"], Some(&body), false)
            .await?["count"]
            .as_u64()
            .ok_or_else(|| error("exact count missing"))
    }
    async fn exact_counts(&self, count: usize) -> Result<()> {
        let dataset = self
            .count(Some(
                json!({"must":[{"key":"dataset_id","match":{"value":self.dataset}}]}),
            ))
            .await?;
        let scope = self.count(Some(self.scope())).await?;
        let lexical = self.count(Some(self.lexical_scope())).await?;
        let all = self.count(None).await?;
        if all != dataset || dataset != scope || lexical != count as u64 {
            return fail("exact scope counts mismatch; refusing partial collection receipt");
        }
        Ok(())
    }
    async fn clear(&self) -> Result<()> {
        let info = self.info().await?;
        self.profile(&info, None)?;
        if info["config"]["metadata"][RECEIPT]["status"] != "complete" {
            return Ok(());
        }
        let invalid = json!({"schema_version":1,"status":"invalidated","reason":"backfill_apply"});
        self.set_receipt(&invalid).await
    }
    async fn set_receipt(&self, receipt: &Value) -> Result<()> {
        let updated = self
            .call(
                Method::PATCH,
                &[],
                Some(&json!({"metadata":{RECEIPT:receipt}})),
                false,
            )
            .await?;
        if updated != true {
            return fail("Qdrant rejected collection receipt");
        }
        if self.info().await?["config"]["metadata"][RECEIPT] != *receipt {
            return fail("collection receipt did not converge");
        }
        Ok(())
    }
    async fn apply(&self, profile: &LexicalProfile) -> Result<usize> {
        let mut offset = Value::Null;
        let mut written = 0;
        let mut offsets = BTreeSet::new();
        loop {
            let (records, next) = self.page(&offset).await?;
            let mut pending = vec![];
            for record in &records {
                let (text, complete) = self.point(record, profile)?;
                if !complete {
                    pending.push((record, text));
                }
            }
            if !pending.is_empty() {
                let points:Vec<_>=pending.iter().map(|(r,text)|json!({"id":r["id"],"vector":{"bm25_v2":profile.document(text)}})).collect();
                let result = self
                    .call(
                        Method::PUT,
                        &["points", "vectors"],
                        Some(&json!({"points":points})),
                        true,
                    )
                    .await?;
                if result["status"] != "completed" {
                    return fail("vector update not completed");
                }
                for (record, text) in &pending {
                    let mut marker = record["payload"]["_lexical"]
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    let mut versions = marker
                        .get("versions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    for v in [json!("lexical_v1"), json!("bm25_v2")] {
                        if !versions.contains(&v) {
                            versions.push(v);
                        }
                    }
                    for (k, v) in profile.marker(text).as_object().unwrap() {
                        marker.insert(k.clone(), v.clone());
                    }
                    marker.insert("versions".into(), json!(versions));
                    let result = self
                        .call(
                            Method::POST,
                            &["points", "payload"],
                            Some(&json!({"payload":{"_lexical":marker},"points":[record["id"]]})),
                            true,
                        )
                        .await?;
                    if result["status"] != "completed" {
                        return fail("point marker update not completed");
                    }
                }
                written += pending.len();
            }
            if next.is_null() {
                break;
            }
            if records.is_empty() || next == offset || !offsets.insert(next.to_string()) {
                return fail("apply scroll failed to make progress");
            }
            offset = next;
        }
        Ok(written)
    }
}
fn matches_manifest(snapshot: &Snapshot, m: &Manifest) -> bool {
    snapshot.revision == m.authority_content_revision
        && snapshot.count == m.point_count
        && snapshot.ids == m.point_ids_sha256
        && snapshot.text == m.source_text_sha256
}
fn receipt(m: &Manifest) -> Value {
    let mut r = m.unsigned();
    for k in ["lexical_metadata_sha256", "kind"] {
        r.as_object_mut().unwrap().remove(k);
    }
    r["schema_version"] = json!(1);
    r["status"] = json!("complete");
    r
}
async fn execute(remote: &Remote, dsn: &str, m: &Manifest, apply: bool) -> Result<Value> {
    m.validate()?;
    let before = authority(dsn, &m.collection_name, &m.tenant_id, &m.dataset_id).await?;
    if !matches_manifest(&before, m) {
        return fail("authority no longer matches manifest");
    }
    let info = remote.info().await?;
    let profile = remote.profile(&info, Some(m))?;
    remote.probe(&profile).await?;
    let scan = remote.scan(&profile, before.revision).await?;
    remote.exact_counts(scan.snapshot.count).await?;
    if scan.snapshot != before
        || authority(dsn, &m.collection_name, &m.tenant_id, &m.dataset_id).await? != before
    {
        return fail("authority or collection changed during scan");
    }
    if !apply {
        return Ok(
            json!({"schema_version":1,"status":"dry_run","writes_performed":0,"collection_name":m.collection_name,"tenant_id":m.tenant_id,"dataset_id":m.dataset_id,"point_count":scan.snapshot.count,"complete_points":scan.complete,"pending_points":scan.snapshot.count-scan.complete,"manifest_sha256":m.manifest_sha256}),
        );
    }
    remote.clear().await?;
    let written = remote.apply(&profile).await?;
    let after = remote.scan(&profile, before.revision).await?;
    remote.exact_counts(after.snapshot.count).await?;
    if after.snapshot != before
        || after.complete != m.point_count
        || authority(dsn, &m.collection_name, &m.tenant_id, &m.dataset_id).await? != before
    {
        return fail("post-apply exact coverage or authority changed");
    }
    let completed = receipt(m);
    let publish: Result<()> = async {
        remote.profile(&remote.info().await?, Some(m))?;
        remote.set_receipt(&completed).await?;
        remote.profile(&remote.info().await?, Some(m))?;
        let final_scan = remote.scan(&profile, before.revision).await?;
        remote.exact_counts(final_scan.snapshot.count).await?;
        if final_scan.snapshot != before
            || final_scan.complete != m.point_count
            || authority(dsn, &m.collection_name, &m.tenant_id, &m.dataset_id).await? != before
            || remote.info().await?["config"]["metadata"][RECEIPT] != completed
        {
            return fail("state changed after completion receipt publication");
        }
        Ok(())
    }
    .await;
    if let Err(error) = publish {
        remote.clear().await?;
        return Err(error);
    }
    let mut result = completed;
    result["exact"] = json!(true);
    result["coverage_percent"] = json!(100.);
    result["points_written_this_run"] = json!(written);
    result["manifest_sha256"] = json!(m.manifest_sha256);
    Ok(result)
}
pub async fn run(args: &[String]) -> Result<()> {
    let args = Args::parse(args, &["dry-run", "apply"])?;
    args.validate(
        &[
            "batch-size",
            "timeout",
            "database-dsn-env",
            "api-key-env",
            "qdrant-url",
            "manifest",
            "confirm-manifest-sha256",
            "collection",
            "tenant-id",
            "dataset-id",
        ],
        1,
    )?;
    if args.has("dry-run") && args.has("apply") {
        return fail("--dry-run and --apply are mutually exclusive");
    }
    let command = args.positional.first().map(String::as_str).unwrap_or("");
    if !["plan", "run"].contains(&command) {
        return fail("backfill-bm25-v2 requires plan or run");
    }
    let batch = args.get("batch-size", "256").parse::<usize>()?;
    let timeout = args.get("timeout", "120").parse::<u64>()?;
    if !(1..=1000).contains(&batch) || !(1..=3600).contains(&timeout) {
        return fail("invalid batch-size or timeout");
    }
    let env = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
    let dsn_key = args.get("database-dsn-env", "KNOWLEDGE_DATABASE__DSN");
    let key_name = args.get("api-key-env", "QDRANT_API_KEY");
    for name in [dsn_key, key_name] {
        if name.is_empty()
            || !name
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
        {
            return fail("invalid environment variable name");
        }
    }
    let dsn = env
        .get(dsn_key)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| error("PostgreSQL authority DSN environment variable missing"))?;
    let url = Url::parse(
        args.get(
            "qdrant-url",
            env.get("QDRANT_URL")
                .map(String::as_str)
                .unwrap_or("http://localhost:6333"),
        ),
    )?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return fail("Qdrant URL must be absolute HTTP(S) without credentials");
    }
    let manifest = if command == "run" {
        let path = args
            .options
            .get("manifest")
            .ok_or_else(|| error("--manifest required"))?;
        let m: Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
        m.validate()?;
        if args.has("apply") && args.get("confirm-manifest-sha256", "") != m.manifest_sha256 {
            return fail("--apply requires exact --confirm-manifest-sha256");
        }
        if !args.has("apply") && args.options.contains_key("confirm-manifest-sha256") {
            return fail("manifest confirmation only valid with --apply");
        }
        Some(m)
    } else {
        None
    };
    let required = |key: &str| {
        args.options
            .get(key)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| error("plan requires collection, tenant-id and dataset-id"))
    };
    let (collection, tenant, dataset) = if let Some(m) = &manifest {
        (
            m.collection_name.clone(),
            m.tenant_id.clone(),
            m.dataset_id.clone(),
        )
    } else {
        (
            required("collection")?,
            required("tenant-id")?,
            required("dataset-id")?,
        )
    };
    let remote = Remote {
        client: Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .build()?,
        url,
        key: env.get(key_name).filter(|s| !s.is_empty()).cloned(),
        collection,
        tenant,
        dataset,
        batch,
    };
    let output = if let Some(m) = manifest {
        execute(&remote, dsn, &m, args.has("apply")).await?
    } else {
        let before = authority(dsn, &remote.collection, &remote.tenant, &remote.dataset).await?;
        let info = remote.info().await?;
        let profile = remote.profile(&info, None)?;
        remote.probe(&profile).await?;
        let scan = remote.scan(&profile, before.revision).await?;
        remote.exact_counts(scan.snapshot.count).await?;
        if scan.snapshot != before
            || authority(dsn, &remote.collection, &remote.tenant, &remote.dataset).await? != before
        {
            return fail("authority or collection changed while planning");
        }
        let mut m = Manifest {
            collection_name: remote.collection.clone(),
            tenant_id: remote.tenant.clone(),
            dataset_id: remote.dataset.clone(),
            bm25_v2_schema_fingerprint: profile.schema_fingerprint(),
            filtering_profile_fingerprint: profile.filtering_fingerprint(),
            lexical_metadata_sha256: format!(
                "sha256:{}",
                digest(&info["config"]["metadata"]["knowledge_lexical"])
            ),
            authority_kind: AUTHORITY.into(),
            authority_content_revision: before.revision,
            point_count: before.count,
            point_ids_sha256: before.ids,
            source_text_sha256: before.text,
            manifest_sha256: String::new(),
            schema_version: 2,
            kind: "knowledge-bm25-v2-shadow-backfill".into(),
            manifest_algorithm: IDS_ALGORITHM.into(),
            source_text_algorithm: TEXT_ALGORITHM.into(),
        };
        m.manifest_sha256 = m.expected_hash();
        m.validate()?;
        serde_json::to_value(m)?
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_manifest() -> Manifest {
        let empty = hashes(&[], 7);
        let mut manifest = Manifest {
            collection_name: "kb_test".into(),
            tenant_id: "tenant".into(),
            dataset_id: "dataset".into(),
            bm25_v2_schema_fingerprint: format!("sha256:{}", "a".repeat(64)),
            filtering_profile_fingerprint: format!("sha256:{}", "b".repeat(64)),
            lexical_metadata_sha256: format!("sha256:{}", "c".repeat(64)),
            authority_kind: AUTHORITY.into(),
            authority_content_revision: empty.revision,
            point_count: 0,
            point_ids_sha256: empty.ids,
            source_text_sha256: empty.text,
            manifest_sha256: String::new(),
            schema_version: 2,
            kind: "knowledge-bm25-v2-shadow-backfill".into(),
            manifest_algorithm: IDS_ALGORITHM.into(),
            source_text_algorithm: TEXT_ALGORITHM.into(),
        };
        manifest.manifest_sha256 = manifest.expected_hash();
        manifest
    }
    #[test]
    fn manifest_rejects_scope_or_revision_tampering() {
        let mut manifest = fixture_manifest();
        manifest.validate().unwrap();
        manifest.dataset_id = "other".into();
        assert!(manifest.validate().is_err());
        let mut manifest = fixture_manifest();
        manifest.authority_content_revision += 1;
        assert!(manifest.validate().is_err());
    }
    #[test]
    fn manifest_rejects_unknown_fields_and_boolean_counts() {
        let mut value = serde_json::to_value(fixture_manifest()).unwrap();
        value["unknown"] = json!(true);
        assert!(serde_json::from_value::<Manifest>(value).is_err());
        let mut value = serde_json::to_value(fixture_manifest()).unwrap();
        value["point_count"] = json!(false);
        assert!(serde_json::from_value::<Manifest>(value).is_err());
    }
    #[test]
    fn completion_receipt_is_source_compatible() {
        let receipt = receipt(&fixture_manifest());
        assert_eq!(receipt["status"], "complete");
        assert_eq!(receipt["schema_version"], 1);
        assert!(receipt.get("manifest_sha256").is_none());
        assert!(receipt.get("lexical_metadata_sha256").is_none());
        assert_eq!(receipt["authority_kind"], AUTHORITY);
    }
    #[test]
    fn manifest_digests_preserve_boundaries_and_order() {
        let a = hashes(&[("b".into(), "x".into()), ("a".into(), "y".into())], 1);
        let b = hashes(&[("a".into(), "y".into()), ("b".into(), "x".into())], 1);
        assert_eq!(a, b);
        assert_ne!(
            a.text,
            hashes(&[("a".into(), "x".into()), ("b".into(), "y".into())], 1).text
        );
    }
    #[tokio::test]
    async fn apply_without_manifest_confirmation_never_reaches_transport() {
        let args = vec![
            "run".into(),
            "--apply".into(),
            "--database-dsn-env".into(),
            "INVALID-NAME".into(),
        ];
        assert!(run(&args).await.is_err());
    }
}
