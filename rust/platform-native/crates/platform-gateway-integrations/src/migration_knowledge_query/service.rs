use super::{
    config::{self, ResolvedConfig, RetrieveRequest},
    embedding::EmbeddingManager,
    lexical,
    lexical_v2::LexicalProfile,
    parents,
    qdrant::{QdrantStore, VectorHit},
    rerank::{self, Reranker},
    store::QueryStore,
    types::{Candidate, Dataset, QueryError, Result, RetrieveResult, UserContext},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

type Cached = (String, Instant, Vec<RetrieveResult>, Value);
#[path = "result_cache.rs"]
mod result_cache;
#[derive(Clone)]
pub struct RetrievalService {
    pub store: QueryStore,
    pub vectors: QdrantStore,
    pub embeddings: EmbeddingManager,
    pub reranker: Option<Reranker>,
    pub cache_ttl: Duration,
    pub interactive_deadline: Duration,
    pub max_query_concurrency: usize,
    cache: Arc<Mutex<VecDeque<Cached>>>,
}
impl RetrievalService {
    pub fn new(
        store: QueryStore,
        vectors: QdrantStore,
        embeddings: EmbeddingManager,
        reranker: Option<Reranker>,
    ) -> Self {
        Self {
            store,
            vectors,
            embeddings,
            reranker,
            cache_ttl: Duration::ZERO,
            interactive_deadline: Duration::from_secs(3),
            max_query_concurrency: 3,
            cache: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
    pub async fn retrieve(
        &self,
        user: &UserContext,
        dataset_id: &str,
        request: RetrieveRequest,
    ) -> Result<(Vec<RetrieveResult>, Value)> {
        let started = Instant::now();
        let mode = request.mode.clone();
        let hierarchical = request.hierarchical;
        let result = self
            .vectors
            // Keep the full standard/hierarchical pipeline out of every caller's
            // inline future. Debug builds otherwise reserve >300 KiB even when
            // an outer tool rejects its input before polling retrieval.
            // Boxing does not spawn: the same task-local budget, deadline and
            // drop/cancellation behavior continue to govern this future.
            .with_interactive_budget(Box::pin(
                self.retrieve_within_budget(user, dataset_id, request),
            ))
            .await;
        if !hierarchical {
            if let Ok((_, meta)) = &result {
                super::pipeline_metrics::retrieval(
                    &mode,
                    meta["retrieval_cache_hit"] == true,
                    started.elapsed().as_secs_f64(),
                );
            }
        }
        result
    }
    async fn retrieve_within_budget(
        &self,
        user: &UserContext,
        dataset_id: &str,
        request: RetrieveRequest,
    ) -> Result<(Vec<RetrieveResult>, Value)> {
        if request.hierarchical {
            let (result, meta) = self
                .retrieve_hierarchical(user, dataset_id, &request)
                .await?;
            let d = self
                .store
                .require_dataset_access(user, dataset_id, "viewer")
                .await?;
            let mut r = request.clone();
            r.telemetry_source = "hierarchical".into();
            self.telemetry(d, user.clone(), r, result.clone(), meta.clone());
            return Ok((result, meta));
        }
        let deadline = self.vectors.publication_deadline(self.interactive_deadline);
        let mut last = QueryError::PublicationInProgress;
        for attempt in 0..=24 {
            match self
                .retrieve_once(user, dataset_id, &request, deadline)
                .await
            {
                Err(e @ (QueryError::PublicationInProgress | QueryError::GenerationChanged)) => {
                    last = e;
                    let delay = Duration::from_millis((25 * (attempt + 1)).min(200));
                    if attempt == 24 || Instant::now() + delay >= deadline {
                        break;
                    }
                    tokio::time::sleep(delay).await
                }
                result => return result,
            }
        }
        Err(last)
    }
    async fn retrieve_once(
        &self,
        user: &UserContext,
        id: &str,
        r: &RetrieveRequest,
        deadline: Instant,
    ) -> Result<(Vec<RetrieveResult>, Value)> {
        let started = Instant::now();
        let dataset = self
            .store
            .require_dataset_access(user, id, "viewer")
            .await?;
        let generation = dataset.generation()?;
        let cfg = config::resolve(&dataset, r)?;
        self.preflight(&dataset).await?;
        let normalized_query = r.query.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut request_value =
            serde_json::to_value(r).map_err(|_| QueryError::InvalidProviderResponse)?;
        request_value["query"] = json!(normalized_query);
        let fingerprint=format!("{:x}",Sha256::digest(json!({"contract":"standard-v1","user_id":user.user_id,"dataset":dataset,"request":request_value}).to_string().as_bytes()));
        if !self.cache_ttl.is_zero() && !user.user_id.is_empty() {
            let cached = {
                let mut cache = self.cache.lock().await;
                result_cache::get(&mut cache, &fingerprint, self.cache_ttl, Instant::now())
            };
            if let Some((results, mut meta)) = cached {
                let ids: Vec<_> = results.iter().map(|r| r.segment_id.clone()).collect();
                let active = self.store.active_segment_ids(&dataset, &ids).await?;
                if active == ids.into_iter().collect() {
                    self.store.require_generation(user, id, &generation).await?;
                    meta["retrieval_cache_hit"] = json!(true);
                    if let Some(m) = meta.as_object_mut() {
                        m.remove("rerank_degraded");
                        m.remove("rerank_error");
                    }
                    stamp_observation(&mut meta, &r.query);
                    self.telemetry(
                        dataset.clone(),
                        user.clone(),
                        r.clone(),
                        results.clone(),
                        meta.clone(),
                    );
                    return Ok((results, meta));
                }
            }
        }
        let (results, mut meta) = self
            .pipeline(user, &dataset, r, std::slice::from_ref(r), cfg, deadline)
            .await?;
        meta["retrieval_cache_hit"] = json!(false);
        meta["retrieval_query_fingerprint"] = json!(fingerprint);
        meta["timings_ms"]["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        if !self.cache_ttl.is_zero() && meta["rerank_degraded"].is_null() {
            let mut cache = self.cache.lock().await;
            result_cache::set(
                &mut cache,
                fingerprint,
                results.clone(),
                meta.clone(),
                self.cache_ttl,
                2000,
                Instant::now(),
            );
        }
        stamp_observation(&mut meta, &r.query);
        self.telemetry(
            dataset,
            user.clone(),
            r.clone(),
            results.clone(),
            meta.clone(),
        );
        Ok((results, meta))
    }
    pub async fn preflight(&self, dataset: &Dataset) -> Result<()> {
        dataset.generation()?;
        let provider = super::embedding::canonical_provider(
            dataset.embedding_provider.as_deref().unwrap_or(""),
        );
        let model = dataset.embedding_model.as_deref().unwrap_or("");
        if provider.ends_with("multimodal")
            || [
                "multimodal-embedding-v1",
                "multimodal-embedding-one-peace-v1",
                "multimodal-embedding-one-peace",
                "tongyi-embedding-vision-plus",
                "qwen2.5-vl-embedding",
            ]
            .contains(&model)
            || dataset.index_config["multimodal_enabled"].as_bool() == Some(true)
            || dataset.index_config["enable_multimodal"].as_bool() == Some(true)
        {
            return Err(QueryError::Validation(
                "multimodal dataset retrieval is unavailable in this release".into(),
            ));
        }
        LexicalProfile::from_dataset(dataset)?;
        config::validate_bounds(&dataset.index_config["retrieval"])?;
        config::server_owned_config(&dataset.index_config["retrieval"]["rerank"])?;
        self.vectors.require_collection_readable(dataset).await?;
        Ok(())
    }
    async fn pipeline(
        &self,
        user: &UserContext,
        dataset: &Dataset,
        primary: &RetrieveRequest,
        specs: &[RetrieveRequest],
        mut cfg: ResolvedConfig,
        deadline: Instant,
    ) -> Result<(Vec<RetrieveResult>, Value)> {
        let started = Instant::now();
        let generation = dataset.generation()?;
        let mut dataset = dataset.clone();
        let lexical_profile = LexicalProfile::from_dataset(&dataset)?;
        let structural = parents::parse_structural_settings(&dataset.index_config["retrieval"])?;
        if let Some(binding) = self.store.serving_binding(&dataset.dataset_id).await {
            let expected = json!({"embedding_provider":dataset.embedding_provider.as_deref().unwrap_or("local"),"embedding_model":dataset.embedding_model.as_deref().unwrap_or("hash-384"),"embedding_model_version":dataset.embedding_model_version.as_deref().unwrap_or(""),"embedding_dimension":dataset.embedding_dimension.unwrap_or(0)});
            let bound_provider = binding["embedding_provider"].as_str().unwrap_or("").trim();
            let bound_model = binding["embedding_model"].as_str().unwrap_or("").trim();
            if !bound_provider.is_empty() && !bound_model.is_empty() {
                let mismatch = !bound_provider
                    .eq_ignore_ascii_case(expected["embedding_provider"].as_str().unwrap_or(""))
                    || !bound_model
                        .eq_ignore_ascii_case(expected["embedding_model"].as_str().unwrap_or(""))
                    || (!expected["embedding_model_version"]
                        .as_str()
                        .unwrap_or("")
                        .is_empty()
                        && binding["embedding_model_version"]
                            != expected["embedding_model_version"])
                    || (binding["embedding_dimension"].as_u64().unwrap_or(0) > 0
                        && expected["embedding_dimension"].as_u64().unwrap_or(0) > 0
                        && binding["embedding_dimension"] != expected["embedding_dimension"]);
                if mismatch {
                    self.store
                        .require_generation(user, &dataset.dataset_id, &generation)
                        .await?;
                    return Err(QueryError::Validation(
                        "query embedding identity does not match serving binding".into(),
                    ));
                }
            }
            if let Some(collection) = binding["collection_name"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                dataset.collection_name = Some(collection.into())
            }
        }
        let configs: Vec<_> = specs
            .iter()
            .map(|r| config::resolve(&dataset, r))
            .collect::<Result<_>>()?;
        let dense_requested = configs.iter().any(|c| c.mode != "bm25");
        let lexical_requested = configs.iter().any(|c| c.mode != "dense");
        if dataset.needs_reindex && dense_requested {
            return Err(QueryError::Validation(
                "Dataset embeddings were migrated and require re-indexing before vector retrieval"
                    .into(),
            ));
        }
        if dense_requested && lexical_requested {
            cfg.mode = "hybrid".into()
        } else if dense_requested {
            cfg.mode = "dense".into()
        } else {
            cfg.mode = "bm25".into()
        }
        let mut query_vectors = BTreeMap::<String, Vec<f64>>::new();
        let mut errors = Vec::<String>::new();
        let prepare = Instant::now();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            self.max_query_concurrency.max(1),
        ));
        let mut embeddings = tokio::task::JoinSet::new();
        for (r, c) in specs.iter().zip(&configs) {
            if c.mode != "bm25" {
                let manager = self.embeddings.clone();
                let dataset = dataset.clone();
                let query = r.query.clone();
                let limit = semaphore.clone();
                embeddings.spawn(async move {
                    let _permit = limit
                        .acquire_owned()
                        .await
                        .map_err(|_| QueryError::DependenciesFailed)?;
                    let result = manager.embed_query(&dataset, &query).await;
                    Ok::<_, QueryError>((query, result))
                });
            }
        }
        while let Some(joined) = embeddings.join_next().await {
            let (query, result) = joined.map_err(|_| QueryError::DependenciesFailed)??;
            match result {
                Ok(vector) => {
                    query_vectors.insert(query, vector);
                }
                Err(e) => {
                    if cfg.mode == "dense" && specs.len() == 1 {
                        return Err(QueryError::Validation(format!(
                            "Dense retrieval preparation failed: {e}"
                        )));
                    }
                    errors.push(format!("dense_prepare: {e}"));
                }
            }
        }
        let dense_prepare_ms = prepare.elapsed().as_secs_f64() * 1000.0;
        let filters_equal = specs.windows(2).all(|r| {
            r[0].source_type_filter == r[1].source_type_filter
                && r[0].language_filter == r[1].language_filter
                && r[0].metadata_filter == r[1].metadata_filter
        });
        let native_enabled = dataset.index_config["retrieval"]["native_hybrid"]
            .as_bool()
            .unwrap_or(true)
            && cfg.mode == "hybrid"
            && cfg.fusion_method == "rrf"
            && filters_equal
            && configs.iter().all(|c| c.mode == "hybrid")
            && specs.iter().all(|r| {
                query_vectors.contains_key(&r.query)
                    && (lexical_profile.active
                        || !lexical::sparse_vector(&r.query, true).0.is_empty())
            });
        let mut candidates = BTreeMap::<String, Candidate>::new();
        let mut candidate_order = Vec::<String>::new();
        let mut native_used = false;
        let mut native_error = None;
        let mut native_scores = BTreeMap::<String, f64>::new();
        let mut lists = Vec::<Vec<(String, Vec<String>)>>::new();
        let mut dense_count = 0usize;
        let mut bm25_count = 0usize;
        if native_enabled {
            let batch_specs: Vec<_> = specs
                .iter()
                .zip(&configs)
                .map(|(r, c)| {
                    (
                        r.query.clone(),
                        query_vectors[&r.query].clone(),
                        r.clone(),
                        c.vector_k,
                        c.keyword_k,
                    )
                })
                .collect();
            let weights = cfg
                .weighted_rrf
                .then_some((cfg.dense_weight, cfg.bm25_weight));
            match self
                .vectors
                .hybrid_search_batch(&dataset, &batch_specs, cfg.candidate_k, cfg.rrf_k, weights)
                .await
            {
                Ok(batches) => {
                    for hits in batches {
                        for hit in hits {
                            if let Some(c) = candidate_from_hit(hit, "qdrant_rrf") {
                                let score = c.final_score;
                                native_scores
                                    .entry(c.segment_id.clone())
                                    .and_modify(|v| *v = v.max(score))
                                    .or_insert(score);
                                if !candidates.contains_key(&c.segment_id) {
                                    candidate_order.push(c.segment_id.clone());
                                }
                                candidates.entry(c.segment_id.clone()).or_insert(c);
                            }
                        }
                    }
                    native_used = true
                }
                Err(e @ QueryError::CollectionAuthority(_)) => return Err(e),
                Err(e) => {
                    if lexical_profile.active {
                        return Err(e);
                    }
                    native_error = Some(e.to_string());
                    candidates.clear();
                    candidate_order.clear();
                    native_scores.clear();
                }
            }
        }

        if !native_used {
            for (r, c) in specs.iter().zip(&configs) {
                let dense = async {
                    if c.mode == "bm25" {
                        return Ok(vec![]);
                    }
                    let Some(vector) = query_vectors.get(&r.query) else {
                        return Ok(vec![]);
                    };
                    self.vectors.search(&dataset, vector, c.vector_k, r).await
                };
                let bm25 = async {
                    if c.mode == "dense" {
                        Ok(vec![])
                    } else {
                        self.bm25_recall(&dataset, r, c, &lexical_profile).await
                    }
                };
                let (dense, keyword) = tokio::join!(dense, bm25);
                let mut ranked = Vec::new();
                match dense {
                    Ok(hits) => {
                        dense_count += hits.len();
                        let mut ids = Vec::new();
                        for hit in hits {
                            if let Some(mut candidate) = candidate_from_hit(hit, "dense") {
                                if filters_equal || matches_filters(&candidate.metadata, r) {
                                    candidate.dense_score = Some(candidate.final_score);
                                    ids.push(candidate.segment_id.clone());
                                    merge_candidate(
                                        &mut candidates,
                                        &mut candidate_order,
                                        candidate,
                                    );
                                }
                            }
                        }
                        ranked.push(("dense".into(), ids));
                    }
                    Err(e @ QueryError::CollectionAuthority(_)) => return Err(e),
                    Err(e) => {
                        if cfg.mode == "dense" {
                            return Err(e);
                        }
                        errors.push(format!("dense: {e}"));
                    }
                }
                match keyword {
                    Ok(hits) => {
                        bm25_count += hits.len();
                        let mut ids = Vec::new();
                        for candidate in hits {
                            if filters_equal || matches_filters(&candidate.metadata, r) {
                                ids.push(candidate.segment_id.clone());
                                merge_candidate(&mut candidates, &mut candidate_order, candidate);
                            }
                        }
                        ranked.push(("bm25".into(), ids));
                    }
                    Err(e) => {
                        if lexical_profile.active {
                            return Err(e);
                        }
                        errors.push(format!("bm25: {e}"));
                    }
                }
                lists.push(ranked);
            }
        }
        let ids: Vec<_> = candidates.keys().cloned().collect();
        let active = self.store.active_segment_ids(&dataset, &ids).await?;
        let inactive = ids.len() - active.len();
        candidates.retain(|id, c| active.contains(id) && !contains_image(&c.metadata));
        let dense_scores: BTreeMap<_, _> = candidates
            .iter()
            .filter_map(|(id, c)| c.dense_score.map(|v| (id.clone(), v)))
            .collect();
        let bm25_scores: BTreeMap<_, _> = candidates
            .iter()
            .filter_map(|(id, c)| c.bm25_score.map(|v| (id.clone(), v)))
            .collect();
        let dn = lexical::normalize(&dense_scores, "robust");
        let bn = lexical::normalize(&bm25_scores, "robust");
        let use_rrf = !native_used
            && cfg.fusion_method == "rrf"
            && (cfg.mode == "hybrid" || lists.iter().map(Vec::len).sum::<usize>() > 1);
        let mut rrf_scores = BTreeMap::<String, f64>::new();
        if use_rrf {
            let weights = if cfg.weighted_rrf {
                BTreeMap::from([
                    ("dense".into(), cfg.dense_weight),
                    ("bm25".into(), cfg.bm25_weight),
                ])
            } else {
                BTreeMap::new()
            };
            for query_lists in &lists {
                for (id, score) in lexical::reciprocal_rank_fusion(
                    query_lists,
                    cfg.rrf_k,
                    &weights,
                    cfg.weighted_rrf,
                )? {
                    rrf_scores
                        .entry(id)
                        .and_modify(|v| *v = v.max(score))
                        .or_insert(score);
                }
            }
        }
        let rrf_max = rrf_scores
            .values()
            .copied()
            .reduce(f64::max)
            .unwrap_or(1.0)
            .max(f64::MIN_POSITIVE);
        let native_max = native_scores
            .values()
            .copied()
            .reduce(f64::max)
            .unwrap_or(1.0)
            .max(f64::MIN_POSITIVE);
        let sum_weights = cfg.dense_weight + cfg.bm25_weight;
        for (id, c) in &mut candidates {
            c.metadata["_dense_score_norm"] = dn
                .get(id)
                .map_or(json!("N/A"), |v| json!(parents::round(*v, 4)));
            c.metadata["_bm25_score_norm"] = bn
                .get(id)
                .map_or(json!("N/A"), |v| json!(parents::round(*v, 4)));
            let score = if native_used {
                native_scores.get(id).copied().unwrap_or(0.0) / native_max
            } else if use_rrf {
                rrf_scores.get(id).copied().unwrap_or(0.0) / rrf_max
            } else if cfg.mode == "dense" {
                dn.get(id).copied().unwrap_or(0.0)
            } else if cfg.mode == "bm25" {
                bn.get(id).copied().unwrap_or(0.0)
            } else {
                dn.get(id).copied().unwrap_or(0.0) * cfg.dense_weight / sum_weights
                    + bn.get(id).copied().unwrap_or(0.0) * cfg.bm25_weight / sum_weights
            };
            c.fusion_score = Some(score);
            c.final_score = score;
            if native_used || use_rrf {
                c.metadata["_rrf_score"] = json!(parents::round(score, 4));
                c.metadata["_rrf_score_raw"] = json!(parents::round(
                    if native_used {
                        native_scores.get(id).copied().unwrap_or(0.0)
                    } else {
                        rrf_scores.get(id).copied().unwrap_or(0.0)
                    },
                    6
                ));
            }
            let (match_score, details) = lexical::text_match_score(&primary.query, &c.text);
            c.metadata["_text_match_score"] = json!(match_score);
            c.metadata["_exact_match"] = details["exact_match"].clone();
            c.metadata["_term_matches"] = details["term_matches"].clone();
            c.metadata["_term_ratio"] = details["term_ratio"].clone();
        }
        let mut meta = json!({"mode":cfg.mode,"fusion_method":cfg.fusion_method,"dense_weight":cfg.dense_weight,"bm25_weight":cfg.bm25_weight,"rrf_k":cfg.rrf_k,"native_hybrid":native_used,"fusion_applied_by":if native_used{"qdrant"}else{"python"},"dense_hits":dense_count,"bm25_hits":bm25_count,"rerank":cfg.rerank,"inactive_candidates":inactive,"pipeline_stages":[],"timings_ms":{"dense_prepare_ms":dense_prepare_ms},"recall_max_parallel":self.max_query_concurrency,"score_threshold":if cfg.mode=="dense"{cfg.threshold}else{0.0}});
        if let Some(e) = native_error {
            meta["native_hybrid_error"] = json!(e)
        }
        if !errors.is_empty() {
            meta["recall_errors"] = json!(errors)
        }
        if !cfg.presets.as_object().is_none_or(|m| m.is_empty()) {
            meta["query_transformations"] = cfg.presets.clone()
        }
        if let Some(prepend) = cfg.prepend_summary {
            meta["summary_index"] = parents::merge_summary_siblings(&mut candidates, prepend)
        }
        if let Some(structural) = structural {
            meta["structural_routing"] =
                parents::apply_structural_routing(&mut candidates, &primary.query, &structural)
        }
        if !errors.is_empty() && candidates.is_empty() {
            return Err(QueryError::DependenciesFailed);
        }
        let mut ranked: Vec<Candidate> = candidate_order
            .into_iter()
            .filter_map(|id| candidates.remove(&id))
            .collect();
        ranked.sort_by(|a, b| b.final_score.total_cmp(&a.final_score));
        if filters_equal {
            ranked.retain(|c| matches_filters(&c.metadata, primary));
        }
        ranked.truncate(cfg.candidate_k);
        if native_used {
            for (i, c) in ranked.iter_mut().enumerate() {
                c.metadata["global_rank"] = json!(i + 1)
            }
        }
        let mut rerank_served = false;
        let mut effective_threshold = if cfg.mode == "dense" {
            cfg.threshold
        } else {
            0.0
        };
        if cfg.rerank && !ranked.is_empty() {
            let rerank_started = Instant::now();
            let budget = deadline.saturating_duration_since(Instant::now());
            if budget < Duration::from_millis(50) {
                meta["rerank_degraded"] = json!("budget_exhausted");
                meta["rerank_error"] = json!("rerank interactive budget exhausted")
            } else if let Some(reranker) = &self.reranker {
                let docs: Vec<_> = ranked.iter().map(|c| c.text.clone()).collect();
                let top_n = cfg
                    .rerank_top_n
                    .unwrap_or(ranked.len().min((primary.top_k * 3).max(20)));
                let mut provider =
                    rerank::normalize_provider(&cfg.rerank_provider, &cfg.rerank_model);
                let mut model = rerank::normalize_model(&provider, &cfg.rerank_model);
                let mut response = tokio::time::timeout(
                    budget,
                    reranker.rerank(&provider, &model, &primary.query, &docs, top_n),
                )
                .await;
                if provider == "bge"
                    && matches!(&response,Ok(Err(QueryError::Validation(message))) if message.contains("FlagEmbedding"))
                {
                    provider = "dashscope".into();
                    model = "qwen3-rerank".into();
                    meta["rerank_fallback"] =
                        json!({"from_provider":"bge","to_provider":provider,"to_model":model});
                    response = tokio::time::timeout(
                        deadline.saturating_duration_since(Instant::now()),
                        reranker.rerank(&provider, &model, &primary.query, &docs, top_n),
                    )
                    .await;
                }
                match response {
                    Ok(Ok(results)) => {
                        let mut used = BTreeSet::new();
                        let mut reordered = Vec::new();
                        for result in results {
                            if let Some(c) = ranked.get(result.index) {
                                let mut c = c.clone();
                                c.rerank_score = Some(result.relevance_score);
                                c.final_score = result.relevance_score;
                                used.insert(result.index);
                                reordered.push(c)
                            }
                        }
                        reordered.extend(
                            ranked
                                .into_iter()
                                .enumerate()
                                .filter(|(i, _)| !used.contains(i))
                                .map(|(_, c)| c),
                        );
                        ranked = reordered;
                        rerank_served = true;
                        effective_threshold = cfg.threshold;
                        meta["rerank_applied_provider"] = json!(provider);
                        meta["rerank_applied_model"] = json!(model);
                        meta["rerank_top_n"] = json!(top_n);
                    }
                    Ok(Err(e)) => {
                        meta["rerank_error"] = json!(e.to_string());
                        meta["rerank_degraded"] = json!("error")
                    }
                    Err(_) => {
                        meta["rerank_error"] = json!("rerank timed out");
                        meta["rerank_degraded"] = json!("timeout")
                    }
                }
            } else {
                meta["rerank_error"] = json!("server-owned reranker is unavailable");
                meta["rerank_degraded"] = json!("error")
            }
            meta["timings_ms"]["rerank_ms"] =
                json!(rerank_started.elapsed().as_secs_f64() * 1000.0);
        }
        if let Some(reason) = meta["rerank_degraded"].as_str() {
            super::pipeline_metrics::rerank_degraded(reason);
        }
        if cfg.mmr && !ranked.is_empty() {
            if ranked.len() <= primary.top_k {
                meta["mmr_skipped"] = json!("candidate_count<=top_k")
            } else {
                let ids: Vec<_> = ranked.iter().map(|c| c.segment_id.clone()).collect();
                let point_ids: Vec<String> = ranked
                    .iter()
                    .map(|c| c.point_id.clone().unwrap_or_else(|| c.segment_id.clone()))
                    .collect();
                match self.vectors.retrieve_vectors(&dataset, &point_ids).await {
                    Ok(point_vectors) => {
                        let vectors: BTreeMap<String, Vec<f64>> = ranked
                            .iter()
                            .filter_map(|c| {
                                point_vectors
                                    .get(c.point_id.as_deref().unwrap_or(&c.segment_id))
                                    .map(|v| (c.segment_id.clone(), v.clone()))
                            })
                            .collect();
                        let relevance: BTreeMap<_, _> = ranked
                            .iter()
                            .map(|c| {
                                (
                                    c.segment_id.clone(),
                                    c.rerank_score.or(c.fusion_score).unwrap_or(c.final_score),
                                )
                            })
                            .collect();
                        let mut ordered = ids.clone();
                        ordered.sort_by(|a, b| relevance[b].total_cmp(&relevance[a]));
                        let (selected, picks) = lexical::mmr_select(
                            &ordered,
                            &relevance,
                            &vectors,
                            primary.top_k,
                            cfg.mmr_lambda,
                            cfg.mmr_threshold,
                            &cfg.mmr_fill_policy,
                        )?;
                        if cfg.mmr_fill_policy == "strict"
                            && selected.len() < primary.top_k
                            && ordered.len() > selected.len()
                        {
                            meta["mmr_diversity_shortfall"] = json!({"requested":primary.top_k,"selected":selected.len(),"candidates":ordered.len()})
                        }
                        let mut by_id: BTreeMap<_, _> = ranked
                            .into_iter()
                            .map(|c| (c.segment_id.clone(), c))
                            .collect();
                        ranked = selected
                            .into_iter()
                            .filter_map(|id| {
                                let mut c = by_id.remove(&id)?;
                                if let Some(p) = picks.get(&id) {
                                    c.metadata["_mmr_score"] = json!(p.mmr_score);
                                    c.metadata["_mmr_relevance"] = json!(p.relevance);
                                    c.metadata["_mmr_max_sim"] = json!(p.max_sim_to_selected);
                                    c.final_score = p.relevance
                                }
                                Some(c)
                            })
                            .collect();
                    }
                    Err(e @ QueryError::CollectionAuthority(_)) => return Err(e),
                    Err(e) => meta["mmr_error"] = json!(e.to_string()),
                }
            }
        }
        if let Some(settings) = &cfg.parent_child {
            let (folded, stats) =
                parents::fold_candidates_to_parents(ranked, settings, &self.store, &dataset)
                    .await?;
            ranked = folded;
            meta["parent_child"] = json!({"enabled":true,"return_mode":settings.return_mode,"fanout":{"top_k":settings.fanout_top_k.max(primary.top_k),"source":if settings.configured_fanout{"config"}else{"default"},"vector_k":cfg.vector_k,"keyword_k":cfg.keyword_k,"candidate_k":cfg.candidate_k,"keyword_pool_k":cfg.keyword_pool_k},"fold":stats});
        }
        if cfg.threshold > 0.0 && effective_threshold == 0.0 {
            meta["score_threshold_skipped"] = json!("uncalibrated_final_score")
        } else if effective_threshold > 0.0 {
            ranked.retain(|c| {
                (rerank_served && c.rerank_score.is_none()) || c.final_score >= effective_threshold
            })
        }
        ranked.truncate(primary.top_k);
        let raw: BTreeMap<_, _> = ranked
            .iter()
            .map(|c| (c.segment_id.clone(), c.final_score))
            .collect();
        let display = lexical::normalize(&raw, "robust");
        let mut results = Vec::new();
        for (i, mut c) in ranked.into_iter().enumerate() {
            let score = display.get(&c.segment_id).copied().unwrap_or(0.0);
            c.metadata["_sources"] = json!(c.sources);
            for (key, value) in [
                ("_dense_score", c.dense_score),
                ("_bm25_score", c.bm25_score),
                ("_fusion_score", c.fusion_score),
                ("_rerank_score", c.rerank_score),
            ] {
                c.metadata[key] = value.map_or(json!("N/A"), |v| json!(parents::round(v, 4)));
            }
            if let Some(v) = c.dense_score {
                c.metadata["_vector_score"] = json!(parents::round(v, 4))
            }
            if let Some(v) = c.bm25_score {
                c.metadata["_keyword_score"] = json!(parents::round(v, 4))
            }
            for key in ["_mmr_score", "_mmr_relevance", "_mmr_max_sim"] {
                if c.metadata[key].is_null() {
                    c.metadata[key] = json!("N/A")
                }
            }
            c.metadata["_rank"] = json!(i + 1);
            c.metadata["_final_score_raw"] = json!(parents::round(c.final_score, 6));
            c.metadata["_final_score"] = json!(parents::round(score, 6));
            let content_type = c.metadata["content_type"].as_str().unwrap_or("text").into();
            let image_url = c.metadata["image_url"].as_str().map(str::to_owned);
            let vlm_description = c.metadata["vlm_description"].as_str().map(str::to_owned);
            results.push(RetrieveResult {
                segment_id: c.segment_id,
                document_id: c.document_id,
                score,
                text: c.text,
                metadata: c.metadata,
                content_type,
                image_url,
                vlm_description,
                associated_images: vec![],
            });
        }
        self.store
            .require_generation(user, &dataset.dataset_id, &generation)
            .await?;
        meta["timings_ms"]["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        Ok((results, meta))
    }
    async fn bm25_recall(
        &self,
        d: &Dataset,
        r: &RetrieveRequest,
        c: &ResolvedConfig,
        profile: &LexicalProfile,
    ) -> Result<Vec<Candidate>> {
        if profile.active {
            return Ok(self
                .vectors
                .sparse_search(d, &r.query, c.keyword_k, r)
                .await?
                .into_iter()
                .filter_map(|h| {
                    let mut c = candidate_from_hit(h, "bm25")?;
                    c.bm25_score = Some(c.final_score);
                    Some(c)
                })
                .collect());
        }
        let terms = lexical::tokenize(&r.query, true, true, true);
        if terms.is_empty() {
            return Ok(vec![]);
        }
        let limit = c.keyword_pool_k.min(80);
        let mut rows = self.store.search_segments_text(d, &terms, r, limit).await?;
        if rows.is_empty()&&r.query.chars().any(|c|matches!(c as u32,0x3040..=0x30ff|0x3400..=0x4dbf|0x4e00..=0x9fff|0xf900..=0xfaff|0xac00..=0xd7af)){rows=self.store.search_ilike(d,&terms,r,limit).await.unwrap_or_default();}
        rows.retain(|c| !c.text.trim().is_empty());
        let tokens: Vec<_> = rows
            .iter()
            .map(|r| lexical::tokenize(r.text.trim(), false, false, true))
            .collect();
        let scores = lexical::bm25_scores(&terms, &tokens, 1.2, 0.75);
        let mut out = Vec::new();
        for (row, score) in rows.into_iter().zip(scores) {
            if row.segment_id.is_empty() || score <= 0.0 {
                continue;
            }
            let payload = json!({"dataset_id":row.dataset_id,"document_id":row.document_id,"segment_id":row.segment_id,"position":row.position,"text":row.text.trim(),"token_count":row.token_count,"source_type":row.source_type,"language":row.language,"metadata":row.metadata,"citation_text":row.citation_text,"source_reference":row.source_reference});
            let mut candidate = Candidate::from_chunk(row);
            candidate.text = candidate.text.trim().into();
            candidate.metadata = payload;
            candidate.sources.insert("bm25".into());
            candidate.bm25_score = Some(score);
            candidate.final_score = score;
            out.push(candidate)
        }
        out.sort_by(|a, b| b.final_score.total_cmp(&a.final_score));
        out.truncate(c.keyword_k);
        Ok(out)
    }
    pub async fn retrieve_batch(
        &self,
        user: &UserContext,
        dataset_id: &str,
        primary: RetrieveRequest,
        queries: Vec<RetrieveRequest>,
        max_parallel: usize,
    ) -> Result<(Vec<Value>, Value)> {
        self.vectors
            .with_interactive_budget(self.retrieve_batch_within_budget(
                user,
                dataset_id,
                primary,
                queries,
                max_parallel,
            ))
            .await
    }
    async fn retrieve_batch_within_budget(
        &self,
        user: &UserContext,
        dataset_id: &str,
        primary: RetrieveRequest,
        queries: Vec<RetrieveRequest>,
        max_parallel: usize,
    ) -> Result<(Vec<Value>, Value)> {
        let started = Instant::now();
        if !(1..=20).contains(&queries.len()) || !(1..=10).contains(&max_parallel) {
            return Err(QueryError::Validation(
                "batch requires 1-20 queries and max_parallel between 1 and 10".into(),
            ));
        }
        let count = queries.len();
        let mut seen = BTreeSet::new();
        let specs: Vec<_> = queries
            .into_iter()
            .filter(|r| seen.insert(r.query.trim().to_owned()))
            .collect();
        let dataset = self
            .store
            .require_dataset_access(user, dataset_id, "viewer")
            .await?;
        self.preflight(&dataset).await?;
        let cfg = config::resolve(&dataset, &primary)?;
        let mut service = self.clone();
        service.max_query_concurrency = max_parallel.min(self.max_query_concurrency).max(1);
        let (results, mut metadata) = service
            .pipeline(
                user,
                &dataset,
                &primary,
                &specs,
                cfg,
                self.vectors.publication_deadline(self.interactive_deadline),
            )
            .await?;
        metadata["queries"] = json!(specs.iter().map(|r| &r.query).collect::<Vec<_>>());
        metadata["input_query_count"] = json!(count);
        metadata["unique_query_count"] = json!(specs.len());
        metadata["duplicate_query_count"] = json!(count - specs.len());
        metadata["final_top_k"] = json!(primary.top_k);
        stamp_observation(&mut metadata, &primary.query);
        let meta = json!({"execution_time_ms":started.elapsed().as_secs_f64()*1000.0,"total_queries":count,"unique_queries":specs.len(),"final_top_k":primary.top_k,"total_results":results.len(),"max_parallel":service.max_query_concurrency,"dedupe_results":true,"avg_queue_wait_ms":0.0,"max_queue_wait_ms":0.0,"trace_id":metadata["trace_id"],"query_fingerprint":metadata["query_fingerprint"]});
        self.telemetry(
            dataset,
            user.clone(),
            primary.clone(),
            results.clone(),
            metadata.clone(),
        );
        Ok((
            vec![json!({"query":primary.query,"results":results,"meta":metadata})],
            meta,
        ))
    }
    fn telemetry(
        &self,
        d: Dataset,
        u: UserContext,
        r: RetrieveRequest,
        results: Vec<RetrieveResult>,
        meta: Value,
    ) {
        let store = self.store.clone();
        tokio::spawn(async move {
            let _ = store
                .record_query(
                    &d,
                    &u,
                    &r.query,
                    &r.mode,
                    if r.telemetry_source.is_empty() {
                        "api"
                    } else {
                        &r.telemetry_source
                    },
                    r.top_k,
                    &results,
                    &meta,
                )
                .await;
        });
    }
}
fn candidate_from_hit(hit: VectorHit, source: &str) -> Option<Candidate> {
    let text = hit.payload["text"].as_str()?.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    let id = hit.payload["segment_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&hit.point_id)
        .to_owned();
    let document = hit.payload["document_id"].as_str().unwrap_or("").to_owned();
    Some(Candidate {
        segment_id: id,
        document_id: document,
        text,
        metadata: hit.payload,
        point_id: Some(hit.point_id),
        dense_score: None,
        bm25_score: None,
        fusion_score: None,
        rerank_score: None,
        final_score: hit.score,
        sources: BTreeSet::from([source.into()]),
    })
}
fn merge_candidate(map: &mut BTreeMap<String, Candidate>, order: &mut Vec<String>, c: Candidate) {
    if let Some(old) = map.get_mut(&c.segment_id) {
        old.sources.extend(c.sources);
        if let Some(score) = c.dense_score {
            old.dense_score = Some(old.dense_score.map_or(score, |v| v.max(score)))
        }
        if let Some(score) = c.bm25_score {
            old.bm25_score = Some(old.bm25_score.map_or(score, |v| v.max(score)))
        }
    } else {
        order.push(c.segment_id.clone());
        map.insert(c.segment_id.clone(), c);
    }
}
fn matches_filters(payload: &Value, r: &RetrieveRequest) -> bool {
    let get = |key: &str| payload.get(key).or_else(|| payload["metadata"].get(key));
    if r.source_type_filter
        .as_deref()
        .is_some_and(|v| get("source_type").and_then(Value::as_str) != Some(v))
        || r.language_filter
            .as_deref()
            .is_some_and(|v| get("language").and_then(Value::as_str) != Some(v))
    {
        return false;
    }
    if let Some(m) = r.metadata_filter.as_ref().and_then(Value::as_object) {
        for (k, v) in m {
            if get(k) != Some(v) {
                return false;
            }
        }
    }
    true
}
fn contains_image(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, v)| {
            (k.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .eq_ignore_ascii_case("contenttype")
                && v.as_str().is_some_and(|v| {
                    ["image", "page_image", "mixed", "multimodal"]
                        .contains(&v.trim().to_lowercase().as_str())
                }))
                || contains_image(v)
        }),
        Value::Array(a) => a.iter().any(contains_image),
        _ => false,
    }
}
pub fn stamp_observation(meta: &mut Value, query: &str) {
    let normalized = query
        .nfkc()
        .case_fold()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    meta["trace_id"] = json!(uuid::Uuid::new_v4().to_string());
    meta["query_fingerprint"] = json!(format!("{:x}", Sha256::digest(normalized.as_bytes())));
}
