//! Dataset catalog, config, aggregates, debug and canonical dedupe maintenance.
use super::{
    Actor, Dataset, IngestError, IngestionService, Result, integer, object, require_index_writable,
    string,
};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::BTreeMap;
fn zero_stats() -> Value {
    json!({"document_count":0,"segment_count":0,"available_document_count":0,"available_segment_count":0,"word_count":0,"hit_count":0})
}
pub fn sanitize_dataset(value: &Value) -> Value {
    let mut value = value.clone();
    for key in ["embedding_config", "index_config"] {
        value[key] = super::service::redact_server_config(&object(&value[key]));
    }
    value
}
fn permission(rank: i32) -> Value {
    match rank {
        3 => json!("owner"),
        2 => json!("editor"),
        1 => json!("viewer"),
        _ => Value::Null,
    }
}
impl IngestionService {
    async fn statistics_batch(&self, ids: &[String]) -> Result<BTreeMap<String, Value>> {
        let rows:Vec<Value>=sqlx::query_scalar("WITH document_stats AS(SELECT dataset_id,COUNT(*)::bigint AS document_count,COUNT(*) FILTER(WHERE status='completed' AND enabled=TRUE AND archived=FALSE)::bigint AS available_document_count,COALESCE(SUM(word_count),0)::bigint AS word_count FROM knowledge.documents WHERE dataset_id=ANY($1) GROUP BY dataset_id),segment_stats AS(SELECT dataset_id,COUNT(*)::bigint AS segment_count,COUNT(*) FILTER(WHERE enabled=TRUE AND status='completed')::bigint AS available_segment_count,COALESCE(SUM(hit_count),0)::bigint AS hit_count FROM knowledge.segments WHERE dataset_id=ANY($1) GROUP BY dataset_id) SELECT to_jsonb(result) FROM(SELECT d.dataset_id,COALESCE(doc.document_count,0) AS document_count,COALESCE(doc.available_document_count,0) AS available_document_count,COALESCE(doc.word_count,0) AS word_count,COALESCE(seg.segment_count,0) AS segment_count,COALESCE(seg.available_segment_count,0) AS available_segment_count,COALESCE(seg.hit_count,0) AS hit_count FROM knowledge.datasets d LEFT JOIN document_stats doc ON doc.dataset_id=d.dataset_id LEFT JOIN segment_stats seg ON seg.dataset_id=d.dataset_id WHERE d.dataset_id=ANY($1) AND d.is_deleted=FALSE) result").bind(ids).fetch_all(crate::database::executor(&self.store.pool)).await?;
        let mut result: BTreeMap<String, Value> =
            ids.iter().map(|id| (id.clone(), zero_stats())).collect();
        for mut row in rows {
            let id = string(&row, "dataset_id").to_owned();
            row.as_object_mut().unwrap().remove("dataset_id");
            result.insert(id, row);
        }
        Ok(result)
    }
    pub async fn list_datasets_page(
        &self,
        actor: &Actor,
        limit: i64,
        cursor: Option<&str>,
        archived: bool,
    ) -> Result<Value> {
        if archived && (actor.anonymous() || actor.roles.iter().any(|r| r == "guest")) {
            return Err(IngestError::Permission);
        }
        if !(1..=200).contains(&limit) {
            return Err(IngestError::Validation(
                "dataset page limit must be in 1..200".into(),
            ));
        }
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT to_jsonb(d) FROM knowledge.datasets d WHERE is_deleted=FALSE AND is_archived=",
        );
        query.push_bind(archived);
        if !actor.tenant_id.is_empty() {
            query
                .push(" AND (tenant_id=")
                .push_bind(actor.tenant_id.clone());
            if !archived {
                query.push(" OR visibility='public'");
            }
            query.push(")");
        } else if archived {
            query.push(" AND visibility!='public'");
        }
        if let Some(cursor) = cursor {
            let bad = || IngestError::Validation("invalid dataset pagination cursor".into());
            if cursor.len() > 1024 {
                return Err(bad());
            }
            let (time, id) =
                crate::migration_knowledge_query::observability::decode_query_cursor(cursor)
                    .map_err(|_| bad())?;
            if id.chars().count() > 255 {
                return Err(bad());
            }
            query
                .push(" AND(created_at,dataset_id)<(")
                .push_bind(time)
                .push(",")
                .push_bind(id)
                .push(")");
        }
        query
            .push(" ORDER BY created_at DESC,dataset_id DESC LIMIT ")
            .push_bind(limit + 1)
            .push(" OFFSET 0");
        let mut rows: Vec<Value> = query
            .build_query_scalar()
            .fetch_all(crate::database::executor(&self.store.pool))
            .await?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next = if more {
            let row = rows.last().ok_or(IngestError::Database)?;
            Some(
                crate::migration_knowledge_query::observability::encode_query_cursor(
                    string(row, "created_at"),
                    string(row, "dataset_id"),
                )?,
            )
        } else {
            None
        };
        // Archive enumeration uses the existing owner-only policy on these rows.
        let auth_rows: Vec<Value> = rows
            .iter()
            .map(|row| {
                let mut auth_row = row.clone();
                if archived {
                    auth_row["is_archived"] = json!(false);
                }
                auth_row
            })
            .collect();
        let grants = self
            .store
            .catalog_permissions_batch(actor, &auth_rows)
            .await?;
        let cedar_ranks = if let Some(policy) = self
            .store
            .object_policy
            .clone()
            .or_else(crate::cedar_dataset_policy::current)
        {
            let checks = auth_rows
                .iter()
                .map(|row| {
                    let permissions = grants
                        .get(string(row, "dataset_id"))
                        .cloned()
                        .unwrap_or_default();
                    let rank =
                        super::store::dataset_permission_from_grants(actor, row, &permissions);
                    crate::cedar_dataset_policy::facts(row, permissions, rank)
                })
                .collect();
            Some(
                policy
                    .ranks(actor, checks)
                    .await
                    .map_err(|_| IngestError::Permission)?,
            )
        } else {
            None
        };
        let mut resource_index = 0usize;
        let mut visible = vec![];
        for (mut row, auth_row) in rows.into_iter().zip(auth_rows.iter()) {
            let permissions = grants
                .get(string(auth_row, "dataset_id"))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let rank = super::store::dataset_permission_from_grants(actor, auth_row, permissions);
            let rank = if let Some(ranks) = &cedar_ranks {
                *ranks.get(resource_index).ok_or(IngestError::Permission)?
            } else {
                rank
            };
            resource_index += 1;
            if rank >= if archived { 3 } else { 1 } {
                row["my_permission"] = permission(rank);
                visible.push(row);
            }
        }
        let ids: Vec<String> = visible
            .iter()
            .map(|d| string(d, "dataset_id").to_owned())
            .collect();
        let stats = self.statistics_batch(&ids).await.unwrap_or_default();
        for row in &mut visible {
            row["statistics"] = stats
                .get(string(row, "dataset_id"))
                .cloned()
                .unwrap_or_else(zero_stats);
            if let Some(hash) = revision_fingerprint(row) {
                row["revision_fingerprint"] = json!(hash);
            }
            *row = sanitize_dataset(row);
        }
        Ok(json!({"items":visible,"next_cursor":next}))
    }
    pub async fn get_dataset_response(&self, actor: &Actor, dataset: &str) -> Result<Value> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let rank = self
            .store
            .effective_dataset_permission(actor, &value)
            .await?;
        let mut value = sanitize_dataset(&value);
        value["my_permission"] = permission(rank);
        Ok(value)
    }
    pub async fn get_dataset_statistics(&self, actor: &Actor, dataset: &str) -> Result<Value> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let expected = super::document_reads::generation(&value)?;
        let mut stats = self
            .statistics_batch(&[dataset.to_owned()])
            .await?
            .remove(dataset)
            .unwrap_or_else(zero_stats);
        stats["dataset_id"] = json!(dataset);
        self.require_read_generation(actor, dataset, &expected)
            .await?;
        Ok(stats)
    }
    pub async fn debug_dataset(&self, actor: &Actor, dataset: &str) -> Result<Value> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let expected = super::document_reads::generation(&value)?;
        let stats = self.get_dataset_statistics(actor, dataset).await?;
        let candidates = self
            .store
            .list_segments_scoped(dataset, None, None, 100, 0, false, "")
            .await?;
        let typed: Dataset = serde_json::from_value(value.clone())?;
        let ids: Vec<_> = candidates
            .iter()
            .map(|r| string(r, "segment_id").to_owned())
            .collect();
        let active = self.indexer.store.active_segment_ids(&typed, &ids).await?;
        let samples:Vec<Value>=candidates.iter().filter(|r|active.contains(string(r,"segment_id"))).take(3).map(|r|json!({"segment_id":r["segment_id"],"document_id":r["document_id"],"text_preview":if string(r,"text").is_empty(){Value::Null}else{json!(format!("{}...",string(r,"text").chars().take(100).collect::<String>()))},"token_count":r["token_count"],"vector_id":r["vector_id"]})).collect();
        self.require_read_generation(actor, dataset, &expected)
            .await?;
        Ok(
            json!({"dataset":{"id":dataset,"name":value["name"],"embedding_provider":value["embedding_provider"],"embedding_model":value["embedding_model"],"embedding_dimension":value["embedding_dimension"],"collection_name":value["collection_name"]},"statistics":stats,"has_segments":!samples.is_empty(),"has_collection":!string(&value,"collection_name").is_empty(),"sample_segments":samples}),
        )
    }
    pub fn parsing_report(&self, index: &Value) -> Result<Value> {
        match super::structured::resolve_config(index)? {
            None => Ok(
                json!({"enabled":false,"source":if index.get("parsing").is_some(){"dataset"}else{"default"},"backends":[],"warnings":[]}),
            ),
            Some(config) => {
                let backends:Vec<_>=config.stages.iter().map(|stage|json!({"name":stage.backend,"available":stage.backend=="text_layer"||self.structured_backends.clients.contains_key(&stage.backend)})).collect();
                let warnings: Vec<_> = backends
                    .iter()
                    .filter(|b| b["available"] == false)
                    .map(|b| format!("{}: backend is not configured", string(b, "name")))
                    .collect();
                Ok(
                    json!({"enabled":true,"source":"dataset","cascade":config,"backends":backends,"warnings":warnings}),
                )
            }
        }
    }
    pub async fn get_dataset_config(&self, actor: &Actor, dataset: &str) -> Result<Value> {
        let d = sanitize_dataset(
            &self
                .store
                .require_dataset_access(actor, dataset, "viewer")
                .await?,
        );
        let config = object(&d["index_config"]);
        let report = self.parsing_report(&config)?;
        let stats = self
            .get_dataset_statistics(actor, dataset)
            .await
            .unwrap_or(json!({}));
        Ok(
            json!({"dataset_id":dataset,"chunking":config.get("chunking").cloned().unwrap_or(json!({"mode":"automatic","chunk_size":500,"chunk_overlap":50})),"retrieval":config.get("retrieval").cloned().unwrap_or(json!({"mode":"hybrid","top_k":5,"rerank":{"enabled":false},"mmr":{"enabled":false}})),"configuration_warnings":report["warnings"],"parsing":report,"embedding":{"provider":d["embedding_provider"],"model":d["embedding_model"],"dimension":d["embedding_dimension"],"collection_name":d["collection_name"]},"statistics":stats}),
        )
    }
    pub async fn update_dataset_config(
        &self,
        actor: &Actor,
        dataset: &str,
        input: &Value,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "owner")
            .await?;
        require_index_writable(&d)?;
        let mut config = object(&d["index_config"]);
        if let Some(chunking) = input.get("chunking_config").filter(|v| !v.is_null()) {
            let mut chunking = object(chunking);
            for (k, v) in [
                ("mode", json!("automatic")),
                ("chunk_size", json!(2000)),
                ("chunk_overlap", json!(300)),
                ("use_token_count", json!(true)),
                ("separator", json!("\n")),
                ("remove_extra_spaces", json!(true)),
                ("remove_urls_emails", json!(false)),
            ] {
                chunking
                    .as_object_mut()
                    .unwrap()
                    .entry(k.to_owned())
                    .or_insert(v);
            }
            chunking
                .as_object_mut()
                .unwrap()
                .retain(|_, v| !v.is_null());
            super::chunking::ChunkingConfig::from_value(&chunking)?;
            config["chunking"] = chunking;
        }
        if let Some(patch) = input.get("retrieval_config").filter(|v| !v.is_null()) {
            let mut retrieval = object(&config["retrieval"]);
            merge_patch(&mut retrieval, patch);
            config["retrieval"] = retrieval;
        }
        let mut changes = json!({"index_config":config});
        if let Some(dimension) = input.get("embedding_dimension").filter(|v| !v.is_null()) {
            if dimension != &d["embedding_dimension"] {
                let stats = self.get_dataset_statistics(actor, dataset).await?;
                let count = integer(&stats, "segment_count");
                if count > 0 {
                    return Err(IngestError::Validation(format!(
                        "Cannot change embedding dimension when {count} segments exist. Please create a new dataset or delete all existing documents first."
                    )));
                }
            }
        }
        for key in [
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
        ] {
            if let Some(value) = input.get(key).filter(|v| !v.is_null()) {
                changes[key] = value.clone();
            }
        }
        let updated = self.update_dataset(actor, dataset, &changes).await?;
        Ok(
            json!({"status":"success","dataset_id":dataset,"index_config":updated["index_config"],"embedding":{"provider":updated["embedding_provider"],"model":updated["embedding_model"],"dimension":updated["embedding_dimension"]}}),
        )
    }
    pub async fn dataset_sources(&self, actor: &Actor, dataset: &str) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let expected = super::document_reads::generation(&d)?;
        let counts:Vec<(Option<String>,i64)>=sqlx::query_as("SELECT source_type,COUNT(*) FROM knowledge.documents WHERE dataset_id=$1 GROUP BY source_type").bind(dataset).fetch_all(crate::database::executor(&self.store.pool)).await?;
        let counts: BTreeMap<_, _> = counts
            .into_iter()
            .map(|(key, n)| (key.unwrap_or_else(|| "unknown".into()), n))
            .collect();
        let mut bindings = vec![];
        let mut connector_status = "not_configured";
        if self.confluence_sources_enabled {
            let rows:std::result::Result<Vec<Value>,sqlx::Error>=sqlx::query_scalar("SELECT to_jsonb(binding) FROM knowledge.confluence_space_bindings binding WHERE dataset_id=$1 ORDER BY created_at DESC LIMIT 100").bind(dataset).fetch_all(crate::database::executor(&self.store.pool)).await;
            match rows {
                Ok(rows) => {
                    connector_status = "available";
                    for row in rows {
                        if actor.user_tier != "admin"
                            && row["owner_id"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .unwrap_or(string(&row, "created_by"))
                                != actor.user_id
                        {
                            continue;
                        }
                        bindings.push(json!({"binding_id":row["binding_id"],"space_name":row["space_name"],"page_count":integer(&row,"synced_page_count"),"status":row["status"],"last_success_at":row["last_sync_at"],"has_problem":!["completed","success"].contains(&string(&row,"status").to_lowercase().as_str())&&!string(&row,"last_error").is_empty()}));
                    }
                }
                Err(_) => connector_status = "unavailable",
            }
        }
        let result = json!({"file_uploads":{"count":counts.get("upload").copied().unwrap_or(0)},"url_imports":{"count":counts.get("url").copied().unwrap_or(0)},"confluence_bindings":bindings,"connector_status":connector_status,"total_documents":counts.values().sum::<i64>()});
        self.require_read_generation(actor, dataset, &expected)
            .await?;
        Ok(result)
    }
    pub async fn dedupe_segments(&self, actor: &Actor, dataset: &str, dry: bool) -> Result<Value> {
        let mut d = self
            .store
            .require_dataset_access(actor, dataset, "owner")
            .await?;
        let mut recovered = 0;
        let marker = object(&object(&d["index_config"])["retrieval"])
            .get("_index_deletion_fence")
            .cloned();
        if let Some(marker) = marker {
            match super::document_reads::generation(&d) {
                Err(IngestError::Validation(message))
                    if message
                        == "dataset index deletion is pending; indexed content is unavailable" => {}
                Err(error) => return Err(error),
                _ => {}
            }
            if dry || marker["operation"] != "segment_delete" {
                super::document_reads::generation(&d)?;
            }
            if self
                .delete_segment(actor, dataset, string(&marker, "target_id"))
                .await?
            {
                recovered = 1;
            }
            d = self
                .store
                .require_dataset_access(actor, dataset, "owner")
                .await?;
        }
        let expected = super::document_reads::generation(&d)?;
        let rows = self
            .store
            .list_segments_scoped(dataset, None, None, 10001, 0, false, "")
            .await?;
        if rows.len() > 10000 {
            return Ok(
                json!({"code":"KB_DEDUPE_INCOMPLETE","incomplete":true,"scan_limit":10000,"observed_at_least":rows.len(),"message":"Dataset exceeds the synchronous dedupe limit."}),
            );
        }
        let total = rows.len();
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut order = vec![];
        for row in rows {
            let hash = string(&row, "content_hash").to_owned();
            if hash.is_empty() {
                continue;
            }
            if !groups.contains_key(&hash) {
                order.push(hash.clone());
            }
            groups.entry(hash).or_default().push(row);
        }
        let unique = groups.len();
        let mut duplicates = vec![];
        for hash in order {
            let mut rows = groups.remove(&hash).unwrap();
            if rows.len() > 1 {
                rows.sort_by(|a, b| string(a, "created_at").cmp(string(b, "created_at")));
                duplicates.extend(rows.into_iter().skip(1));
            }
        }
        let mut result = json!({"dataset_id":dataset,"total_segments":total,"incomplete":false,"unique_content":unique,"duplicates_found":duplicates.len(),"dry_run":dry});
        if dry {
            result["duplicates_preview"]=json!(duplicates.iter().take(10).map(|r|json!({"segment_id":r["segment_id"],"content_preview":string(r,"content").chars().take(100).collect::<String>()})).collect::<Vec<_>>());
            let latest = self
                .store
                .require_dataset_access(actor, dataset, "owner")
                .await?;
            if super::document_reads::generation(&latest)? != expected {
                return Err(IngestError::Validation(
                    super::document_reads::CONTENT_CHANGED.into(),
                ));
            }
            return Ok(result);
        }
        require_index_writable(&d)?;
        let mut deleted = recovered;
        let mut errors = vec![];
        for row in duplicates {
            let id = string(&row, "segment_id").trim();
            let outcome = if id.is_empty() {
                Err(IngestError::Validation(
                    "duplicate segment has no durable segment identity".into(),
                ))
            } else {
                self.delete_segment(actor, dataset, id).await
            };
            match outcome {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                Err(error) => errors.push(json!({"segment_id":id,"error":error.to_string()})),
            }
        }
        result["deleted_count"] = json!(deleted);
        if !errors.is_empty() {
            errors.truncate(10);
            result["errors"] = json!(errors)
        }
        let latest = self
            .store
            .require_dataset_access(actor, dataset, "owner")
            .await?;
        super::document_reads::generation(&latest)?;
        Ok(result)
    }
}
fn merge_patch(base: &mut Value, patch: &Value) {
    if let Some(patch) = patch.as_object() {
        if !base.is_object() {
            *base = json!({})
        }
        for (k, v) in patch {
            if v.is_null() {
                continue;
            }
            if v.is_object() {
                merge_patch(&mut base[k], v)
            } else {
                base[k] = v.clone()
            }
        }
    }
}

