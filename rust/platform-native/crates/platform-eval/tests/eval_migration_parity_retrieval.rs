//! All deterministic test_retrieval_metrics.py behavior, through the native shared scorer.
use ai_platform_eval::metrics::{QueryRetrievalJudgement, evaluate_retrieval, score_single_query};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}
fn grades(v: &[&str]) -> BTreeMap<String, f64> {
    v.iter().map(|s| (s.to_string(), 1.)).collect()
}
fn score(r: &[&str], relevant: &[&str], ks: &[usize]) -> Value {
    score_single_query(&strings(r), &grades(relevant), ks).unwrap()
}
fn near(value: &Value, expected: f64) {
    let value = value.as_f64().unwrap();
    assert!((value - expected).abs() < 1e-12, "{value} != {expected}");
}
#[test]
fn hit_rate_five_source_cases() {
    for (r, g, k, e) in [
        (vec!["a", "b", "c", "d"], vec!["a", "c"], 1, 1.),
        (vec!["x", "a"], vec!["a", "c"], 1, 0.),
        (vec!["x", "a"], vec!["a", "c"], 2, 1.),
        (vec!["x", "y"], vec!["a", "c"], 5, 0.),
        (vec!["a", "b", "c", "d"], vec![], 5, 0.),
    ] {
        near(&score(&r, &g, &[k])["by_k"][k.to_string()]["hit_rate"], e);
    }
}
#[test]
fn precision_requested_cutoff_includes_empty_slots() {
    for (r, k, e) in [
        (vec!["a", "b", "c", "d"], 2, 0.5),
        (vec!["a", "b", "c", "d"], 4, 0.5),
        (vec!["a", "b", "c", "d"], 1, 1.),
        (vec![], 5, 0.),
        (vec!["a"], 10, 0.1),
    ] {
        near(
            &score(&r, &["a", "c"], &[k])["by_k"][k.to_string()]["precision_at_k"],
            e,
        );
    }
}
#[test]
fn recall_three_source_cases() {
    for (r, k, e) in [
        (vec!["a", "b", "c", "d"], 4, 1.),
        (vec!["a", "b", "c", "d"], 1, 0.5),
        (vec!["x", "y"], 2, 0.),
    ] {
        near(
            &score(&r, &["a", "c"], &[k])["by_k"][k.to_string()]["recall_at_k"],
            e,
        );
    }
}
#[test]
fn reciprocal_rank_and_average_precision() {
    for (r, rr, ap) in [
        (vec!["a", "b", "c", "d"], 1., (1. + 2. / 3.) / 2.),
        (vec!["x", "c", "a"], 0.5, (0.5 + 2. / 3.) / 2.),
        (vec!["x", "y", "z"], 0., 0.),
    ] {
        let s = score(&r, &["a", "c"], &[1, 5]);
        near(&s["reciprocal_rank"], rr);
        near(&s["average_precision"], ap);
    }
    let s = score(&["a", "b", "c", "d"], &["a", "c"], &[1, 5]);
    near(&s["by_k"]["1"]["hit_rate"], 1.);
    near(&s["by_k"]["5"]["recall_at_k"], 1.);
}
#[test]
fn binary_and_graded_ndcg() {
    let ideal = 1. + 1. / 3f64.log2();
    near(
        &score(&["a", "b", "c", "d"], &["a", "c"], &[4])["by_k"]["4"]["ndcg_at_k"],
        1.5 / ideal,
    );
    near(
        &score(&["a", "c", "b", "d"], &["a", "c"], &[4])["by_k"]["4"]["ndcg_at_k"],
        1.,
    );
    near(
        &score(&["x", "y"], &["a", "c"], &[2])["by_k"]["2"]["ndcg_at_k"],
        0.,
    );
    let g = BTreeMap::from([("a".into(), 3.), ("b".into(), 2.), ("c".into(), 3.)]);
    let s = score_single_query(&strings(&["a", "b"]), &g, &[2]).unwrap();
    near(
        &s["by_k"]["2"]["ndcg_at_k"],
        (3. + 2. / 3f64.log2()) / (3. + 3. / 3f64.log2()),
    );
}
fn judgement(id: &str, r: &[&str], g: &[&str]) -> QueryRetrievalJudgement {
    QueryRetrievalJudgement {
        query_id: id.into(),
        retrieved: strings(r),
        relevance: grades(g),
    }
}
#[test]
fn aggregation_and_serialized_shape() {
    let report = evaluate_retrieval(
        &[
            judgement("q1", &["a", "c", "b"], &["a", "c"]),
            judgement("q2", &["x", "y", "z"], &["a"]),
        ],
        &[1, 3],
    )
    .unwrap();
    assert_eq!(report["k_values"], json!([1, 3]));
    near(&report["metrics"]["1"]["hit_rate"], 0.5);
    for field in ["mrr", "recall_at_k", "map"] {
        near(&report["metrics"]["3"][field], 0.5);
    }
    assert_eq!(report["metrics"]["3"]["num_queries"], 2);
    assert!(report["per_query"].get("q1").is_some());
}
#[test]
fn empty_evaluation_has_zero_metrics() {
    let r = evaluate_retrieval(&[], &[5]).unwrap();
    assert_eq!(r["metrics"]["5"]["num_queries"], 0);
    near(&r["metrics"]["5"]["mrr"], 0.);
}
#[test]
fn duplicate_ids_receive_relevance_credit_once() {
    let s = score(&["a", "a", "a", "b"], &["a"], &[1, 2, 4]);
    assert_eq!(s["retrieved_count"], 4);
    assert_eq!(s["unique_retrieved_count"], 2);
    near(&s["by_k"]["4"]["recall_at_k"], 1.);
    near(&s["by_k"]["4"]["precision_at_k"], 0.25);
    near(&s["average_precision"], 1.);
    near(&s["by_k"]["4"]["ndcg_at_k"], 1.);
    for row in s["by_k"].as_object().unwrap().values() {
        assert!(
            row.as_object()
                .unwrap()
                .values()
                .all(|v| (0.0..=1.0).contains(&v.as_f64().unwrap()))
        );
    }
}
#[test]
fn duplicate_slots_do_not_promote_later_relevant_results() {
    let s = score(
        &[
            "duplicate",
            "duplicate",
            "relevant-1",
            "relevant-1",
            "relevant-2",
        ],
        &["relevant-1", "relevant-2"],
        &[2, 3, 5],
    );
    assert_eq!(s["retrieved_count"], 5);
    assert_eq!(s["unique_retrieved_count"], 3);
    for f in [
        "hit_rate",
        "precision_at_k",
        "recall_at_k",
        "mrr",
        "map",
        "ndcg_at_k",
    ] {
        near(&s["by_k"]["2"][f], 0.);
    }
    near(&s["by_k"]["3"]["precision_at_k"], 1. / 3.);
    near(&s["by_k"]["5"]["precision_at_k"], 2. / 5.);
    near(&s["by_k"]["3"]["recall_at_k"], 0.5);
    near(&s["by_k"]["5"]["recall_at_k"], 1.);
    near(&s["reciprocal_rank"], 1. / 3.);
    near(&s["average_precision"], (1. / 3. + 2. / 5.) / 2.);
    near(&s["by_k"]["5"]["map"], (1. / 3. + 2. / 5.) / 2.);
    near(&s["by_k"]["5"]["mrr"], 1. / 3.);
    near(
        &s["by_k"]["5"]["ndcg_at_k"],
        (1. / 4f64.log2() + 1. / 6f64.log2()) / (1. + 1. / 3f64.log2()),
    );
}
#[test]
fn mrr_and_map_truncate_at_each_k() {
    let r = evaluate_retrieval(&[judgement("q", &["miss", "hit"], &["hit"])], &[1, 2]).unwrap();
    for f in ["mrr", "map"] {
        near(&r["metrics"]["1"][f], 0.);
        near(&r["metrics"]["2"][f], 0.5);
    }
}
#[test]
fn invalid_grades_cutoffs_and_duplicate_query_ids_fail_closed() {
    for grade in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -0.1] {
        let e = score_single_query(
            &strings(&["a"]),
            &BTreeMap::from([("a".into(), grade)]),
            &[1],
        )
        .unwrap_err();
        assert!(e.to_string().contains("finite and non-negative"));
    }
    for ks in [vec![], vec![0], vec![101]] {
        assert!(
            evaluate_retrieval(&[], &ks)
                .unwrap_err()
                .to_string()
                .contains("k_values")
        );
    }
    for invalid in [json!([-1]), json!([true]), json!([1.5])] {
        assert!(serde_json::from_value::<Vec<usize>>(invalid).is_err());
    }
    let e = evaluate_retrieval(
        &[
            judgement("same", &["a"], &["a"]),
            judgement("same", &["b"], &["b"]),
        ],
        &[1],
    )
    .unwrap_err();
    assert!(e.to_string().contains("duplicate query_id"));
}
