//! Durable evaluator construction using the public Gateway runtime boundary.
//! Construction neither dispatches a model nor starts an outbox consumer.
use super::{BootstrapError, compat::CompatServices, core::CoreServices};
use crate::{
    migration_compat::{
        eval_candidate::EvalCandidateClient,
        eval_clients::{EvalGatewayLlmClient, EvalJwtSigner},
        eval_executor::{EvalOutboxWorker, EvaluatorExecutor},
        eval_live::EvalCandidateRunner,
        eval_queue::EvalOutbox,
        eval_scoring::EvalScorer,
    },
    migration_host::{
        lifecycle::LifecycleStep, observability::ObservabilityServices, settings::HostSettings,
    },
    usage_parser::python_whitespace,
};
use ai_platform_auth::{JwtKeyConfig, JwtSigningMode, TokenCodec};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// Contains captured signing/credential material; deliberately not Debug/Serialize.
pub struct EvaluationConfiguration {
    pub enabled: bool,
    pub concurrency: usize,
    pub poll_interval: Duration,
    pub judge_enabled: bool,
    pub judge_origin: String,
    pub judge_model: String,
    pub system_tenant: String,
    pub judge_timeout: Duration,
    pub candidate_origin: String,
    pub candidate_token: Option<String>,
    pub candidate_api_key: Option<String>,
    pub jwt_mode: JwtSigningMode,
    pub jwt_secret: String,
    pub jwt_active_kid: Option<String>,
    pub jwt_keys: Vec<JwtKeyConfig>,
    pub jwt_audience: Option<String>,
    pub jwt_issuer: Option<String>,
}
impl EvaluationConfiguration {
    pub fn from_settings(
        settings: &HostSettings,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, BootstrapError> {
        let env = |name: &str, default: &str| {
            environment
                .get(name)
                .map(String::as_str)
                .unwrap_or(default)
                .to_owned()
        };
        let nonblank = |name: &str| {
            environment
                .get(name)
                .map(|v| v.trim_matches(python_whitespace))
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let gateway = env("GATEWAY_URL", "http://gateway:8080");
        // Source fallback accepts unknown truthy spellings; only these disable.
        let judge_enabled = !matches!(
            env("EVAL_LLM_ENABLED", "true")
                .trim_matches(python_whitespace)
                .to_ascii_lowercase()
                .as_str(),
            "0" | "false" | "no" | "off"
        );
        let timeout = nonblank("EVAL_LLM_TIMEOUT_READ_S")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(120.0);
        let judge_timeout = Duration::try_from_secs_f64(timeout)
            .map_err(|_| BootstrapError::Configuration("EVAL_LLM_TIMEOUT_READ_S"))?;
        let jwt = &settings.authentication.jwt;
        Ok(Self {
            // Source main initializes this worker only for enabled database.
            enabled: settings.database.enabled,
            concurrency: 2,
            poll_interval: Duration::from_secs(2),
            judge_enabled,
            judge_origin: env("EVAL_LLM_GATEWAY_URL", &gateway)
                .trim_end_matches('/')
                .to_owned(),
            judge_model: nonblank("EVAL_JUDGE_MODEL_ID").unwrap_or_else(|| "qwen3.7-plus".into()),
            system_tenant: nonblank("EVAL_SYSTEM_TENANT_ID").unwrap_or_else(|| "default".into()),
            judge_timeout,
            candidate_origin: env("AGENT_EVAL_GATEWAY_URL", &gateway)
                .trim_end_matches('/')
                .to_owned(),
            candidate_token: nonblank("AGENT_EVAL_AUTH_TOKEN")
                .or_else(|| nonblank("GATEWAY_TOKEN"))
                .or_else(|| nonblank("GATEWAY_ADMIN_JWT")),
            candidate_api_key: nonblank("AGENT_EVAL_API_KEY")
                .or_else(|| nonblank("GATEWAY_API_KEY")),
            // Use the same resolved signing authority as the actual Gateway.
            // No parallel default/dev secret is generated for evaluations.
            jwt_mode: jwt.mode,
            jwt_secret: jwt.secret.clone(),
            jwt_active_kid: jwt.active_kid.clone(),
            jwt_keys: jwt.key_configs(environment, true),
            jwt_audience: jwt.audience.clone(),
            jwt_issuer: jwt.issuer.clone(),
        })
    }
}

pub struct EvaluationServices {
    pub worker: Option<Arc<EvalOutboxWorker>>,
    pub executor: Option<Arc<EvaluatorExecutor>>,
    concurrency: usize,
}
impl EvaluationServices {
    pub fn new(
        config: EvaluationConfiguration,
        core: &CoreServices,
        compat: &CompatServices,
        observability: &ObservabilityServices,
    ) -> Result<Arc<Self>, BootstrapError> {
        if !config.enabled {
            return Ok(Arc::new(Self {
                worker: None,
                executor: None,
                concurrency: config.concurrency,
            }));
        }
        let codec = TokenCodec::from_keyring(
            config.jwt_mode,
            config.jwt_secret.as_bytes(),
            config.jwt_active_kid.as_deref(),
            &config.jwt_keys,
            config.jwt_audience.clone(),
            config.jwt_issuer.clone(),
        )
        .map_err(|_| BootstrapError::Configuration("eval_jwt"))?;
        let signer = Arc::new(EvalJwtSigner::from_signer(
            codec.signer(),
            config.jwt_audience,
            config.jwt_issuer,
        ));
        let llm = if config.judge_enabled {
            Some(Arc::new(
                EvalGatewayLlmClient::new(
                    &config.judge_origin,
                    signer.clone(),
                    config.judge_timeout,
                    true,
                    config.judge_model,
                    config.system_tenant,
                )
                .map_err(|_| BootstrapError::Configuration("eval_llm_client"))?,
            ))
        } else {
            None
        };
        let outbox = EvalOutbox {
            pool: compat.evaluations.pool.clone(),
            enabled: true,
        };
        // Delegated candidates must use the public V2 Thread/Turn/Item routes,
        // not a second in-process model/tool loop. Those routes own the existing
        // model/provider plane, runtime, admission, usage and current-user ACLs.
        let candidate_client = Arc::new(
            EvalCandidateClient::new(
                &config.candidate_origin,
                config.candidate_token,
                config.candidate_api_key,
                signer,
                true,
                outbox.clone(),
            )
            .map_err(|_| BootstrapError::Configuration("eval_candidate_client"))?,
        );
        let ragas = compat
            .eval_options
            .ragas
            .clone()
            .ok_or(BootstrapError::Construction("eval_ragas_client"))?;
        let executor = Arc::new(EvaluatorExecutor {
            service: compat.evaluations.clone(),
            traces: observability.traces.clone(),
            scorer: Arc::new(EvalScorer {
                llm,
                ragas: Some(ragas),
            }),
            outbox: outbox.clone(),
            created_by: "eval-worker".into(),
            candidate: Some(Arc::new(EvalCandidateRunner {
                client: candidate_client,
                traces: observability.traces.clone(),
            })),
        });
        let mut worker = EvalOutboxWorker::new(executor.clone(), outbox);
        worker.poll_interval = config.poll_interval;
        worker.capacity = Some(core.admission.clone());
        worker.capacity_resolver = Some(core.capacity.clone());
        Ok(Arc::new(Self {
            worker: Some(Arc::new(worker)),
            executor: Some(executor),
            concurrency: config.concurrency,
        }))
    }

    pub fn lifecycle_steps(self: &Arc<Self>) -> (Vec<LifecycleStep>, Vec<LifecycleStep>) {
        let Some(worker) = &self.worker else {
            return (vec![], vec![]);
        };
        let start_worker = worker.clone();
        let stop_worker = worker.clone();
        let concurrency = self.concurrency;
        (
            vec![LifecycleStep {
                name: "eval_outbox_worker",
                best_effort: true,
                operation: Arc::new(move || {
                    let worker = start_worker.clone();
                    Box::pin(async move {
                        worker.start(concurrency).await;
                        Ok(())
                    })
                }),
            }],
            vec![LifecycleStep {
                name: "eval_outbox_worker",
                best_effort: true,
                operation: Arc::new(move || {
                    let worker = stop_worker.clone();
                    Box::pin(async move {
                        worker.stop().await;
                        Ok(())
                    })
                }),
            }],
        )
    }
}
