//! Strict adapters for write capabilities whose side effects are owned by the
//! Gateway.  This module deliberately contains no provider client, agent loop,
//! or long-lived credential.  The worker sends one, scope-bound request to a
//! Gateway broker and returns only a structured receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ai_platform_capability_contract::canonical_json_hash;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RuntimeConnectorBinding;
use crate::write_capabilities::WriteCapabilityContext;

const MAX_CONTEXT_FIELD: usize = 255;
const MAX_TOOL_CALL_ID: usize = 160;
const MAX_RECEIPT_ID: usize = 160;
const MAX_CONFLUENCE_TITLE: usize = 4_096;
const MAX_CONFLUENCE_CONTENT: usize = 100_000;
const MAX_CONFLUENCE_TEXT: usize = 20_000;
const MAX_IMAGE_PROMPT: usize = 8_000;
const MAX_IMAGE_NEGATIVE_PROMPT: usize = 4_000;
const MAX_QUIZ_TITLE: usize = 300;
const MAX_QUIZ_DESCRIPTION: usize = 2_000;
const MAX_QUIZ_QUESTIONS: usize = 50;
const MAX_QUIZ_QUESTION_TEXT: usize = 4_000;
const MAX_QUIZ_OPTION_TEXT: usize = 2_000;
const MAX_QUIZ_EXPLANATION: usize = 4_000;

/// The verified facts copied from the active Runtime lease.  The worker must
/// receive this value from the Runtime; it must never build it from arguments.
#[derive(Clone, Debug)]
pub struct ExternalWriteContext {
    pub write: WriteCapabilityContext,
    pub tool_call_id: String,
    pub arguments_hash: String,
    pub connector_binding: Option<RuntimeConnectorBinding>,
}

impl ExternalWriteContext {
    fn validate(&self, capability_id: &str, arguments: &Value) -> Result<(), ExternalWriteError> {
        if !matches!(
            capability_id,
            "confluence_write" | "generate_image" | "generate_quiz"
        ) || self.write.capability_revision == 0
            || !valid_text(&self.write.tenant_id, MAX_CONTEXT_FIELD)
            || !valid_text(&self.write.user_id, MAX_CONTEXT_FIELD)
            || !valid_text(&self.write.session_id, MAX_CONTEXT_FIELD)
            || !valid_text(&self.write.execution_id, MAX_CONTEXT_FIELD)
            || !valid_text(&self.write.run_id, MAX_CONTEXT_FIELD)
            || !valid_text(&self.tool_call_id, MAX_TOOL_CALL_ID)
            || !self.arguments_hash.starts_with("sha256:")
        {
            return Err(ExternalWriteError::Context);
        }
        let calculated =
            canonical_json_hash(arguments).map_err(|_| ExternalWriteError::Arguments)?;
        if calculated != self.arguments_hash {
            return Err(ExternalWriteError::ArgumentsHashMismatch);
        }

        match capability_id {
            "confluence_write" => {
                let binding = self
                    .connector_binding
                    .as_ref()
                    .ok_or(ExternalWriteError::ConnectorBinding)?;
                binding
                    .validate("confluence_write")
                    .map_err(|_| ExternalWriteError::ConnectorBinding)?;
                if binding.provider != "confluence" {
                    return Err(ExternalWriteError::ConnectorBinding);
                }
            }
            "generate_image" | "generate_quiz" if self.connector_binding.is_some() => {
                // These capabilities use a Gateway model/data broker, not a
                // connector credential.  Rejecting a binding prevents a
                // caller from smuggling a connector secret path into them.
                return Err(ExternalWriteError::ConnectorBinding);
            }
            _ => {}
        }
        Ok(())
    }
}

/// Request sent to the Gateway private broker.  There is intentionally no
/// credential field: the broker resolves a short-lived handle server-side.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayWriteRequest {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub capability_id: String,
    pub arguments_hash: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_binding: Option<RuntimeConnectorBinding>,
}

