//! Source CacheManager TTL/LRU operations shared by actual retrieval and tests.
//! Caller owns its async mutex; no authority checks are bypassed by this helper.
use super::super::types::RetrieveResult;
use super::Cached;
use serde_json::Value;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(super) fn get(
    cache: &mut VecDeque<Cached>,
    key: &str,
    ttl: Duration,
    now: Instant,
) -> Option<(Vec<RetrieveResult>, Value)> {
    if ttl.is_zero() {
        return None;
    }
    let index = cache
        .iter()
        .position(|(candidate, _, _, _)| candidate == key)?;
    let entry = cache.remove(index)?;
    if now.saturating_duration_since(entry.1) >= ttl {
        return None;
    }
    let value = (entry.2.clone(), entry.3.clone());
    cache.push_back(entry);
    Some(value)
}

pub(super) fn set(
    cache: &mut VecDeque<Cached>,
    key: String,
    results: Vec<RetrieveResult>,
    meta: Value,
    ttl: Duration,
    max_entries: usize,
    now: Instant,
) {
    if ttl.is_zero() || max_entries == 0 {
        return;
    }
    // Replacing is a use, not another entry. Remove any prior duplicate too,
    // allowing an already-running service to recover from the former behavior.
    cache.retain(|(candidate, _, _, _)| candidate != &key);
    while cache.len() >= max_entries {
        cache.pop_front();
    }
    cache.push_back((key, now, results, meta));
}

#[cfg(test)]
mod knowledge_migration_parity_cache {
    use super::*;
    use serde_json::json;
    fn put(
        cache: &mut VecDeque<Cached>,
        key: &str,
        id: &str,
        ttl: Duration,
        max: usize,
        now: Instant,
    ) {
        let result = RetrieveResult {
            segment_id: id.into(),
            document_id: "doc".into(),
            text: id.into(),
            score: 1.0,
            metadata: json!({}),
            content_type: "text".into(),
            image_url: None,
            vlm_description: None,
            associated_images: vec![],
        };
        set(
            cache,
            key.into(),
            vec![result],
            json!({"tag":key}),
            ttl,
            max,
            now,
        );
    }
    #[test]
    fn hit_refreshes_recency_and_evicts_least_recently_used() {
        let mut cache = VecDeque::new();
        let now = Instant::now();
        let ttl = Duration::from_secs(60);
        put(&mut cache, "a", "seg-a", ttl, 2, now);
        put(&mut cache, "b", "seg-b", ttl, 2, now);
        assert!(get(&mut cache, "a", ttl, now).is_some());
        put(&mut cache, "c", "seg-c", ttl, 2, now);
        assert!(get(&mut cache, "a", ttl, now).is_some());
        assert!(get(&mut cache, "c", ttl, now).is_some());
        assert!(get(&mut cache, "b", ttl, now).is_none());
    }
    #[test]
    fn rewrite_refreshes_without_growth_and_preserves_latest_value() {
        let mut cache = VecDeque::new();
        let now = Instant::now();
        let ttl = Duration::from_secs(60);
        put(&mut cache, "a", "seg-a-v1", ttl, 2, now);
        put(&mut cache, "b", "seg-b", ttl, 2, now);
        put(&mut cache, "a", "seg-a-v2", ttl, 2, now);
        assert_eq!(cache.len(), 2);
        put(&mut cache, "c", "seg-c", ttl, 2, now);
        assert!(get(&mut cache, "a", ttl, now).is_some());
        assert!(get(&mut cache, "c", ttl, now).is_some());
        assert!(get(&mut cache, "b", ttl, now).is_none());
        assert_eq!(
            get(&mut cache, "a", ttl, now).unwrap().0[0].segment_id,
            "seg-a-v2"
        );
    }
    #[test]
    fn expired_entry_is_removed_on_access() {
        let mut cache = VecDeque::new();
        let now = Instant::now();
        let ttl = Duration::from_secs(1);
        put(&mut cache, "a", "seg-a", ttl, 8, now);
        assert!(get(&mut cache, "a", ttl, now + Duration::from_secs(2)).is_none());
        assert!(cache.is_empty());
    }
    #[test]
    fn disabled_ttl_short_circuits_read_and_write() {
        let mut cache = VecDeque::new();
        let now = Instant::now();
        put(&mut cache, "a", "seg-a", Duration::ZERO, 2, now);
        assert!(get(&mut cache, "a", Duration::ZERO, now).is_none());
        assert!(cache.is_empty());
    }
    #[test]
    fn returned_rows_and_nested_metadata_are_copies() {
        let mut cache = VecDeque::new();
        let now = Instant::now();
        let ttl = Duration::from_secs(60);
        put(&mut cache, "a", "seg-a", ttl, 2, now);
        let (mut rows, mut meta) = get(&mut cache, "a", ttl, now).unwrap();
        rows[0].text = "changed".into();
        meta["tag"] = json!("changed");
        let (rows, meta) = get(&mut cache, "a", ttl, now).unwrap();
        assert_eq!(rows[0].text, "seg-a");
        assert_eq!(meta["tag"], "a");
    }
}
