//! Native equivalents of the online sampling and outbox worker fake-repository contracts.
use ai_platform_gateway::migration_compat::{
    common::CompatError,
    eval_executor::{EvaluatorRunResult, should_sample_trace_id},
    eval_online::{self, OnlineRepository, Options},
    eval_outbox_dispatch::{self, Dispatch},
};
use serde_json::{Value, json};
use std::sync::Mutex;
#[derive(Default)]
struct Repo {
    evaluators: Vec<Value>,
    pending: i64,
    active: bool,
    detail: Value,
    calls: Mutex<Vec<Value>>,
}
impl OnlineRepository for Repo {
    async fn detail(&self, _: &str, _: &str, f: &str) -> Result<Value, CompatError> {
        self.calls.lock().unwrap().push(json!({"family":f}));
        Ok(self.detail.clone())
    }
    async fn pending(&self, _: &str) -> Result<i64, CompatError> {
        Ok(self.pending)
    }
    async fn evaluators(
        &self,
        _: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Value>, i64), CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"offset":offset,"limit":limit}));
        Ok((
            self.evaluators
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect(),
            self.evaluators.len() as i64,
        ))
    }
    async fn active(&self, _: &str, _: &str, _: &str) -> Result<bool, CompatError> {
        Ok(self.active)
    }
    async fn enqueue(
        &self,
        tenant: &str,
        id: &str,
        creator: &str,
        payload: Value,
    ) -> Result<(), CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"tenant":tenant,"evaluator":id,"creator":creator,"payload":payload}));
        Ok(())
    }
}
fn evaluator() -> Value {
    json!({"evaluator_id":"e1","evaluator_type":"rule","sampling_config":{"online":{"enabled":true,"rate":1}}})
}
async fn schedule(repo: &Repo, payload: Value) -> Value {
    eval_online::schedule(repo, "tenant", &payload, Options::default())
        .await
        .unwrap()
}
#[test]
fn deterministic_sampling_and_invalid_rates() {
    for id in ["00000000-0000-0000-0000-000000000001", "non-uuid"] {
        assert_eq!(
            should_sample_trace_id(id, 0.4),
            should_sample_trace_id(id, 0.4)
        );
        assert!(should_sample_trace_id(id, 1.));
        for rate in [0., -1., f64::NAN, f64::INFINITY] {
            assert!(!should_sample_trace_id(id, rate));
        }
    }
    assert!(should_sample_trace_id(
        "00000000-0000-0000-0000-000000000001",
        0.001
    ));
}
#[test]
fn configuration_defaults_and_source_coercion() {
    let mut e = evaluator();
    for (v, expected) in [
        (Value::Null, 0.),
        (json!("bad"), 0.2),
        (json!("0.8"), 0.8),
        (json!(true), 1.),
    ] {
        e["sampling_config"]["online"]["rate"] = v;
        assert_eq!(eval_online::resolve_rate(&e, 0.2), expected);
    }
    for (v, expected) in [
        (json!("bad"), 200),
        (json!("7"), 7),
        (json!(0), 200),
        (json!(-1), 1),
        (json!(2.9), 2),
    ] {
        e["sampling_config"]["online"]["max_pending_runs"] = v;
        assert_eq!(eval_online::resolve_cap(&e, 200), expected);
    }
    assert!(!eval_online::matches_online(&e, "unknown", "succeeded"));
    e["sampling_config"]["online"]["only_failed"] = json!(true);
    assert!(!eval_online::matches_online(&e, "assistant", "succeeded"));
    assert!(eval_online::matches_online(&e, "assistant", "failed"));
    e["evaluator_type"] = json!("human");
    assert!(!eval_online::matches_online(&e, "assistant", "failed"));
}
#[tokio::test]
async fn matching_enqueue_preserves_snapshot() {
    let repo = Repo {
        evaluators: vec![evaluator()],
        ..Default::default()
    };
    assert_eq!(
        schedule(
            &repo,
            json!({"trace_id":"trace","trace_family":"rag","status":"FAILED","source_adapter":"kb"})
        )
        .await,
        json!({"scheduled":1,"matched":1,"trace_id":"trace"})
    );
    let calls = repo.calls.lock().unwrap();
    let call = calls.last().unwrap();
    assert_eq!(call["creator"], "eval-online-sampler");
    assert_eq!(
        call["payload"]["target_snapshot"],
        json!({"trace_id":"trace","trace_family":"rag","source":"online_sampling","status":"failed","source_adapter":"kb"})
    );
}
#[tokio::test]
async fn active_and_per_evaluator_cap_skip() {
    for repo in [
        Repo {
            evaluators: vec![evaluator()],
            active: true,
            ..Default::default()
        },
        Repo {
            evaluators: vec![{
                let mut e = evaluator();
                e["sampling_config"]["online"]["max_pending_runs"] = json!("1");
                e
            }],
            pending: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            schedule(&repo, json!({"trace_id":"trace"})).await["scheduled"],
            0
        );
    }
}
#[tokio::test]
async fn global_cap_and_internal_origins_skip() {
    let repo = Repo {
        pending: 200,
        ..Default::default()
    };
    assert_eq!(
        schedule(&repo, json!({"trace_id":"trace"})).await["reason"],
        "online_eval_queue_full"
    );
    for payload in [
        json!({"trace_id":"trace","eval_origin":" candidate "}),
        json!({"trace_id":"trace"}),
    ] {
        let repo = Repo {
            detail: json!({"trace":{"metadata":{"eval_origin":"judge"}}}),
            ..Default::default()
        };
        assert_eq!(
            schedule(&repo, payload).await["reason"],
            "internal_eval_trace"
        );
        assert_eq!(repo.calls.lock().unwrap().len(), 1);
    }
    assert_eq!(
        schedule(&Repo::default(), json!({})).await["reason"],
        "missing_trace_id"
    );
}
#[tokio::test]
async fn pagination_schedules_all_101() {
    let repo = Repo {
        evaluators: (0..101)
            .map(|i| {
                let mut e = evaluator();
                e["evaluator_id"] = json!(format!("e{i}"));
                e
            })
            .collect(),
        ..Default::default()
    };
    assert_eq!(
        schedule(&repo, json!({"trace_id":"trace"})).await["scheduled"],
        101
    );
    assert!(
        repo.calls
            .lock()
            .unwrap()
            .contains(&json!({"offset":100,"limit":100}))
    );
}
#[derive(Default)]
struct Worker {
    fail: bool,
    lost: bool,
    failed_result: bool,
    calls: Mutex<Vec<Value>>,
}
impl Dispatch for Worker {
    async fn run(&self, job: &Value) -> Result<EvaluatorRunResult, CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"run":job["payload"],"tenant":job["tenant_id"]}));
        if self.lost {
            return Err(CompatError::new(409, "eval_outbox_claim_lost"));
        }
        if self.fail {
            return Err(CompatError::new(503, "failure"));
        }
        Ok(EvaluatorRunResult {
            run_id: Some("run".into()),
            status: if self.failed_result {
                "failed"
            } else {
                "succeeded"
            }
            .into(),
            score_summary: json!({}),
            metrics: json!({}),
            error_message: None,
            scores_written: 1,
        })
    }
    async fn sample(&self, job: &Value) -> Result<(), CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"sample":job["payload"]}));
        Ok(())
    }
    async fn succeeded(&self, job: &Value) -> Result<(), CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"succeeded":job["job_id"]}));
        Ok(())
    }
    async fn update(
        &self,
        _: &Value,
        run: &str,
        status: &str,
        error: &str,
        terminal: bool,
    ) -> Result<(), CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"update":run,"status":status,"error":error,"terminal":terminal}));
        Ok(())
    }
    async fn failed(
        &self,
        job: &Value,
        error: &str,
        delay: i32,
        max: i32,
    ) -> Result<(), CompatError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!({"failed":job["job_id"],"error":error,"delay":delay,"max":max}));
        Ok(())
    }
}
fn job(kind: &str, attempts: i32) -> Value {
    json!({"job_id":"job-1","tenant_id":"tenant","job_type":kind,"attempts":attempts,"payload":{"run_id":"run","trace_id":"trace"}})
}
#[tokio::test]
async fn evaluator_and_trace_dispatch_acknowledge() {
    for kind in ["eval.evaluator.run", "trace.ingested", "unknown"] {
        let worker = Worker::default();
        eval_outbox_dispatch::execute(&worker, &job(kind, 1), 5)
            .await
            .unwrap();
        assert_eq!(
            worker.calls.lock().unwrap().last().unwrap(),
            &json!({"succeeded":"job-1"})
        );
    }
}
#[tokio::test]
async fn exceptions_and_failed_results_retry_then_exhaust() {
    for attempts in [2, 5] {
        for failed_result in [false, true] {
            let worker = Worker {
                fail: !failed_result,
                failed_result,
                ..Default::default()
            };
            eval_outbox_dispatch::execute(&worker, &job("eval.evaluator.run", attempts), 5)
                .await
                .unwrap();
            let calls = worker.calls.lock().unwrap();
            assert_eq!(
                calls[1],
                json!({"update":"run","status":if attempts==5{"failed"}else{"queued"},"error":"eval_job_failed:RuntimeError","terminal":attempts==5})
            );
            assert_eq!(
                calls[2],
                json!({"failed":"job-1","error":"eval_job_failed:RuntimeError","delay":5*attempts,"max":5})
            );
        }
    }
}
#[tokio::test]
async fn lost_claim_never_acknowledges_or_retries() {
    let worker = Worker {
        lost: true,
        ..Default::default()
    };
    assert_eq!(
        eval_outbox_dispatch::execute(&worker, &job("eval.evaluator.run", 2), 5)
            .await
            .unwrap_err()
            .status,
        409
    );
    assert_eq!(worker.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn kb_metric_failure_kind_controls_retry_without_failing_semantic_review() {
    use ai_platform_gateway::migration_compat::{
        eval_executor::persisted_run_error,
        eval_scoring::{kb_metric_score, kb_ragas_sample},
    };
    let evaluator =
        json!({"evaluator_type":"ragas","filter_config":{"metrics":["context_relevancy"]}});
    let target = json!({"trace_id":"trace","trace_family":"rag","input_preview":"question","spans":[{"span_kind":"retriever","attributes":{"retrieval":{"documents":[{"content_eval":"chunk"}]}}}]});
    let sample = kb_ragas_sample(&target, None).unwrap();
    for kind in ["infrastructure", "semantic_review"] {
        let score = kb_metric_score(
            &evaluator,
            &target,
            &json!({"metric":"context_relevancy","score":0.0,"label":"review","failure_kind":kind}),
            &sample,
        );
        assert_eq!(score["metadata"]["failure_kind"], kind);
        assert_eq!(score["label"], "review");
        let error = persisted_run_error(
            "ragas",
            score["metadata"]["failure_kind"] == "infrastructure",
            0,
        );
        assert_eq!(error.is_some(), kind == "infrastructure");
        let worker = Worker {
            failed_result: error.is_some(),
            ..Default::default()
        };
        eval_outbox_dispatch::execute(&worker, &job("eval.evaluator.run", 1), 5)
            .await
            .unwrap();
        let calls = worker.calls.lock().unwrap();
        assert_eq!(
            calls.last().unwrap().get("failed").is_some(),
            kind == "infrastructure"
        );
    }
}
#[test]
fn claim_owner_lease_and_typed_ingestion_parameters() {
    use ai_platform_gateway::{
        migration_compat::eval_queue::{CLAIM_OUTBOX_SQL, claim_plan},
        migration_trace::sql::{ENQUEUE_TRACE, enqueue_trace_arguments},
    };
    let plan = claim_plan("worker-1", 3, 7, 30);
    assert_eq!(
        (
            plan.limit,
            plan.max_attempts,
            plan.owner,
            plan.lease_seconds
        ),
        (3, 7, "worker-1", 30)
    );
    assert!(CLAIM_OUTBOX_SQL.contains("FOR UPDATE SKIP LOCKED"));
    assert!(CLAIM_OUTBOX_SQL.contains("owner_id=$3"));
    assert!(CLAIM_OUTBOX_SQL.contains("attempts<$2"));
    assert!(ENQUEUE_TRACE.contains("$1::varchar"));
    assert!(ENQUEUE_TRACE.contains("$3::text"));
    assert!(ENQUEUE_TRACE.contains("WHERE NOT EXISTS"));
    assert!(ENQUEUE_TRACE.contains("status IN ('queued', 'running')"));
    let args = enqueue_trace_arguments("tenant", json!({"trace_id":"trace"}), "trace", false);
    assert_eq!(args.0, "tenant");
    assert_eq!(args.2, "trace");
    assert!(!args.3);
}
