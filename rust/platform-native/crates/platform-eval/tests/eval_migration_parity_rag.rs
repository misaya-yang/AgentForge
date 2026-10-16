//! Retrieval/statistical half of the original RAG regression gate matrix.
use ai_platform_eval::{bootstrap, rag};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn fixtures() -> (Vec<Value>, BTreeMap<String, Value>) {
    let rows = |s: &str| {
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect::<Vec<Value>>()
    };
    let c = rows(include_str!(
        "../../../../../tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl"
    ));
    let o = ai_platform_eval::observations(&rows(include_str!(
        "../../../../../tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl"
    )))
    .unwrap();
    (c, o)
}
fn failure(v: &Value, message: &str) {
    assert!(
        v["gate"]["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains(message)),
        "{v}"
    );
}
#[test]
fn both_tracks_retrieval_ci_and_unrun_answer_quality_are_explicit() {
    let (c, o) = fixtures();
    assert_eq!(rag::validate_cases(&c)["valid"], true);
    assert_eq!(rag::validate_observations(&c, &o)["valid"], true);
    let r = rag::evaluate(&c, &o, &Default::default()).unwrap();
    assert_eq!(
        r["gate"],
        json!({"status":"pass","outcome":"retrieval_pass_answer_not_run","scope":"offline_retrieval_only","not_run":["answer_quality"],"failures":[]})
    );
    assert_eq!(r["sample_count"], 12);
    assert_eq!(
        r["track_counts"],
        json!({"retrieval_only":6,"answer_aware":6})
    );
    assert_eq!(r["retrieval"]["status"], "pass");
    assert_eq!(r["retrieval"]["critical"]["case_count"], 12);
    assert_eq!(r["retrieval"]["critical"]["status"], "pass");
    for m in ["recall_at_k", "mrr", "ndcg_at_k"] {
        assert_eq!(r["retrieval"]["all"]["metrics"]["5"][m], 1.);
        assert_eq!(
            r["retrieval"]["all"]["confidence_intervals_95"][m],
            json!([1., 1.])
        );
    }
    assert_eq!(r["answer_quality"]["status"], "not_run");
    assert_eq!(
        r["answer_quality"]["critical"],
        json!({"case_count":6,"status":"not_run","results":[]})
    );
    assert_eq!(
        r["answer_quality"]["evidence_scope"],
        "external_judge_opt_in_required"
    );
    assert!(r["answer_quality"]["metrics"]["faithfulness"]["mean"].is_null());
    assert_eq!(r["evidence_tiers"]["real_provider_call"], "not_run");
}
#[test]
fn critical_metadata_is_optional_boolean_only() {
    let (c, _) = fixtures();
    let mut absent = c[0].clone();
    absent["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("critical");
    let mut no = c[1].clone();
    no["metadata"]["critical"] = json!(false);
    assert_eq!(rag::validate_cases(&[absent, no])["valid"], true);
    let mut bad = c[2].clone();
    bad["metadata"]["critical"] = json!("yes");
    let r = rag::validate_cases(&[bad]);
    assert_eq!(r["valid"], false);
    assert!(
        r["errors"][0]["errors"]
            .as_array()
            .unwrap()
            .contains(&json!("metadata.critical must be boolean"))
    );
}
#[test]
fn one_critical_miss_blocks_large_strong_cohort() {
    let mut c = vec![];
    let mut o = BTreeMap::new();
    for track in ["retrieval_only", "answer_aware"] {
        for i in 0..100 {
            let id = format!("{track}.{i}");
            let segment = format!("segment-{id}");
            let mut case = json!({"case_id":id,"track":track,"query":format!("query {track} {i}"),"relevance":{segment.clone():1.},"metadata":{"critical":i==0}});
            let mut replay = json!({"status":"succeeded","ranked_segment_ids":[segment,"irrelevant-1","irrelevant-2","irrelevant-3","irrelevant-4"],"answer_source":"retrieval_only"});
            if track == "answer_aware" {
                case["reference_answer"] = json!(format!("reference {i}"));
                replay["answer"] = json!(format!("generated answer {i}"));
                replay["answer_source"] = json!("generated");
            }
            c.push(case);
            o.insert(id, replay);
        }
    }
    o.get_mut("retrieval_only.0").unwrap()["ranked_segment_ids"] = json!([
        "irrelevant-a",
        "irrelevant-b",
        "irrelevant-c",
        "irrelevant-d",
        "irrelevant-e"
    ]);
    let opts = rag::GateOptions {
        bootstrap_samples: 500,
        ..Default::default()
    };
    let r = rag::evaluate(&c, &o, &opts).unwrap();
    assert!(
        r["retrieval"]["all"]["confidence_intervals_95"]["recall_at_k"][0]
            .as_f64()
            .unwrap()
            > 0.8
    );
    assert_eq!(r["gate"]["status"], "fail");
    assert_eq!(r["retrieval"]["critical"]["status"], "fail");
    failure(&r, "critical RAG retrieval case");
}
#[test]
fn empty_and_undersized_samples_fail_closed() {
    let r = rag::evaluate(&[], &BTreeMap::new(), &Default::default()).unwrap();
    assert_eq!(r["gate"]["status"], "fail");
    failure(&r, "sample_count 0 < minimum 8");
    let (c, o) = fixtures();
    let subset = vec![c[0].clone(), c[6].clone()];
    let o = subset
        .iter()
        .map(|c| {
            let id = c["case_id"].as_str().unwrap();
            (id.to_string(), o[id].clone())
        })
        .collect();
    let r = rag::evaluate(&subset, &o, &Default::default()).unwrap();
    assert_eq!(r["gate"]["status"], "fail");
    for message in [
        "sample_count 2 < minimum 8",
        "retrieval_only sample_count 1 < minimum 4",
        "answer_aware sample_count 1 < minimum 4",
    ] {
        failure(&r, message);
    }
}
#[test]
fn valid_low_quality_rankings_fail_all_retrieval_thresholds() {
    let (c, mut o) = fixtures();
    for r in o.values_mut() {
        r["ranked_segment_ids"] = json!([
            "irrelevant-1",
            "irrelevant-2",
            "irrelevant-3",
            "irrelevant-4",
            "irrelevant-5"
        ]);
    }
    let r = rag::evaluate(&c, &o, &Default::default()).unwrap();
    assert_eq!(r["gate"]["status"], "fail");
    for m in ["recall_at_k", "mrr", "ndcg_at_k"] {
        failure(&r, &format!("{m} 0.0000 < 0.8000"));
    }
}
#[test]
fn bootstrap_and_gate_invariant_to_case_order() {
    let (mut c, o) = fixtures();
    let opts = rag::GateOptions {
        bootstrap_samples: 500,
        ..Default::default()
    };
    let forward = rag::evaluate(&c, &o, &opts).unwrap();
    c.reverse();
    let reverse = rag::evaluate(&c, &o, &opts).unwrap();
    assert_eq!(forward["retrieval"], reverse["retrieval"]);
    assert_eq!(forward["gate"], reverse["gate"]);
    let values = [1., 0.5, 0., 1., 0.5, 1.];
    let first = bootstrap::mean_ci(&values, 500);
    assert_eq!(first, bootstrap::mean_ci(&values, 500));
    let [low, high] = first.unwrap();
    assert!(0. <= low && low <= high && high <= 1.);
}
#[test]
fn track_answer_and_metric_contamination_fails_closed() {
    for (id, mutation, message) in [
        (
            "rag.retrieval.refund",
            json!({"answer":"document count is not a generated answer"}),
            "retrieval_only observation must not contain an answer",
        ),
        (
            "rag.answer.refund",
            json!({"answer":null}),
            "answer_aware observation requires a generated answer",
        ),
        (
            "rag.retrieval.password",
            json!({"answer_metrics":{"faithfulness":1.}}),
            "answer_metrics must use the explicit judge observation channel",
        ),
    ] {
        let (c, mut o) = fixtures();
        o.get_mut(id)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .extend(mutation.as_object().unwrap().clone());
        let v = rag::validate_observations(&c, &o);
        assert_eq!(v["valid"], false);
        assert!(v["errors"].to_string().contains(message));
        let r = rag::evaluate(&c, &o, &Default::default()).unwrap();
        assert_eq!(r["gate"]["status"], "fail");
        assert_eq!(r["answer_quality"]["status"], "not_run");
    }
}
#[test]
fn invalid_statistical_configuration_rejected() {
    let (c, o) = fixtures();
    for ks in [vec![], vec![0, 5], vec![101], vec![5, 5]] {
        let opts = rag::GateOptions {
            ks,
            ..Default::default()
        };
        assert!(rag::evaluate(&c, &o, &opts).is_err());
    }
    let opts = rag::GateOptions {
        bootstrap_samples: 99,
        ..Default::default()
    };
    assert!(rag::evaluate(&c, &o, &opts).is_err());
    for (k, v) in [("recall_at_k", 1.01), ("precision_at_k", 0.8)] {
        let mut opts = rag::GateOptions::default();
        opts.retrieval_thresholds.insert(k.into(), v);
        assert!(rag::evaluate(&c, &o, &opts).is_err());
    }
    for value in [json!(100.5), json!(true)] {
        assert!(serde_json::from_value::<usize>(value).is_err());
    }
}
