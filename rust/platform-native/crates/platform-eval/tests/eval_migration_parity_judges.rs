//! Recorded judge authority/provenance cases. Isolated subprocess environment pins, no provider calls.
use ai_platform_eval::{
    canonical::{digest, sha256},
    judges, rag,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn fixtures() -> (Vec<Value>, BTreeMap<String, Value>) {
    let rows = |s: &str| {
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect::<Vec<Value>>()
    };
    (
        rows(include_str!(
            "../../../../../tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl"
        )),
        ai_platform_eval::observations(&rows(include_str!(
            "../../../../../tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl"
        )))
        .unwrap(),
    )
}
fn cohort() -> Value {
    json!({"provider":"recorded-provider","model":"recorded-judge-model","judge_run_id":"judge-batch-1","rubric_version":"rag-answer-v1","judge_prompt_sha256":"a".repeat(64)})
}
fn rows(c: &[Value], o: &BTreeMap<String, Value>, score: f64) -> Vec<Value> {
    c.iter().filter(|c|c["track"]=="answer_aware").map(|c|{let id=c["case_id"].as_str().unwrap();let mut r=cohort();r.as_object_mut().unwrap().extend(json!({"case_id":id,"expectation_sha256":digest(c),"observation_sha256":digest(&o[id]),"metrics":{"faithfulness":score,"answer_relevancy":score}}).as_object().unwrap().clone());r}).collect()
}
fn policy(rows: &[Value]) -> Value {
    json!({"schema_version":"rag-judge-policy/v1","policy_id":"rag-answer-regression","revision":"v1","artifact":{"hash_domain":"rag-recorded-judge-rows/v1","sha256":digest(&json!({"hash_domain":"rag-recorded-judge-rows/v1","rows":rows}))},"cohort":cohort()})
}
fn prepared(mode: &str) -> (Vec<Value>, BTreeMap<String, Value>, Vec<Value>, Value) {
    let (c, mut o) = if mode == "critical_large" {
        large_cohort()
    } else {
        fixtures()
    };
    let mut r = rows(
        &c,
        &o,
        if mode == "low_missing" {
            0.2
        } else if mode == "rounding" {
            0.69996
        } else {
            0.9
        },
    );
    if mode == "mixed" {
        r[1]["provider"] = json!("other-provider");
        r[1]["model"] = json!("other-model");
        r[1]["rubric_version"] = json!("totally-different-v99");
        r[1]["judge_prompt_sha256"] = json!("b".repeat(64));
    }
    if mode == "provenance" {
        r[0]["observation_sha256"] = json!("f".repeat(64));
    }
    if mode == "low_missing" {
        r.pop();
    }
    if mode == "critical_large" {
        r[0]["metrics"] = json!({"faithfulness":0.1,"answer_relevancy":0.1});
    }
    let mut p = policy(&r);
    if mode == "wrong_cohort" {
        p["cohort"]["model"] = json!("different-expected-model");
    }
    if mode == "answer_changed" {
        o.get_mut("rag.answer.refund").unwrap()["answer"] = json!("Completely unsupported answer.");
    }
    if mode == "artifact_changed" {
        for row in &mut r {
            row["metrics"] = json!({"faithfulness":0.2,"answer_relevancy":0.2});
        }
    }
    match mode {
        "unknown_field" => p["unexpected"] = json!(true),
        "hash_domain" => p["artifact"]["hash_domain"] = json!("untrusted-domain/v9"),
        "artifact_digest" => p["artifact"]["sha256"] = json!("not-a-digest"),
        "cohort_fields" => p["cohort"]["unexpected"] = json!("drift"),
        _ => {}
    }
    (c, o, r, p)
}
#[test]
fn pinned_judge_behavior_matrix() {
    const MODE: &str = "EVAL_MIGRATION_JUDGE_CASE";
    let mode = std::env::var(MODE).ok();
    if mode.is_none() {
        for mode in [
            "pass",
            "critical_large",
            "mixed",
            "wrong_cohort",
            "answer_changed",
            "provenance",
            "low_missing",
            "artifact_changed",
            "rounding",
            "unpinned",
            "wrong_pin",
            "unknown_field",
            "hash_domain",
            "artifact_digest",
            "cohort_fields",
        ] {
            let (_, _, _, p) = prepared(mode);
            let raw = serde_json::to_vec(&p).unwrap();
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args(["--exact", "pinned_judge_behavior_matrix", "--nocapture"])
                .env(MODE, mode);
            if mode == "unpinned" {
                cmd.env_remove("RAG_JUDGE_POLICY_SHA256");
            } else {
                cmd.env(
                    "RAG_JUDGE_POLICY_SHA256",
                    if mode == "wrong_pin" {
                        "f".repeat(64)
                    } else {
                        sha256(&raw)
                    },
                );
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        return;
    }
    let mode = mode.unwrap();
    let (c, o, r, p) = prepared(&mode);
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("policy.json");
    std::fs::write(&path, serde_json::to_vec(&p).unwrap()).unwrap();
    let resolved = judges::resolve_policy(path.to_str().unwrap());
    if let Some(message) = match mode.as_str() {
        "unpinned" => Some("RAG_JUDGE_POLICY_SHA256"),
        "wrong_pin" => Some("trusted environment pin"),
        "unknown_field" => Some("invalid fields"),
        "hash_domain" => Some("hash domain is unsupported"),
        "artifact_digest" => Some("digest must be lowercase SHA-256"),
        "cohort_fields" => Some("invalid expected judge cohort"),
        _ => None,
    } {
        assert!(resolved.err().unwrap().to_string().contains(message));
        return;
    }
    let resolved = resolved.unwrap();
    let options = rag::GateOptions::default();
    let mut report = rag::evaluate(&c, &o, &options).unwrap();
    judges::apply(&mut report, &c, &o, &r, &resolved, &options, 0.7, 0.7).unwrap();
    if mode == "pass" {
        verify_cli_snapshot(t.path(), &r, &path);
        assert_eq!(report["gate"]["status"], "pass");
        assert_eq!(report["answer_quality"]["status"], "pass");
        assert_eq!(
            report["answer_quality"]["evidence_scope"],
            "recorded_external_judge_opt_in"
        );
        assert_eq!(
            report["answer_quality"]["metrics"]["faithfulness"]["sample_count"],
            6
        );
        assert_eq!(
            report["answer_quality"]["metrics"]["response_relevancy"]["mean"],
            0.9
        );
        let mut expected = cohort();
        expected["cohort_sha256"] = report["answer_quality"]["cohort"]["cohort_sha256"].clone();
        assert_eq!(report["answer_quality"]["cohort"], expected);
        assert_eq!(expected["cohort_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(
            report["answer_quality"]["artifact_binding"]["matched"],
            true
        );
        assert_eq!(
            report["answer_quality"]["provenance"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
        assert_eq!(
            report["gate"]["outcome"],
            "retrieval_and_recorded_answer_pass"
        );
        assert_eq!(
            report["evidence_tiers"]["external_answer_judge"],
            "recorded_bound_opt_in"
        );
        assert_eq!(report["evidence_tiers"]["real_provider_call"], "not_run");
        return;
    }
    assert_eq!(report["gate"]["status"], "fail", "{report}");
    let failures = report["gate"]["failures"].to_string();
    match mode.as_str() {
        "critical_large" => {
            assert!(
                report["answer_quality"]["metrics"]["faithfulness"]["ci_95"][0]
                    .as_f64()
                    .unwrap()
                    > 0.7
            );
            assert_eq!(report["retrieval"]["status"], "pass");
            assert_eq!(report["answer_quality"]["critical"]["status"], "fail");
            assert!(failures.contains("critical RAG answer case"));
        }
        "mixed" => {
            assert!(failures.contains("one provider/model/run/rubric/prompt cohort"));
            assert!(report["answer_quality"]["cohort"].is_null());
        }
        "wrong_cohort" => assert!(failures.contains("does not match the pinned expected cohort")),
        "answer_changed" => {
            assert!(failures.contains("observation hash mismatch"));
            assert_eq!(report["retrieval"]["status"], "pass");
            assert_eq!(report["answer_quality"]["status"], "fail");
        }
        "provenance" => {
            assert_eq!(report["retrieval"]["status"], "pass");
            assert_eq!(report["answer_quality"]["critical"]["status"], "fail");
            let v = report["answer_quality"]["critical"]["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["case_id"] == r[0]["case_id"])
                .unwrap();
            assert!(
                v["failures"]
                    .to_string()
                    .contains("observation provenance hash mismatch")
            );
        }
        "low_missing" => {
            for message in [
                "missing answer judge rows",
                "faithfulness lower_ci",
                "response_relevancy lower_ci",
            ] {
                assert!(failures.contains(message), "{failures}");
            }
        }
        "artifact_changed" => {
            assert_eq!(
                report["answer_quality"]["artifact_binding"]["matched"],
                false
            );
            assert!(failures.contains("policy-pinned SHA-256"));
            assert_eq!(
                report["evidence_tiers"]["external_answer_judge"],
                "not_verified"
            );
        }
        "rounding" => assert!(
            report["answer_quality"]["metrics"]["faithfulness"]["ci_95"][0]
                .as_f64()
                .unwrap()
                < 0.7
        ),
        _ => panic!("unknown mode"),
    }
}

fn large_cohort() -> (Vec<Value>, BTreeMap<String, Value>) {
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
    (c, o)
}

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn cli_args(output: &std::path::Path) -> ai_platform_eval::cli::Args {
    ai_platform_eval::cli::Args {
        positional: vec![
            "gate".into(),
            root()
                .join("tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl")
                .to_str()
                .unwrap()
                .into(),
        ],
        options: BTreeMap::from([
            (
                "observations".into(),
                root()
                    .join("tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl")
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            ("output".into(), output.to_str().unwrap().into()),
            ("bootstrap-samples".into(), "500".into()),
        ]),
        repeated: BTreeMap::new(),
    }
}
#[test]
fn external_judge_requires_explicit_opt_in_rows_and_trusted_policy() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("report.json");
    let mut a = cli_args(&out);
    a.options
        .insert("judge-observations".into(), "unused.jsonl".into());
    assert!(
        ai_platform_eval::cli::rag_cli(a)
            .unwrap_err()
            .to_string()
            .contains("enable-external-judge-gate")
    );
    let mut a = cli_args(&out);
    a.options
        .insert("enable-external-judge-gate".into(), "true".into());
    assert!(
        ai_platform_eval::cli::rag_cli(a)
            .unwrap_err()
            .to_string()
            .contains("requires judge observations")
    );
    let mut a = cli_args(&out);
    a.options
        .insert("enable-external-judge-gate".into(), "true".into());
    a.options
        .insert("judge-observations".into(), "unused.jsonl".into());
    assert!(
        ai_platform_eval::cli::rag_cli(a)
            .unwrap_err()
            .to_string()
            .contains("requires a trusted resolved judge policy")
    );
    assert!(serde_json::from_value::<Vec<Value>>(json!({})).is_err());
}
#[test]
fn rag_cli_writes_provenanced_offline_report() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("report.json");
    let (r, code) = ai_platform_eval::cli::rag_cli(cli_args(&out)).unwrap();
    assert_eq!(code, 0);
    assert_eq!(r["gate"]["status"], "pass");
    assert_eq!(r["answer_quality"]["status"], "not_run");
    for source in ["expectations", "observations"] {
        assert_eq!(
            r["provenance"][source]["sha256"].as_str().unwrap().len(),
            64
        );
    }
    assert_eq!(
        r["provenance"]["provider_execution"],
        "not_performed_by_this_command"
    );
    let persisted: Value = serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap();
    assert_eq!(r, persisted);
}
fn verify_cli_snapshot(t: &std::path::Path, rows: &[Value], policy: &std::path::Path) {
    let path = t.join("judge.jsonl");
    let bytes = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, bytes.as_bytes()).unwrap();
    let mut a = cli_args(&t.join("cli.json"));
    a.options
        .insert("judge-observations".into(), path.to_str().unwrap().into());
    a.options.insert(
        "judge-policy-manifest".into(),
        policy.to_str().unwrap().into(),
    );
    a.options
        .insert("enable-external-judge-gate".into(), "true".into());
    let (r, code) = ai_platform_eval::cli::rag_cli(a).unwrap();
    assert_eq!(code, 0);
    assert_eq!(r["gate"]["status"], "pass");
    let expected = self::policy(rows)["artifact"]["sha256"].clone();
    assert_eq!(
        r["provenance"]["judge_observations"],
        json!({"source":path.to_str().unwrap(),"raw_source_sha256":sha256(bytes.as_bytes()),"artifact_binding":{"algorithm":"sha256","hash_domain":"rag-recorded-judge-rows/v1","expected_sha256":expected,"observed_sha256":expected,"matched":true}})
    );
    assert_eq!(
        r["provenance"]["judge_policy"]["manifest_sha256"],
        sha256(&std::fs::read(policy).unwrap())
    );
    assert_eq!(
        r["provenance"]["judge_policy"]["resolver"],
        "environment:RAG_JUDGE_POLICY_SHA256"
    );
}
