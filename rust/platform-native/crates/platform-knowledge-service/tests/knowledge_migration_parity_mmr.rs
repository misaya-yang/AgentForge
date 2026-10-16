//! Production MMR contracts from test_b5_mmr_fill_policy.py.
//! DB-backed pipeline metadata/authority assertions remain separate obligations.
use ai_platform_knowledge_service::migration_knowledge_query::{
    config::{self, RetrieveRequest},
    lexical::{cosine, mmr_select},
    types::Dataset,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}
fn fixture() -> (
    Vec<String>,
    BTreeMap<String, f64>,
    BTreeMap<String, Vec<f64>>,
) {
    (
        ids(&["a", "b", "c", "d"]),
        [("a", 1.0), ("b", 0.95), ("c", 0.9), ("d", 0.1)]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect(),
        [
            ("a", vec![1.0, 0.0]),
            ("b", vec![0.99999, 0.0045]),
            ("c", vec![0.99998, 0.0063]),
            ("d", vec![0.0, 1.0]),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect(),
    )
}
#[test]
fn strict_rejects_duplicates_and_fill_restores_only_unannotated_tail() {
    let (order, relevance, vectors) = fixture();
    let (strict, strict_picks) =
        mmr_select(&order, &relevance, &vectors, 4, 0.5, Some(0.99), "strict").unwrap();
    let (fill, fill_picks) =
        mmr_select(&order, &relevance, &vectors, 4, 0.5, Some(0.99), "fill").unwrap();
    assert_eq!(strict, ids(&["a", "d"]));
    assert_eq!(fill, ids(&["a", "d", "b", "c"]));
    assert_eq!(
        strict_picks.keys().cloned().collect::<Vec<_>>(),
        ids(&["a", "d"])
    );
    assert_eq!(
        fill_picks.keys().cloned().collect::<Vec<_>>(),
        ids(&["a", "d"])
    );
}
#[test]
fn no_threshold_has_same_selection_for_both_policies() {
    let (order, relevance, vectors) = fixture();
    let strict = mmr_select(&order, &relevance, &vectors, 3, 0.5, None, "strict").unwrap();
    let fill = mmr_select(&order, &relevance, &vectors, 3, 0.5, None, "fill").unwrap();
    assert_eq!(strict.0, fill.0);
    assert_eq!(strict.0.len(), 3);
}
#[test]
fn empty_zero_k_and_empty_ids_do_not_select() {
    let (order, relevance, vectors) = fixture();
    for (candidates, k) in [(Vec::new(), 3), (order, 0), (ids(&["", ""]), 2)] {
        let (selected, picks) =
            mmr_select(&candidates, &relevance, &vectors, k, 0.5, None, "strict").unwrap();
        assert!(selected.is_empty());
        assert!(picks.is_empty());
    }
}
#[test]
fn extreme_lambda_selects_relevance_or_diversity() {
    let (order, relevance, vectors) = fixture();
    assert_eq!(
        mmr_select(&order, &relevance, &vectors, 2, 1.0, None, "strict")
            .unwrap()
            .0,
        ids(&["a", "b"])
    );
    assert_eq!(
        mmr_select(&order, &relevance, &vectors, 2, 0.0, None, "strict")
            .unwrap()
            .0,
        ids(&["a", "d"])
    );
}
fn brute_force(
    order: &[String],
    relevance: &BTreeMap<String, f64>,
    vectors: &BTreeMap<String, Vec<f64>>,
    k: usize,
    lambda: f64,
) -> Vec<String> {
    let mut selected = Vec::<String>::new();
    while selected.len() < k {
        let mut best = None;
        let mut score = f64::NEG_INFINITY;
        for id in order.iter().filter(|id| !selected.contains(id)) {
            let sim = selected
                .iter()
                .filter_map(|s| Some(cosine(vectors.get(id)?, vectors.get(s)?)))
                .fold(0.0, f64::max);
            let candidate =
                lambda * relevance.get(id).copied().unwrap_or(0.0) - (1.0 - lambda) * sim;
            if candidate > score {
                score = candidate;
                best = Some(id.clone());
            }
        }
        let Some(id) = best else { break };
        selected.push(id);
    }
    selected
}
#[test]
fn incremental_selection_matches_independent_brute_force_and_pick_metrics() {
    // Deterministic bounded vector families with positive/negative coordinates,
    // zero-floor cosine behavior and an injected near-duplicate pair.
    for seed in [11u64, 23, 42] {
        let mut state = seed;
        let mut random = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 11) as f64 / ((1u64 << 53) as f64)
        };
        let mut order: Vec<String> = (0..24).map(|i| format!("seg-{i}")).collect();
        let mut vectors: BTreeMap<String, Vec<f64>> = order
            .iter()
            .map(|id| (id.clone(), (0..8).map(|_| random() * 2.0 - 1.0).collect()))
            .collect();
        vectors.insert(
            "seg-5".into(),
            vectors["seg-4"]
                .iter()
                .map(|v| v * 1.001 + 0.0001)
                .collect(),
        );
        let relevance: BTreeMap<String, f64> =
            order.iter().map(|id| (id.clone(), random())).collect();
        order.sort_by(|a, b| relevance[b].total_cmp(&relevance[a]));
        for lambda in [0.0, 0.35, 0.5, 1.0] {
            let want = brute_force(&order, &relevance, &vectors, 7, lambda);
            let (got, picks) =
                mmr_select(&order, &relevance, &vectors, 7, lambda, None, "strict").unwrap();
            assert_eq!(got, want);
            for (index, id) in want.iter().enumerate() {
                let sim = want[..index]
                    .iter()
                    .map(|s| cosine(&vectors[id], &vectors[s]))
                    .fold(0.0, f64::max);
                assert!((picks[id].relevance - relevance[id]).abs() < 1e-9);
                assert!((picks[id].max_sim_to_selected - sim).abs() < 1e-9);
                assert!(
                    (picks[id].mmr_score - (lambda * relevance[id] - (1.0 - lambda) * sim)).abs()
                        < 1e-9
                );
            }
        }
    }
}
#[test]
fn missing_vectors_and_zero_norms_keep_reference_order() {
    let order = ids(&["m", "a", "z"]);
    let relevance = [("a".into(), 0.5), ("z".into(), 0.4), ("m".into(), 1.0)]
        .into_iter()
        .collect();
    let vectors = [("a".into(), vec![1.0, 0.0]), ("z".into(), vec![0.0, 0.0])]
        .into_iter()
        .collect();
    let (got, picks) = mmr_select(&order, &relevance, &vectors, 3, 0.5, None, "strict").unwrap();
    assert_eq!(got, brute_force(&order, &relevance, &vectors, 3, 0.5));
    assert_eq!(got, order);
    assert_eq!(picks["m"].max_sim_to_selected, 0.0);
}
#[test]
fn resolved_policy_defaults_shorthand_precedence_and_invalid_values() {
    let resolve = |mmr: Value| {
        config::resolve(
            &Dataset {
                index_config: json!({"retrieval":{"mmr":mmr}}),
                ..Default::default()
            },
            &RetrieveRequest {
                query: "alpha".into(),
                ..Default::default()
            },
        )
    };
    for (value, expected) in [
        (json!({"enabled":true}), "strict"),
        (json!({"strict_diversity":false}), "fill"),
        (
            json!({"strict_diversity":false,"fill_policy":"strict"}),
            "strict",
        ),
        (json!({"fill_policy":"fill"}), "fill"),
    ] {
        assert_eq!(resolve(value).unwrap().mmr_fill_policy, expected);
    }
    for bad in [json!("resurrect"), json!("any"), json!("True"), json!(1)] {
        assert!(
            resolve(json!({"fill_policy":bad}))
                .unwrap_err()
                .to_string()
                .contains("mmr.fill_policy")
        );
    }
    assert!(
        resolve(json!({"strict_diversity":"yes"}))
            .unwrap_err()
            .to_string()
            .contains("mmr.strict_diversity")
    );
}
