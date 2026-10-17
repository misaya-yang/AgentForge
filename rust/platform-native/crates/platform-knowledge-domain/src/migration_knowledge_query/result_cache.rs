//! Bounded, weighted cache; authorization and generation checks remain caller-owned.
use super::super::types::{Dataset, IndexGeneration, RetrieveResult, UserContext};
use moka::future::Cache;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
const RESULT_CACHE_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Clone)]
struct Entry {
    inserted: Instant,
    ttl: Duration,
    results: Vec<RetrieveResult>,
    metadata: Value,
    weight: u32,
}
#[derive(Clone)]
pub(super) struct ResultCache {
    entries: Cache<String, Arc<Entry>>,
    max_bytes: u64,
}
pub(super) use super::super::cache_identity::{acl_scope_hash, normalize_text as normalize_query};
fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
pub(super) fn key(
    user: &UserContext,
    dataset: &Dataset,
    generation: &IndexGeneration,
    request: &Value,
) -> String {
    let mut profile_request = request.clone();
    let query = normalize_query(request["query"].as_str().unwrap_or(""));
    if let Some(object) = profile_request.as_object_mut() {
        object.remove("query");
    }
    // The complete dataset snapshot includes current index/embedding/rerank defaults,
    // lexical profile, collection and model versions; request carries every override/filter.
    let profile =
        hash(&json!({"dataset":dataset,"request":profile_request,"contract":"standard-v3"}));
    hash(
        &json!({"tenant":dataset.tenant_id,"dataset":dataset.dataset_id,"generation":generation,"acl_scope_hash":acl_scope_hash(user),"normalized_query":query,"exact_input_sha256":super::super::cache_identity::input_hash(request["query"].as_str().unwrap_or("")),"profile_fingerprint":profile,"model_plane_scope":super::super::model_plane::cache_scope_identity()}),
    )
}
/// Degraded upstream results must not mask recovery on later requests.
pub(super) fn cacheable_metadata(meta: &Value) -> bool {
    meta["rerank_degraded"].is_null()
        && meta["mmr_error"].is_null()
        && meta
            .get("recall_errors")
            .is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty))
}
impl Default for ResultCache {
    fn default() -> Self {
        Self::new(RESULT_CACHE_BYTES)
    }
}
impl ResultCache {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            entries: Cache::builder()
                .max_capacity(max_bytes)
                .weigher(|key: &String, value: &Arc<Entry>| {
                    value
                        .weight
                        .saturating_add(key.len().min(u32::MAX as usize) as u32)
                })
                .time_to_idle(Duration::from_secs(3600))
                .build(),
            max_bytes,
        }
    }
    pub(super) async fn get(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Option<(Vec<RetrieveResult>, Value)> {
        self.get_at(key, ttl, Instant::now()).await
    }
    async fn get_at(
        &self,
        key: &str,
        ttl: Duration,
        now: Instant,
    ) -> Option<(Vec<RetrieveResult>, Value)> {
        if ttl.is_zero() {
            return None;
        }
        let entry = self.entries.get(key).await?;
        if now.saturating_duration_since(entry.inserted) >= ttl.min(entry.ttl) {
            // Expired entries are never returned. Moka's idle/weight eviction reclaims
            // storage; avoid invalidating a concurrent fresh replacement of this key.
            return None;
        }
        Some((entry.results.clone(), entry.metadata.clone()))
    }
    pub(super) async fn set(
        &self,
        key: String,
        results: Vec<RetrieveResult>,
        metadata: Value,
        ttl: Duration,
    ) {
        self.set_aged(key, results, metadata, ttl, Duration::ZERO)
            .await;
    }
    pub(super) async fn set_aged(
        &self,
        key: String,
        results: Vec<RetrieveResult>,
        metadata: Value,
        ttl: Duration,
        age: Duration,
    ) {
        if age >= ttl || ttl.is_zero() || self.max_bytes == 0 {
            return;
        }
        let Some(inserted) = Instant::now().checked_sub(age) else {
            return;
        };
        let weight = retained_weight(&results, &metadata);
        let total = weight.saturating_add(key.len());
        if total as u64 > self.max_bytes || total > u32::MAX as usize {
            return;
        }
        self.entries
            .insert(
                key,
                Arc::new(Entry {
                    inserted: inserted,
                    ttl,
                    results,
                    metadata,
                    weight: weight as u32,
                }),
            )
            .await;
    }
}
// Weight retained allocations rather than serializing/cloning an entire response.
// Object node links/allocator bookkeeping remain an estimate, not exact process RSS.
fn json_heap_weight(value: &Value) -> usize {
    match value {
        Value::String(s) => s.capacity(),
        Value::Array(values) => values
            .capacity()
            .saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(
                values
                    .iter()
                    .map(json_heap_weight)
                    .fold(0usize, usize::saturating_add),
            ),
        Value::Object(values) => values.iter().fold(0usize, |sum, (key, value)| {
            sum.saturating_add(
                std::mem::size_of::<(String, Value)>() + 4 * std::mem::size_of::<usize>(),
            )
            .saturating_add(key.capacity())
            .saturating_add(json_heap_weight(value))
        }),
        _ => 0,
    }
}
fn retained_weight(results: &Vec<RetrieveResult>, metadata: &Value) -> usize {
    results.iter().fold(
        std::mem::size_of::<Entry>()
            .saturating_add(128)
            .saturating_add(
                results
                    .capacity()
                    .saturating_mul(std::mem::size_of::<RetrieveResult>()),
            )
            .saturating_add(json_heap_weight(metadata)),
        |sum, r| {
            let strings = [&r.segment_id, &r.document_id, &r.text, &r.content_type];
            let bytes = strings
                .iter()
                .map(|s| s.capacity())
                .fold(0usize, usize::saturating_add);
            sum.saturating_add(bytes)
                .saturating_add(r.image_url.as_ref().map_or(0, String::capacity))
                .saturating_add(r.vlm_description.as_ref().map_or(0, String::capacity))
                .saturating_add(json_heap_weight(&r.metadata))
                .saturating_add(
                    r.associated_images
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Value>()),
                )
                .saturating_add(
                    r.associated_images
                        .iter()
                        .map(json_heap_weight)
                        .fold(0usize, usize::saturating_add),
                )
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn user() -> UserContext {
        serde_json::from_value(json!({"user_id":"u","tenant_id":"t","roles":["reader"]})).unwrap()
    }
    fn dataset() -> Dataset {
        Dataset {
            dataset_id: "d".into(),
            tenant_id: "t".into(),
            content_revision: Some(1),
            collection_name: Some("collection".into()),
            ..Default::default()
        }
    }
    fn cache_key(user: &UserContext, dataset: &Dataset, query: &str) -> String {
        key(
            user,
            dataset,
            &dataset.generation().unwrap(),
            &json!({"query":query,"top_k":10}),
        )
    }
    #[test]
    fn acl_tenant_role_tier_authentication_and_dataset_are_distinct() {
        let u = user();
        let d = dataset();
        let original = cache_key(&u, &d, "q");
        for changed in [
            json!({"tenant_id":"other"}),
            json!({"user_id":"other"}),
            json!({"roles":["admin"]}),
            json!({"user_tier":"admin"}),
            json!({"user_type":"service"}),
            json!({"is_authenticated":false}),
        ] {
            let mut v = serde_json::to_value(&u).unwrap();
            v.as_object_mut()
                .unwrap()
                .extend(changed.as_object().unwrap().clone());
            let other = serde_json::from_value(v).unwrap();
            assert_ne!(original, cache_key(&other, &d, "q"));
        }
        let mut other = d.clone();
        other.dataset_id = "other".into();
        assert_ne!(original, cache_key(&u, &other, "q"));
    }
    #[test]
    fn publication_model_profile_and_exact_input_changes_miss_but_role_order_canonicalizes() {
        let mut u = user();
        u.roles = vec!["b".into(), "a".into()];
        let d = dataset();
        let original = cache_key(&u, &d, "one two");
        u.roles = vec!["a".into(), "b".into(), "a".into()];
        assert_eq!(original, cache_key(&u, &d, "one two"));
        assert_ne!(original, cache_key(&u, &d, " one\n two "));
        let mut other = d.clone();
        other.content_revision = Some(2);
        assert_ne!(original, cache_key(&u, &other, "one two"));
        let mut other = d.clone();
        other.embedding_model_version = Some("revision-2".into());
        assert_ne!(original, cache_key(&u, &other, "one two"));
        let mut other = d.clone();
        other.index_config = json!({"retrieval":{"rerank_model":"other"}});
        assert_ne!(original, cache_key(&u, &other, "one two"));
        assert_ne!(
            original,
            key(
                &u,
                &d,
                &d.generation().unwrap(),
                &json!({"query":"one two","top_k":11})
            )
        );
    }
    #[test]
    fn batch_query_sets_filters_and_hierarchy_are_distinct_and_degradation_is_not_cacheable() {
        let u = user();
        let d = dataset();
        let generation = d.generation().unwrap();
        let base =
            json!({"query":"q","batch_specs":[{"query":"one","top_k":3}],"hierarchical":false});
        let original = key(&u, &d, &generation, &base);
        for replacement in [
            json!({"query":"two","top_k":3}),
            json!({"query":"one","top_k":3,"metadata_filter":{"team":"other"}}),
        ] {
            let mut other = base.clone();
            other["batch_specs"][0] = replacement;
            assert_ne!(original, key(&u, &d, &generation, &other));
        }
        let mut hierarchy = base.clone();
        hierarchy["hierarchical"] = json!(true);
        assert_ne!(original, key(&u, &d, &generation, &hierarchy));
        assert!(cacheable_metadata(&json!({"recall_errors":[]})));
        for meta in [
            json!({"recall_errors":["dense failed"]}),
            json!({"rerank_degraded":"timeout"}),
            json!({"mmr_error":"vector unavailable"}),
        ] {
            assert!(!cacheable_metadata(&meta));
        }
        let array = json!([null, null, null, null, null, null, null, null]);
        assert!(json_heap_weight(&array) >= 8 * std::mem::size_of::<Value>());
    }
    #[tokio::test]
    async fn concurrent_acl_partitions_and_late_old_generation_writer_do_not_poison_current_keys() {
        let cache = ResultCache::new(1024 * 1024);
        let ready = std::sync::Arc::new(tokio::sync::Barrier::new(33));
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..32 {
            let (cache, ready) = (cache.clone(), ready.clone());
            tasks.spawn(async move {
                let mut actor = user();
                actor.roles = vec![format!("role-{index}")];
                let k = cache_key(&actor, &dataset(), "q");
                cache
                    .set(
                        k.clone(),
                        vec![],
                        json!({"partition":index}),
                        Duration::from_secs(60),
                    )
                    .await;
                ready.wait().await;
                assert_eq!(
                    cache.get(&k, Duration::from_secs(60)).await.unwrap().1["partition"],
                    index
                );
            });
        }
        ready.wait().await;
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        let actor = user();
        let before = dataset();
        let mut after = before.clone();
        after.content_revision = Some(2);
        let old_key = cache_key(&actor, &before, "q");
        let new_key = cache_key(&actor, &after, "q");
        cache
            .set(
                new_key.clone(),
                vec![],
                json!({"revision":2}),
                Duration::from_secs(60),
            )
            .await;
        // An in-flight pre-publication computation finishes after publication.
        cache
            .set(
                old_key,
                vec![],
                json!({"revision":1}),
                Duration::from_secs(60),
            )
            .await;
        assert_eq!(
            cache
                .get(&new_key, Duration::from_secs(60))
                .await
                .unwrap()
                .1["revision"],
            2
        );
    }
    #[tokio::test]
    async fn actual_lookup_cannot_cross_acl_or_generation_and_returns_detached_metadata() {
        let c = ResultCache::new(4096);
        let u = user();
        let d = dataset();
        let k = cache_key(&u, &d, "q");
        let ttl = Duration::from_secs(60);
        c.set(k.clone(), vec![], json!({"nested":{"value":1}}), ttl)
            .await;
        let (_, mut metadata) = c.get(&k, ttl).await.unwrap();
        metadata["nested"]["value"] = json!(2);
        assert_eq!(c.get(&k, ttl).await.unwrap().1["nested"]["value"], 1);
        let mut other = u.clone();
        other.roles.push("admin".into());
        assert!(c.get(&cache_key(&other, &d, "q"), ttl).await.is_none());
        let mut next = d.clone();
        next.content_revision = Some(2);
        assert!(c.get(&cache_key(&u, &next, "q"), ttl).await.is_none());
        assert!(c.get_at(&k, ttl, Instant::now() + ttl).await.is_none());
        // Raising TTL cannot resurrect an entry that expired under its admission TTL.
        assert!(c.get_at(&k, ttl * 10, Instant::now() + ttl).await.is_none());
    }
    #[tokio::test]
    async fn disabled_and_oversized_payloads_are_not_admitted_and_weight_is_bounded() {
        let c = ResultCache::new(1024);
        let ttl = Duration::from_secs(60);
        c.set("disabled".into(), vec![], json!({}), Duration::ZERO)
            .await;
        assert!(c.get("disabled", ttl).await.is_none());
        c.set(
            "large".into(),
            vec![],
            json!({"text":"x".repeat(2048)}),
            ttl,
        )
        .await;
        assert!(c.get("large", ttl).await.is_none());
        for i in 0..100 {
            c.set(
                i.to_string(),
                vec![],
                json!({"payload":"x".repeat(100)}),
                ttl,
            )
            .await;
        }
        c.entries.run_pending_tasks().await;
        assert!(c.entries.weighted_size() <= 1024);
    }
    #[tokio::test]
    async fn remote_promotion_keeps_original_admission_age_under_ttl_changes() {
        let cache = ResultCache::new(4096);
        let ttl = Duration::from_secs(20);
        cache
            .set_aged(
                "promoted".into(),
                vec![],
                json!({}),
                ttl,
                Duration::from_secs(15),
            )
            .await;
        assert!(cache.get("promoted", ttl).await.is_some());
        assert!(
            cache
                .get("promoted", Duration::from_secs(10))
                .await
                .is_none()
        );
        assert!(
            cache
                .get_at(
                    "promoted",
                    ttl * 10,
                    Instant::now() + Duration::from_secs(5)
                )
                .await
                .is_none()
        );
    }
}