fn fingerprint_projection(value: &Value, keys: &[&str]) -> Value {
    let mut result = json!({});
    for key in keys {
        if let Some(v) = value.get(*key).filter(|v| !v.is_array() && !v.is_object()) {
            result[*key] = v.clone();
        }
    }
    result
}
pub fn revision_fingerprint(dataset: &Value) -> Option<String> {
    let revision = dataset["content_revision"].as_i64().filter(|n| *n >= 0)?;
    let index = object(&dataset["index_config"]);
    let retrieval = object(&index["retrieval"]);
    let keys: &[&str] = &[
        "adaptive_weights",
        "alpha",
        "bm25_weight",
        "candidate_top_k",
        "dense_weight",
        "enforce_config",
        "fusion",
        "fusion_method",
        "hyde",
        "keyword_candidate_k",
        "keyword_top_k",
        "lexical",
        "lock",
        "locked",
        "mmr",
        "mmr_lambda",
        "mmr_threshold",
        "mode",
        "multi_query_expansion",
        "native_hybrid",
        "parent_child",
        "query_rewrite",
        "rerank",
        "rerank_model",
        "rerank_top_n",
        "rrf_k",
        "rrf_weights",
        "score_threshold",
        "structural_routing",
        "summary_index",
        "top_k",
        "vector_top_k",
    ];
    let mut safe = fingerprint_projection(&retrieval, keys);
    if let Ok(Some(profile)) =
        crate::migration_knowledge_query::retrieval_v2::Profile::parse(&retrieval["v2"])
    {
        safe["v2"] = json!({"fingerprint":profile.fingerprint()});
    }
    if retrieval["fusion"].is_object() {
        safe["fusion"] = fingerprint_projection(
            &retrieval["fusion"],
            &[
                "alpha",
                "bm25_weight",
                "dense_weight",
                "method",
                "rrf_k",
                "rrf_weights",
                "strategy",
            ],
        );
    }
    if retrieval["rerank"].is_object() {
        safe["rerank"] = fingerprint_projection(
            &retrieval["rerank"],
            &["enabled", "model", "provider", "top_n"],
        );
    }
    if retrieval["mmr"].is_object() {
        safe["mmr"] =
            fingerprint_projection(&retrieval["mmr"], &["enabled", "lambda", "threshold"]);
    }
    if retrieval["rrf_weights"].is_object() {
        safe["rrf_weights"] = fingerprint_projection(
            &retrieval["rrf_weights"],
            &["bm25", "dense", "keyword", "vector"],
        );
    }
    if retrieval["parent_child"].is_object() {
        safe["parent_child"] = fingerprint_projection(
            &retrieval["parent_child"],
            &["enabled", "fanout_top_k", "return_mode"],
        );
    }
    if retrieval["summary_index"].is_object() {
        safe["summary_index"] =
            fingerprint_projection(&retrieval["summary_index"], &["enabled", "prepend_summary"]);
    }
    if retrieval["structural_routing"].is_object() {
        safe["structural_routing"] = fingerprint_projection(
            &retrieval["structural_routing"],
            &["boost", "enabled", "min_affinity", "mode"],
        );
    }
    if retrieval["query_rewrite"].is_object() {
        safe["query_rewrite"] =
            fingerprint_projection(&retrieval["query_rewrite"], &["enabled", "preset"]);
    }
    if retrieval["multi_query_expansion"].is_object() {
        safe["multi_query_expansion"] =
            fingerprint_projection(&retrieval["multi_query_expansion"], &["enabled", "preset"]);
    }
    if retrieval["hyde"].is_object() {
        safe["hyde"] = fingerprint_projection(&retrieval["hyde"], &["enabled"]);
    }
    let weights = fingerprint_projection(
        &object(&retrieval["fusion"])["rrf_weights"],
        &["vector", "keyword", "dense", "bm25"],
    );
    if weights.as_object().is_some_and(|m| !m.is_empty()) {
        if !safe["fusion"].is_object() {
            safe["fusion"] = json!({});
        }
        safe["fusion"]["rrf_weights"] = weights;
    }
    let mut embedding = fingerprint_projection(
        &object(&dataset["embedding_config"]),
        &["base_url", "dimension", "max_concurrent"],
    );
    let endpoint = embedding["base_url"]
        .as_str()
        .and_then(super::display_url::secret_free_base_url);
    if let Some(endpoint) = endpoint {
        embedding["base_url"] = json!(endpoint);
    } else {
        embedding.as_object_mut()?.remove("base_url");
    }
    let typed: std::result::Result<Dataset, _> = serde_json::from_value(dataset.clone());
    let lexical = typed.ok().and_then(|d| {
        crate::migration_knowledge_query::lexical_v2::LexicalProfile::from_dataset(&d).ok()
    });
    if let Some(profile) = lexical {
        if retrieval.get("lexical").is_some_and(|v| !v.is_null()) {
            let mut bm25 = profile.options.clone();
            bm25["shadow_write_enabled"] = json!(profile.shadow);
            bm25["schema_fingerprint"] = json!(profile.schema_fingerprint());
            let mut filtering = profile.filtering.clone();
            filtering["profile_fingerprint"] = json!(profile.filtering_fingerprint());
            bm25["filtering"] = filtering;
            safe["lexical"] = json!({"active_version":if profile.active{"bm25_v2"}else{"lexical_v1"},"bm25_v2":bm25});
        }
    } else {
        safe["lexical"] = json!({"invalid":true});
    }
    let payload = json!({"dataset_id":dataset["dataset_id"],"content_revision":revision,"embedding_provider":dataset["embedding_provider"],"embedding_model":dataset["embedding_model"],"embedding_dimension":dataset["embedding_dimension"],"needs_reindex":dataset["needs_reindex"],"collection_name":dataset["collection_name"],"retrieval_effective_config":{"retrieval":safe,"multimodal_enabled":index["multimodal_enabled"].as_bool().unwrap_or(false)||index["enable_multimodal"].as_bool().unwrap_or(false),"embedding":embedding}});
    Some(format!(
        "sha256:{}",
        super::sha256(super::canonical_json(&payload).as_bytes())
    ))
}
