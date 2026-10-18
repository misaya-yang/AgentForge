//! Lease-independent dispatch; the production adapter retains all claim fencing.
use super::{
    common::{CompatError, text},
    eval_executor::EvaluatorRunResult,
};
use serde_json::Value;
pub trait Dispatch: Sync {
    fn run(
        &self,
        job: &Value,
    ) -> impl std::future::Future<Output = Result<EvaluatorRunResult, CompatError>> + Send;
    fn sample(
        &self,
        job: &Value,
    ) -> impl std::future::Future<Output = Result<(), CompatError>> + Send;
    fn succeeded(
        &self,
        job: &Value,
    ) -> impl std::future::Future<Output = Result<(), CompatError>> + Send;
    fn update(
        &self,
        job: &Value,
        run: &str,
        status: &str,
        error: &str,
        terminal: bool,
    ) -> impl std::future::Future<Output = Result<(), CompatError>> + Send;
    fn failed(
        &self,
        job: &Value,
        error: &str,
        delay: i32,
        max: i32,
    ) -> impl std::future::Future<Output = Result<(), CompatError>> + Send;
}
pub async fn execute(adapter: &impl Dispatch, job: &Value, max: i32) -> Result<(), CompatError> {
    let result = async {
        match text(job, "job_type") {
            "eval.evaluator.run" => {
                let result = adapter.run(job).await?;
                if result.status == "failed" {
                    return Err(CompatError::new(
                        503,
                        result
                            .error_message
                            .unwrap_or_else(|| "evaluator run failed".into()),
                    ));
                }
            }
            "trace.ingested" => adapter.sample(job).await?,
            _ => {}
        }
        adapter.succeeded(job).await
    }
    .await;
    if let Err(error) = result {
        if error.status == 409
            && error
                .detail
                .as_str()
                .is_some_and(|v| matches!(v, "eval_outbox_claim_lost" | "eval_outbox_lease_lost"))
        {
            return Err(error);
        }
        let attempts = job["attempts"].as_i64().filter(|n| *n != 0).unwrap_or(1) as i32;
        let terminal = attempts >= max;
        let run = text(&job["payload"], "run_id");
        let code = "eval_job_failed:RuntimeError";
        if text(job, "job_type") == "eval.evaluator.run" && !run.is_empty() {
            adapter
                .update(
                    job,
                    run,
                    if terminal { "failed" } else { "queued" },
                    code,
                    terminal,
                )
                .await?;
        }
        adapter
            .failed(job, code, (5 * attempts).min(300), max)
            .await?;
    }
    Ok(())
}
