//! Direct three-level cascade/parallel/adaptive retriever. L2 is diagnostic in
//! cascade, as in the source; the L1 document filter selects the L3 search.
use super::{
    config::{self, RetrieveRequest},
    qdrant::VectorHit,
    service::RetrievalService,
    types::{Dataset, QueryError, Result, RetrieveResult, UserContext},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
fn key(h: &VectorHit) -> String {
    h.payload["segment_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&h.point_id)
        .into()
}
pub fn contains_multimodal(v: &Value, depth: usize) -> bool {
    if depth > 12 {
        return true;
    }
    match v {
        Value::Object(m) => m.iter().any(|(k, v)| {
            let k = k
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>();
            (["contenttype", "mediatype", "modality"].contains(&k.as_str())
                && v.as_str().is_some_and(|s| {
                    let s = s.trim().to_lowercase();
                    ["image", "page_image", "mixed", "multimodal", "vision"].contains(&s.as_str())
                        || s.starts_with("image/")
                }))
                || contains_multimodal(v, depth + 1)
        }),
        Value::Array(a) => a.iter().any(|v| contains_multimodal(v, depth + 1)),
        _ => false,
    }
}
impl RetrievalService {
    async fn hierarchy_search(
        &self,
        d: &Dataset,
        vector: &[f64],
        k: usize,
        docs: &[String],
        threshold: Option<f64>,
    ) -> Result<Vec<VectorHit>> {
        match self
            .vectors
            .search_hierarchy(d, vector, k, docs, threshold)
            .await
        {
            Ok(v) => Ok(v),
            Err(e @ QueryError::CollectionAuthority(_)) => Err(e),
            Err(_) => Ok(vec![]),
        }
    }
    pub(super) async fn hierarchy_filter(
        &self,
        d: &Dataset,
        hits: Vec<VectorHit>,
        level: usize,
    ) -> Result<Vec<VectorHit>> {
        let mut hits: Vec<_> = hits
            .into_iter()
            .filter(|h| !contains_multimodal(&h.payload, 0))
            .collect();
        if hits.is_empty() {
            return Ok(hits);
        }
        let documents: Vec<_> = hits
            .iter()
            .filter_map(|h| h.payload["document_id"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let active:Vec<String>=sqlx::query_scalar("SELECT d.document_id FROM documents d JOIN datasets ds ON ds.dataset_id=d.dataset_id WHERE d.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND d.document_id=ANY($3::text[]) AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex')").bind(&d.dataset_id).bind(&d.tenant_id).bind(documents).fetch_all(&self.store.pool).await?;
        hits.retain(|h| {
            h.payload["document_id"]
                .as_str()
                .is_some_and(|id| active.iter().any(|d| d == id))
        });
        let ids: Vec<_> = hits.iter().map(key).collect();
        let live: BTreeSet<String> = if level == 1 {
            let ids:Vec<String>=sqlx::query_scalar("SELECT DISTINCT s.vector_id FROM document_summaries s JOIN documents d ON d.document_id=s.document_id JOIN datasets ds ON ds.dataset_id=d.dataset_id WHERE d.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND s.vector_id=ANY($3::text[]) AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex')").bind(&d.dataset_id).bind(&d.tenant_id).bind(&ids).fetch_all(&self.store.pool).await?;
            ids.into_iter().collect()
        } else {
            self.store.active_segment_ids(d, &ids).await?
        };
        hits.retain(|h| live.contains(&key(h)));
        Ok(hits)
    }
    pub async fn retrieve_hierarchical(
        &self,
        user: &UserContext,
        id: &str,
        r: &RetrieveRequest,
    ) -> Result<(Vec<RetrieveResult>, Value)> {
        self.vectors
            .with_interactive_budget(self.retrieve_hierarchical_within_budget(user, id, r))
            .await
    }
    async fn retrieve_hierarchical_within_budget(
        &self,
        user: &UserContext,
        id: &str,
        r: &RetrieveRequest,
    ) -> Result<(Vec<RetrieveResult>, Value)> {
        let started = Instant::now();
        let d = self
            .store
            .require_dataset_access(user, id, "viewer")
            .await?;
        let generation = d.generation()?;
        config::resolve(&d, r)?;
        self.preflight(&d).await?;
        let mut cache_request=r.clone();cache_request.hierarchical=true;
        let fingerprint=self.result_cache_key(user,&d,&cache_request,None)?;
        if let Some(cached)=self.lookup_cached_result(user,&d,&generation,&cache_request,&fingerprint,started).await? {return Ok(cached);}
        let base = d.collection()?.to_owned();
        let mut summary = d.clone();
        summary.collection_name = Some(format!("{base}_summary"));
        let mut sections = d.clone();
        sections.collection_name = Some(format!("{base}_sections"));
        for layer in [&summary, &sections] {
            if self.vectors.collection_exists(layer.collection()?).await? {
                self.vectors.require_collection_readable(layer).await?;
            }
        }
        let strategy = if r.hierarchical_strategy == "adaptive" {
            if r.query.split_whitespace().count() > 10 {
                "parallel"
            } else {
                "cascade"
            }
        } else {
            &r.hierarchical_strategy
        };
        let mut meta = json!({"strategy":strategy,"l1_candidates":0,"l2_candidates":0,"l3_results":0,"l1_time_ms":0.0,"l2_time_ms":0.0,"l3_time_ms":0.0,"total_time_ms":0.0,"filtered_documents":[]});
        // Source intentionally embeds hierarchical query as a document and
        // degrades an embedding failure to an empty hierarchy result.
        let vector = match self
            .embeddings
            .embed_documents(&d, &[r.query.clone()])
            .await
        {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            _ => {
                super::service::stamp_observation(&mut meta, &r.query);
                return Ok((vec![], meta));
            }
        };
        let ranked = if strategy == "cascade" {
            let timer = Instant::now();
            let l1 = self
                .hierarchy_filter(
                    &d,
                    self.hierarchy_search(&summary, &vector, r.l1_top_k, &[], None)
                        .await?,
                    1,
                )
                .await?;
            let docs: Vec<String> = l1
                .iter()
                .filter_map(|h| h.payload["document_id"].as_str().map(str::to_owned))
                .collect();
            meta["l1_candidates"] = json!(l1.len());
            meta["filtered_documents"] = json!(docs);
            meta["l1_time_ms"] = json!(timer.elapsed().as_secs_f64() * 1000.0);
            let timer = Instant::now();
            let l2 = self
                .hierarchy_filter(
                    &d,
                    self.hierarchy_search(&sections, &vector, r.l2_top_k, &docs, None)
                        .await?,
                    2,
                )
                .await?;
            meta["l2_candidates"] = json!(l2.len());
            meta["l2_time_ms"] = json!(timer.elapsed().as_secs_f64() * 1000.0);
            let timer = Instant::now();
            let l3 = self
                .hierarchy_filter(
                    &d,
                    self.hierarchy_search(&d, &vector, r.top_k * 2, &docs, r.score_threshold)
                        .await?,
                    3,
                )
                .await?;
            meta["l3_results"] = json!(l3.len());
            meta["l3_time_ms"] = json!(timer.elapsed().as_secs_f64() * 1000.0);
            l3.into_iter()
                .take(r.top_k)
                .map(|h| (h, 3))
                .collect::<Vec<_>>()
        } else {
            let (a, b, c) = tokio::join!(
                self.hierarchy_search(&summary, &vector, r.top_k * 2, &[], None),
                self.hierarchy_search(&sections, &vector, r.top_k * 2, &[], None),
                self.hierarchy_search(&d, &vector, r.top_k * 3, &[], r.score_threshold)
            );
            let (a, b, c) = tokio::try_join!(
                self.hierarchy_filter(&d, a?, 1),
                self.hierarchy_filter(&d, b?, 2),
                self.hierarchy_filter(&d, c?, 3)
            )?;
            meta["l1_candidates"] = json!(a.len());
            meta["l2_candidates"] = json!(b.len());
            meta["l3_results"] = json!(c.len());
            let mut fused = Vec::<(String, VectorHit, usize, f64)>::new();
            for (level, weight, hits) in [(1, 0.3, a), (2, 0.5, b), (3, 1.0, c)] {
                for (rank, hit) in hits.into_iter().enumerate() {
                    let id = key(&hit);
                    let score = weight / (60 + rank + 1) as f64;
                    if let Some(item) = fused.iter_mut().find(|v| v.0 == id) {
                        item.3 += score
                    } else {
                        fused.push((id, hit, level, score))
                    }
                }
            }
            fused.sort_by(|a, b| b.3.total_cmp(&a.3));
            fused
                .into_iter()
                .take(r.top_k)
                .map(|(_, mut h, level, s)| {
                    h.score = s;
                    h.payload["rrf_score"] = json!(s);
                    (h, level)
                })
                .collect()
        };
        let mut results = Vec::new();
        for (h, level) in ranked {
            let mut metadata = h.payload;
            metadata["level"] = json!(level);
            let document = metadata["document_id"].as_str().unwrap_or("").to_owned();
            let segment = metadata["segment_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&h.point_id)
                .to_owned();
            if r.include_context {
                let summary:std::result::Result<Option<String>,sqlx::Error>=sqlx::query_scalar("SELECT s.summary FROM document_summaries s JOIN documents d ON d.document_id=s.document_id JOIN datasets ds ON ds.dataset_id=d.dataset_id WHERE d.document_id=$1 AND d.dataset_id=$2 AND ds.tenant_id=$3 AND ds.is_deleted=FALSE AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex')").bind(&document).bind(id).bind(&d.tenant_id).fetch_optional(&self.store.pool).await;
                if let Ok(Some(s)) = summary {
                    metadata["document_summary"] = json!(s)
                }
                if let Some(parent) = metadata["parent_segment_id"].as_str() {
                    if let Ok(Some(p)) = self.store.segment_scoped(&d, parent).await {
                        metadata["parent_context"] =
                            json!(p.summary.filter(|s| !s.is_empty()).unwrap_or(p.text));
                    }
                }
            }
            results.push(RetrieveResult {
                segment_id: segment,
                document_id: document,
                score: h.score,
                text: metadata["text"].as_str().unwrap_or("").into(),
                metadata: strip_image_metadata(metadata),
                content_type: "text".into(),
                image_url: None,
                vlm_description: None,
                associated_images: vec![],
            })
        }
        // Recheck SQL lifecycle and generation after optional context lookups.
        let ids: Vec<_> = results
            .iter()
            .filter(|r| r.metadata["level"] != 1)
            .map(|r| r.segment_id.clone())
            .collect();
        let live = self.store.active_segment_ids(&d, &ids).await?;
        results.retain(|r| r.metadata["level"] == 1 || live.contains(&r.segment_id));
        self.store.require_generation(user, id, &generation).await?;
        meta["total_time_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        meta["timings_ms"] = json!({"total_ms":meta["total_time_ms"]});
        meta["retrieval_cache_hit"]=json!(false);
        meta["retrieval_query_fingerprint"]=json!(fingerprint);
        // Hierarchy search intentionally degrades some failures to empty results;
        // do not memoize an empty degraded path as authoritative negative recall.
        if !results.is_empty() {self.store_cached_result(&d,&cache_request,fingerprint,results.clone(),meta.clone()).await?;}
        super::service::stamp_observation(&mut meta, &r.query);
        Ok((results, meta))
    }
}

fn strip_image_metadata(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(k, _)| {
                    let normalized = k
                        .to_lowercase()
                        .chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect::<String>();
                    ![
                        "associatedimages",
                        "imagebytes",
                        "imagecount",
                        "imagepresignedurl",
                        "images",
                        "imagesegmentid",
                        "imageurl",
                        "rawimageurl",
                        "storageurl",
                        "vlmdescription",
                    ]
                    .contains(&normalized.as_str())
                })
                .map(|(k, v)| (k, strip_image_metadata(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(strip_image_metadata).collect()),
        _ => v,
    }
}
