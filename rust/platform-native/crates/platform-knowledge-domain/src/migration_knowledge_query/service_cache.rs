//! Retrieval cache validation and observations, unchanged from the owning service.
use super::*;
impl RetrievalService {
    pub fn with_shared_result_cache(
        mut self,
        connection: Option<ai_platform_service_primitives::valkey::CacheConnection>,
    ) -> Self {
        self.shared_cache = connection.map(shared_result_cache::SharedResultCache::new);
        self
    }

    pub(in super::super) fn result_cache_key(
        &self,
        user: &UserContext,
        dataset: &Dataset,
        request: &RetrieveRequest,
        batch: Option<&[RetrieveRequest]>,
    ) -> Result<String> {
        let mut value =
            serde_json::to_value(request).map_err(|_| QueryError::InvalidProviderResponse)?;
        if let Some(batch) = batch {
            value["batch_specs"] =
                serde_json::to_value(batch).map_err(|_| QueryError::InvalidProviderResponse)?;
        }
        Ok(result_cache::key(
            user,
            dataset,
            &dataset.generation()?,
            &value,
        ))
    }
    pub(super) fn result_cache_profile_available(
        &self,
        dataset: &Dataset,
        request: &RetrieveRequest,
    ) -> Result<bool> {
        let config = config::resolve(dataset, request)?;
        Ok(config.mode == "bm25"
            || super::super::embedding::canonical_provider(
                dataset.embedding_provider.as_deref().unwrap_or("local"),
            ) == "local"
            || dataset
                .embedding_model_version
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty()))
    }
    pub(in super::super) async fn lookup_cached_result(
        &self,
        user: &UserContext,
        dataset: &Dataset,
        generation: &IndexGeneration,
        request: &RetrieveRequest,
        fingerprint: &str,
        started: Instant,
    ) -> Result<Option<(Vec<RetrieveResult>, Value)>> {
        if self.cache_ttl.is_zero()
            || user.user_id.is_empty()
            || !self.result_cache_profile_available(dataset, request)?
        {
            return Ok(None);
        }
        let cached = match self.cache.get(fingerprint, self.cache_ttl).await {
            Some(hit) => Some(hit),
            None => {
                if let Some(remote) = &self.shared_cache {
                    match remote
                        .get(&user.tenant_id, fingerprint, self.cache_ttl)
                        .await?
                    {
                        Some(hit) => {
                            self.cache
                                .set_aged(
                                    fingerprint.into(),
                                    hit.results.clone(),
                                    hit.metadata.clone(),
                                    hit.ttl,
                                    hit.age,
                                )
                                .await;
                            Some((hit.results, hit.metadata))
                        }
                        None => None,
                    }
                } else {
                    None
                }
            }
        };
        let Some((results, mut meta)) = cached else {
            return Ok(None);
        };
        if !result_cache::cacheable_metadata(&meta) {
            return Ok(None);
        }
        let ids: Vec<_> = results
            .iter()
            .filter(|r| !request.hierarchical || r.metadata["level"] != 1)
            .map(|r| r.segment_id.clone())
            .collect();
        let active = self.store.active_segment_ids(dataset, &ids).await?;
        if active != ids.into_iter().collect() {
            return Ok(None);
        }
        if request.hierarchical {
            let summaries: Vec<_> = results
                .iter()
                .filter(|r| r.metadata["level"] == 1)
                .map(|r| {
                    let mut payload = r.metadata.clone();
                    payload["document_id"] = json!(r.document_id);
                    payload["segment_id"] = json!(r.segment_id);
                    VectorHit {
                        point_id: r.segment_id.clone(),
                        score: r.score,
                        payload,
                        vector: None,
                    }
                })
                .collect();
            let count = summaries.len();
            if !summaries.is_empty()
                && self.hierarchy_filter(dataset, summaries, 1).await?.len() != count
            {
                return Ok(None);
            }
        }
        // This rechecks current ACL a second time, after cached row lifecycle reads.
        self.store
            .require_generation(user, &dataset.dataset_id, generation)
            .await?;
        meta["retrieval_cache_hit"] = json!(true);
        meta["retrieval_query_fingerprint"] = json!(fingerprint);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        meta["timings_ms"]["total_ms"] = json!(elapsed);
        if meta.get("total_time_ms").is_some() {
            meta["total_time_ms"] = json!(elapsed);
        }
        stamp_observation(&mut meta, &request.query);
        Ok(Some((results, meta)))
    }
    pub(in super::super) async fn store_cached_result(
        &self,
        dataset: &Dataset,
        request: &RetrieveRequest,
        fingerprint: String,
        results: Vec<RetrieveResult>,
        meta: Value,
    ) -> Result<()> {
        if !self.cache_ttl.is_zero()
            && self.result_cache_profile_available(dataset, request)?
            && result_cache::cacheable_metadata(&meta)
        {
            if let Some(remote) = &self.shared_cache {
                remote
                    .set(
                        &dataset.tenant_id,
                        &fingerprint,
                        &results,
                        &meta,
                        self.cache_ttl,
                    )
                    .await?;
            }
            self.cache
                .set(fingerprint, results, meta, self.cache_ttl)
                .await;
        }
        Ok(())
    }
    pub(super) fn telemetry(
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
