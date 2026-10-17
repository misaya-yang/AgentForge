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
pub(super) fn normalize_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
/// Conservative per-actor ACL partition. Roles are a set; tier/type/authentication are
/// authority inputs too. Current dataset access is always rechecked before lookup.
pub(super) fn acl_scope_hash(user: &UserContext) -> String {
    let mut roles = user.roles.clone();
    roles.sort();
    roles.dedup();
    hash(
        &json!({"tenant":user.tenant_id,"user":user.user_id,"roles":roles,"tier":user.user_tier,"type":user.user_type,"authenticated":user.is_authenticated}),
    )
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
        hash(&json!({"dataset":dataset,"request":profile_request,"contract":"standard-v2"}));
    hash(
        &json!({"tenant":dataset.tenant_id,"dataset":dataset.dataset_id,"generation":generation,"acl_scope_hash":acl_scope_hash(user),"normalized_query":query,"profile_fingerprint":profile}),
    )
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
        if ttl.is_zero() || self.max_bytes == 0 {
            return;
        }
        let Ok(serialized) = serde_json::to_vec(&(&results, &metadata)) else {
            return;
        };
        // Account for strings/JSON/container overhead, not only entry count. This is
        // a conservative payload budget, not an allocator/RSS measurement.
        let weight = serialized
            .len()
            .saturating_mul(2)
            .saturating_add(results.len().saturating_mul(256))
            .saturating_add(std::mem::size_of::<Entry>());
        let total = weight.saturating_add(key.len());
        if total as u64 > self.max_bytes || total > u32::MAX as usize {
            return;
        }
        self.entries
            .insert(
                key,
                Arc::new(Entry {
                    inserted: Instant::now(),
                    ttl,
                    results,
                    metadata,
                    weight: weight as u32,
                }),
            )
            .await;
    }
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
    fn publication_model_and_profile_changes_miss_but_whitespace_and_role_order_canonicalize() {
        let mut u = user();
        u.roles = vec!["b".into(), "a".into()];
        let d = dataset();
        let original = cache_key(&u, &d, "one two");
        u.roles = vec!["a".into(), "b".into(), "a".into()];
        assert_eq!(original, cache_key(&u, &d, " one\n two "));
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
}