/// A response from the broker is already reduced to non-secret metadata.
/// Provider responses, tokens, prompts and raw binary data are not part of the
/// Worker/Gateway write contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayWriteResponse {
    pub receipt_id: String,
    #[serde(default)]
    pub external_id: String,
    #[serde(default)]
    pub external_url: String,
    #[serde(default)]
    pub artifacts: Vec<ArtifactMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetadata {
    pub artifact_id: String,
    pub kind: String,
    pub mime_type: String,
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteReceipt {
    pub receipt_id: String,
    pub capability_id: String,
    pub external_id: Option<String>,
    pub external_url: Option<String>,
    pub artifacts: Vec<ArtifactMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GatewayBrokerError {
    #[error("broker_http_status_{0}")]
    HttpStatus(u16),
    #[error("broker_timeout")]
    Timeout,
    #[error("broker_unavailable")]
    Unavailable,
    #[error("broker_malformed_response")]
    MalformedResponse,
}

impl GatewayBrokerError {
    fn is_side_effect_unknown(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Unavailable | Self::MalformedResponse
        ) || matches!(self, Self::HttpStatus(status) if *status >= 500)
    }
}

/// A single-attempt broker. Implementations must not retry a write: a timeout
/// or 5xx is returned as `SideEffectUnknown` so the Runtime can reconcile it.
#[async_trait]
pub trait GatewayWriteBroker: Send + Sync {
    async fn execute_once(
        &self,
        request: GatewayWriteRequest,
    ) -> Result<GatewayWriteResponse, GatewayBrokerError>;
}

/// Startup-built broker router. Capability selection is driven only by the
/// signed capability ID; prompt text and provider/model names are never used
/// for dispatch. Each registered broker still performs exactly one request.
#[derive(Clone, Default)]
pub struct GatewayWriteBrokerRouter {
    brokers: BTreeMap<String, Arc<dyn GatewayWriteBroker>>,
}

impl GatewayWriteBrokerRouter {
    pub fn register(
        &mut self,
        capability_id: &str,
        broker: Arc<dyn GatewayWriteBroker>,
    ) -> Result<(), ExternalWriteError> {
        if !matches!(
            capability_id,
            "confluence_write" | "generate_image" | "generate_quiz"
        ) || self
            .brokers
            .insert(capability_id.to_string(), broker)
            .is_some()
        {
            return Err(ExternalWriteError::Context);
        }
        Ok(())
    }
}

#[async_trait]
impl GatewayWriteBroker for GatewayWriteBrokerRouter {
    async fn execute_once(
        &self,
        request: GatewayWriteRequest,
    ) -> Result<GatewayWriteResponse, GatewayBrokerError> {
        let broker = self
            .brokers
            .get(&request.capability_id)
            .ok_or(GatewayBrokerError::MalformedResponse)?;
        broker.execute_once(request).await
    }
}

#[derive(Clone)]
pub struct ExternalWriteExecutor {
    broker: Arc<dyn GatewayWriteBroker>,
}

impl ExternalWriteExecutor {
    pub fn new(broker: Arc<dyn GatewayWriteBroker>) -> Self {
        Self { broker }
    }

    pub async fn execute(
        &self,
        capability_id: &str,
        context: &ExternalWriteContext,
        arguments: Value,
    ) -> Result<WriteReceipt, ExternalWriteError> {
        context.validate(capability_id, &arguments)?;
        // Keep the original JSON object byte-for-byte equivalent after
        // canonicalization.  Re-serializing a deserialized struct would add
        // default fields and break the lease's arguments_hash binding.
        validate_arguments(capability_id, &arguments)?;
        let request = GatewayWriteRequest {
            tenant_id: context.write.tenant_id.clone(),
            user_id: context.write.user_id.clone(),
            session_id: context.write.session_id.clone(),
            execution_id: context.write.execution_id.clone(),
            run_id: context.write.run_id.clone(),
            tool_call_id: context.tool_call_id.clone(),
            capability_id: capability_id.to_string(),
            arguments_hash: context.arguments_hash.clone(),
            arguments,
            connector_binding: context.connector_binding.clone(),
        };
        let response = self
            .broker
            .execute_once(request)
            .await
            .map_err(ExternalWriteError::from)?;
        // The broker may have completed the side effect before returning an
        // invalid response.  Never turn that uncertainty into a retryable
        // validation failure.
        validate_response(capability_id, response)
            .map_err(|_| ExternalWriteError::SideEffectUnknown)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExternalWriteError {
    #[error("external_write_context_invalid")]
    Context,
    #[error("external_write_arguments_invalid")]
    Arguments,
    #[error("external_write_arguments_hash_mismatch")]
    ArgumentsHashMismatch,
    #[error("external_write_connector_binding_invalid")]
    ConnectorBinding,
    #[error("external_write_failed")]
    Failed,
    #[error("external_write_side_effect_unknown")]
    SideEffectUnknown,
    #[error("external_write_response_invalid")]
    Response,
}

impl From<GatewayBrokerError> for ExternalWriteError {
    fn from(error: GatewayBrokerError) -> Self {
        if error.is_side_effect_unknown() {
            Self::SideEffectUnknown
        } else {
            // All 4xx responses are definitive failures. No write is retried.
            Self::Failed
        }
    }
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.chars().count() <= maximum && !value.chars().any(char::is_control)
}

fn bounded(value: &str, maximum: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty())
        && value.chars().count() <= maximum
        && !value.chars().any(char::is_control)
}

fn validate_arguments(capability_id: &str, arguments: &Value) -> Result<(), ExternalWriteError> {
    match capability_id {
        "confluence_write" => {
            let action: ConfluenceAction = serde_json::from_value(arguments.clone())
                .map_err(|_| ExternalWriteError::Arguments)?;
            action.validate()?;
            Ok(())
        }
        "generate_image" => {
            let image: ImageArguments = serde_json::from_value(arguments.clone())
                .map_err(|_| ExternalWriteError::Arguments)?;
            image.validate()?;
            Ok(())
        }
        "generate_quiz" => {
            let quiz: QuizArguments = serde_json::from_value(arguments.clone())
                .map_err(|_| ExternalWriteError::Arguments)?;
            quiz.validate()?;
            Ok(())
        }
        _ => Err(ExternalWriteError::Arguments),
    }
}

fn validate_response(
    capability_id: &str,
    response: GatewayWriteResponse,
) -> Result<WriteReceipt, ExternalWriteError> {
    if !valid_text(&response.receipt_id, MAX_RECEIPT_ID)
        || !response.external_id.is_empty()
            && !bounded(&response.external_id, MAX_RECEIPT_ID, false)
        || !response.external_url.is_empty()
            && (!bounded(&response.external_url, 2_048, false)
                || !response.external_url.starts_with("https://"))
        || response.artifacts.len() > 16
        || response.artifacts.iter().any(|artifact| {
            !valid_text(&artifact.artifact_id, MAX_RECEIPT_ID)
                || !valid_text(&artifact.kind, 128)
                || !valid_text(&artifact.mime_type, 255)
                || !valid_text(&artifact.filename, 512)
                || artifact.filename.contains('/')
                || artifact.filename.contains('\\')
                || artifact.filename.contains("..")
        })
    {
        return Err(ExternalWriteError::Response);
    }
    Ok(WriteReceipt {
        receipt_id: response.receipt_id,
        capability_id: capability_id.to_string(),
        external_id: (!response.external_id.is_empty()).then_some(response.external_id),
        external_url: (!response.external_url.is_empty()).then_some(response.external_url),
        artifacts: response.artifacts,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ConfluenceAction {
    CreatePage {
        #[serde(default)]
        space_key: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        content: String,
        #[serde(default)]
        parent_id: String,
    },
    UpdatePage {
        #[serde(default)]
        page_id: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        content: String,
    },
    FindReplace {
        #[serde(default)]
        page_id: String,
        #[serde(default)]
        find: String,
        #[serde(default)]
        replace: String,
        #[serde(default)]
        raw_html: bool,
    },
    MovePage {
        #[serde(default)]
        page_id: String,
        #[serde(default)]
        target_parent_id: String,
    },
    Comment {
        #[serde(default)]
        page_id: String,
        #[serde(default)]
        body: String,
    },
    DeletePage {
        #[serde(default)]
        page_id: String,
    },
}

impl ConfluenceAction {
    fn validate(&self) -> Result<(), ExternalWriteError> {
        let check_id = |value: &str| {
            if valid_text(value, MAX_CONTEXT_FIELD) {
                Ok(())
            } else {
                Err(ExternalWriteError::Arguments)
            }
        };
        match self {
            Self::CreatePage {
                space_key,
                title,
                content,
                parent_id,
            } => {
                check_id(space_key)?;
                if !bounded(title, MAX_CONFLUENCE_TITLE, false)
                    || !bounded(content, MAX_CONFLUENCE_CONTENT, true)
                {
                    return Err(ExternalWriteError::Arguments);
                }
                if !parent_id.is_empty() {
                    check_id(parent_id)?;
                }
            }
            Self::UpdatePage {
                page_id,
                title,
                content,
            } => {
                check_id(page_id)?;
                if !bounded(content, MAX_CONFLUENCE_CONTENT, false)
                    || !bounded(title, MAX_CONFLUENCE_TITLE, true)
                {
                    return Err(ExternalWriteError::Arguments);
                }
            }
            Self::FindReplace {
                page_id,
                find,
                replace,
                ..
            } => {
                check_id(page_id)?;
                if !bounded(find, MAX_CONFLUENCE_TEXT, false)
                    || !bounded(replace, MAX_CONFLUENCE_TEXT, true)
                {
                    return Err(ExternalWriteError::Arguments);
                }
            }
            Self::MovePage {
                page_id,
                target_parent_id,
            } => {
                check_id(page_id)?;
                check_id(target_parent_id)?;
            }
            Self::Comment { page_id, body } => {
                check_id(page_id)?;
                if !bounded(body, MAX_CONFLUENCE_TEXT, false) {
                    return Err(ExternalWriteError::Arguments);
                }
            }
            Self::DeletePage { page_id } => check_id(page_id)?,
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImageArguments {
    prompt: String,
    #[serde(default)]
    negative_prompt: String,
    #[serde(default = "default_image_size")]
    size: String,
    #[serde(default = "default_image_style")]
    style: String,
    #[serde(default = "default_image_count")]
    n: u8,
}

fn default_image_size() -> String {
    "1536*1536".to_string()
}
fn default_image_style() -> String {
    "<auto>".to_string()
}
fn default_image_count() -> u8 {
    1
}

impl ImageArguments {
    fn validate(&self) -> Result<(), ExternalWriteError> {
        if !bounded(&self.prompt, MAX_IMAGE_PROMPT, false)
            || !bounded(&self.negative_prompt, MAX_IMAGE_NEGATIVE_PROMPT, true)
            || !matches!(
                self.size.as_str(),
                "1536*1536" | "1024*1024" | "720*1280" | "1280*720"
            )
            || !matches!(
                self.style.as_str(),
                "<auto>"
                    | "<photography>"
                    | "<portrait>"
                    | "<3d cartoon>"
                    | "<anime>"
                    | "<oil painting>"
                    | "<watercolor>"
                    | "<sketch>"
                    | "<flat illustration>"
            )
            || !(1..=4).contains(&self.n)
        {
            return Err(ExternalWriteError::Arguments);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuizArguments {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_difficulty")]
    difficulty: String,
    questions: Vec<QuizQuestion>,
}

fn default_difficulty() -> String {
    "medium".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuizQuestion {
    question_num: u32,
    question_type: String,
    question_text: String,
    #[serde(default)]
    options: Vec<QuizOption>,
    correct_answer: Vec<String>,
    #[serde(default)]
    explanation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuizOption {
    label: String,
    text: String,
}

impl QuizArguments {
    fn validate(&self) -> Result<(), ExternalWriteError> {
        if !bounded(&self.title, MAX_QUIZ_TITLE, false)
            || !bounded(&self.description, MAX_QUIZ_DESCRIPTION, true)
            || !matches!(self.difficulty.as_str(), "easy" | "medium" | "hard")
            || self.questions.is_empty()
            || self.questions.len() > MAX_QUIZ_QUESTIONS
        {
            return Err(ExternalWriteError::Arguments);
        }
        for (index, question) in self.questions.iter().enumerate() {
            if question.question_num != (index + 1) as u32
                || !bounded(&question.question_text, MAX_QUIZ_QUESTION_TEXT, false)
                || !bounded(&question.explanation, MAX_QUIZ_EXPLANATION, true)
            {
                return Err(ExternalWriteError::Arguments);
            }
            let mut labels = BTreeSet::new();
            for option in &question.options {
                if !matches!(option.label.as_str(), "A" | "B" | "C" | "D")
                    || !bounded(&option.text, MAX_QUIZ_OPTION_TEXT, false)
                    || !labels.insert(option.label.as_str())
                    || option.text.trim().eq_ignore_ascii_case(&option.label)
                        && option.text.trim().chars().count() <= 2
                {
                    return Err(ExternalWriteError::Arguments);
                }
            }
            match question.question_type.as_str() {
                "mc_single" => {
                    if question.options.len() != 4
                        || question.correct_answer.len() != 1
                        || !question
                            .correct_answer
                            .iter()
                            .all(|answer| labels.contains(answer.as_str()))
                    {
                        return Err(ExternalWriteError::Arguments);
                    }
                }
                "mc_multi" => {
                    if question.options.len() != 4
                        || !(2..=3).contains(&question.correct_answer.len())
                        || question
                            .correct_answer
                            .iter()
                            .collect::<BTreeSet<_>>()
                            .len()
                            != question.correct_answer.len()
                        || !question
                            .correct_answer
                            .iter()
                            .all(|answer| labels.contains(answer.as_str()))
                    {
                        return Err(ExternalWriteError::Arguments);
                    }
                }
                "true_false" => {
                    if !(question.options.is_empty()
                        || is_true_false_option_pair(&question.options))
                        || question.correct_answer.len() != 1
                        || !matches!(question.correct_answer[0].as_str(), "true" | "false")
                    {
                        return Err(ExternalWriteError::Arguments);
                    }
                }
                _ => return Err(ExternalWriteError::Arguments),
            }
        }
        Ok(())
    }
}

fn is_true_false_option_pair(options: &[QuizOption]) -> bool {
    if options.len() != 2 || options[0].label != "A" || options[1].label != "B" {
        return false;
    }
    let truthy = options[0].text.trim().to_ascii_lowercase();
    let falsy = options[1].text.trim().to_ascii_lowercase();
    matches!(truthy.as_str(), "true" | "correct" | "正确" | "对" | "是")
        && matches!(falsy.as_str(), "false" | "incorrect" | "错误" | "错" | "否")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBroker {
        calls: AtomicUsize,
        result: Result<GatewayWriteResponse, GatewayBrokerError>,
        seen_arguments: Mutex<Option<Value>>,
    }

    impl FakeBroker {
        fn new(result: Result<GatewayWriteResponse, GatewayBrokerError>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                result,
                seen_arguments: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl GatewayWriteBroker for FakeBroker {
        async fn execute_once(
            &self,
            request: GatewayWriteRequest,
        ) -> Result<GatewayWriteResponse, GatewayBrokerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.seen_arguments.lock().unwrap() = Some(request.arguments.clone());
            let serialized = serde_json::to_string(&request).unwrap();
            assert!(!serialized.contains("secret"));
            self.result.clone()
        }
    }

    fn context(capability_id: &str, arguments: &Value) -> ExternalWriteContext {
        ExternalWriteContext {
            write: WriteCapabilityContext {
                tenant_id: "tenant-a".into(),
                user_id: "user-a".into(),
                session_id: "session-a".into(),
                execution_id: "execution-a".into(),
                run_id: "run-a".into(),
                capability_revision: 1,
                bound_dataset_ids: BTreeSet::new(),
                memory_policy: None,
            },
            tool_call_id: "call-a".into(),
            arguments_hash: canonical_json_hash(arguments).unwrap(),
            connector_binding: (capability_id == "confluence_write").then(|| {
                RuntimeConnectorBinding {
                    binding_type: "catalog".into(),
                    provider: "confluence".into(),
                    tool_name: "confluence_write".into(),
                    principal_type: None,
                    grant_id: None,
                    connection_id: None,
                    schema_hash: None,
                    risk_level: None,
                    channel: "hosted".into(),
                }
            }),
        }
    }

    fn response() -> GatewayWriteResponse {
        GatewayWriteResponse {
            receipt_id: "receipt-a".into(),
            external_id: String::new(),
            external_url: String::new(),
            artifacts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn validates_scope_and_args_before_broker_call() {
        let args = serde_json::json!({"prompt":"a cat"});
        let broker = FakeBroker::new(Ok(response()));
        let executor = ExternalWriteExecutor::new(broker.clone());
        let mut ctx = context("generate_image", &args);
        ctx.write.tenant_id.clear();
        assert_eq!(
            executor.execute("generate_image", &ctx, args.clone()).await,
            Err(ExternalWriteError::Context)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 0);
        let mut ctx = context("generate_image", &args);
        ctx.arguments_hash = "sha256:wrong".into();
        assert_eq!(
            executor.execute("generate_image", &ctx, args).await,
            Err(ExternalWriteError::ArgumentsHashMismatch)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn four_x_is_definitive_failure_and_unknown_is_not_retried() {
        let args = serde_json::json!({"prompt":"a cat"});
        let broker = FakeBroker::new(Err(GatewayBrokerError::HttpStatus(422)));
        let executor = ExternalWriteExecutor::new(broker.clone());
        assert_eq!(
            executor
                .execute(
                    "generate_image",
                    &context("generate_image", &args),
                    args.clone()
                )
                .await,
            Err(ExternalWriteError::Failed)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 1);
        let broker = FakeBroker::new(Err(GatewayBrokerError::Timeout));
        let executor = ExternalWriteExecutor::new(broker.clone());
        assert_eq!(
            executor
                .execute(
                    "generate_image",
                    &context("generate_image", &args),
                    args.clone()
                )
                .await,
            Err(ExternalWriteError::SideEffectUnknown)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 1);

        let mut malformed = response();
        malformed.receipt_id.clear();
        let broker = FakeBroker::new(Ok(malformed));
        let executor = ExternalWriteExecutor::new(broker.clone());
        assert_eq!(
            executor
                .execute(
                    "generate_image",
                    &context("generate_image", &args),
                    args.clone()
                )
                .await,
            Err(ExternalWriteError::SideEffectUnknown)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn confluence_requires_verified_binding_and_rejects_secret_or_url_fields() {
        let args = serde_json::json!({"action":"delete_page","page_id":"123"});
        let broker = FakeBroker::new(Ok(response()));
        let executor = ExternalWriteExecutor::new(broker.clone());
        let mut ctx = context("confluence_write", &args);
        ctx.connector_binding = None;
        assert_eq!(
            executor
                .execute("confluence_write", &ctx, args.clone())
                .await,
            Err(ExternalWriteError::ConnectorBinding)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 0);

        let args_with_url = serde_json::json!({"prompt":"a cat","url":"https://evil.invalid"});
        let ctx = context("generate_image", &args_with_url);
        assert_eq!(
            executor
                .execute("generate_image", &ctx, args_with_url)
                .await,
            Err(ExternalWriteError::Arguments)
        );
        assert_eq!(broker.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn broker_receives_original_arguments_when_defaults_are_omitted() {
        let image_args = serde_json::json!({"prompt":"a cat"});
        let image_broker = FakeBroker::new(Ok(response()));
        let image_executor = ExternalWriteExecutor::new(image_broker.clone());
        assert!(
            image_executor
                .execute(
                    "generate_image",
                    &context("generate_image", &image_args),
                    image_args.clone()
                )
                .await
                .is_ok()
        );
        assert_eq!(
            image_broker.seen_arguments.lock().unwrap().as_ref(),
            Some(&image_args)
        );

        let page_args = serde_json::json!({
            "action":"create_page",
            "space_key":"ENG",
            "title":"Release notes"
        });
        let page_broker = FakeBroker::new(Ok(response()));
        let page_executor = ExternalWriteExecutor::new(page_broker.clone());
        assert!(
            page_executor
                .execute(
                    "confluence_write",
                    &context("confluence_write", &page_args),
                    page_args.clone()
                )
                .await
                .is_ok()
        );
        assert_eq!(
            page_broker.seen_arguments.lock().unwrap().as_ref(),
            Some(&page_args)
        );
    }

    #[test]
    fn strict_image_quiz_and_confluence_shapes() {
        let image = serde_json::json!({"prompt":"cat","n":1});
        assert!(validate_arguments("generate_image", &image).is_ok());
        assert!(
            validate_arguments("generate_image", &serde_json::json!({"prompt":"cat","n":5}))
                .is_err()
        );
        let quiz = serde_json::json!({
            "title":"Basics",
            "questions":[{
                "question_num":1,"question_type":"mc_single","question_text":"Two plus two?",
                "options":[{"label":"A","text":"3"},{"label":"B","text":"4"},{"label":"C","text":"5"},{"label":"D","text":"6"}],
                "correct_answer":["B"]
            }]
        });
        assert!(validate_arguments("generate_quiz", &quiz).is_ok());
        let true_false = serde_json::json!({
            "title":"Basics",
            "questions":[{
                "question_num":1,"question_type":"true_false","question_text":"Agents use tools.",
                "options":[{"label":"A","text":"正确"},{"label":"B","text":"错误"}],
                "correct_answer":["true"]
            }]
        });
        assert!(validate_arguments("generate_quiz", &true_false).is_ok());
        assert!(
            validate_arguments(
                "generate_quiz",
                &serde_json::json!({"title":"x","questions":[],"extra":true})
            )
            .is_err()
        );
        assert!(
            validate_arguments(
                "confluence_write",
                &serde_json::json!({"action":"delete_page","page_id":"1","secret":"x"})
            )
            .is_err()
        );
    }
}
