//! Persistence adapter for the `generate_quiz` capability.
//!
//! The Runtime supplies the already verified execution scope.  This module
//! only validates the catalog-shaped arguments and persists the quiz; it does
//! not call a model and does not contain an Agent loop.  The execution row is
//! also the idempotency receipt.  A transaction-scoped advisory lock makes a
//! retry of the same execution observe the first committed quiz instead of
//! inserting another one.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use ai_platform_capability_contract::canonical_json_hash;

use crate::write_capabilities::WriteCapabilityContext;

pub const GENERATE_QUIZ_SCHEMA_HASH: &str =
    "sha256:9b4ece973284e980c86d013ca5a3e62f3c65765f57f96846374a95a3015a3666";
const MAX_TITLE_CHARS: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 2_000;
const MAX_QUESTION_TEXT_CHARS: usize = 4_000;
const MAX_OPTION_TEXT_CHARS: usize = 2_000;
const MAX_EXPLANATION_CHARS: usize = 4_000;
const MAX_QUESTIONS: usize = 50;

/// The success payload deliberately contains no question, option, or answer
/// data.  The UI reads the quiz artifact using its id after the write has
/// completed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuizPersistenceResult {
    pub quiz_id: String,
    pub title: String,
    pub question_count: usize,
    pub difficulty: String,
    pub receipt_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuizPersistenceError {
    #[error("quiz_arguments_invalid")]
    Failed,
    #[error("quiz_execution_scope_invalid")]
    Scope,
    #[error("quiz_execution_not_found")]
    NotFound,
    #[error("quiz_execution_idempotency_conflict")]
    IdempotencyConflict,
    #[error("quiz_execution_outcome_unknown")]
    SideEffectUnknown,
}

impl QuizPersistenceError {
    pub fn is_side_effect_unknown(&self) -> bool {
        matches!(self, Self::SideEffectUnknown)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuizArguments {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_difficulty")]
    difficulty: String,
    questions: Vec<QuestionArguments>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionArguments {
    question_num: i32,
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

#[derive(Clone, Debug)]
struct ValidatedQuiz {
    title: String,
    description: String,
    difficulty: String,
    questions: Vec<ValidatedQuestion>,
}

#[derive(Clone, Debug)]
struct ValidatedQuestion {
    question_num: i32,
    question_type: String,
    question_text: String,
    options: Vec<QuizOption>,
    correct_answer: Vec<String>,
    explanation: String,
}

#[derive(Clone)]
pub struct QuizPersistenceAdapter {
    pool: PgPool,
}

impl QuizPersistenceAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Persist one validated quiz under the Runtime's execution receipt.
    ///
    /// `arguments` is the original tool payload. `arguments_hash` must be the
    /// canonical hash bound into the Runtime lease. No retry is issued after a
    /// database error: a failed commit is reported as `SideEffectUnknown`.
    pub async fn persist(
        &self,
        context: &WriteCapabilityContext,
        tool_call_id: &str,
        arguments_hash: &str,
        arguments: Value,
    ) -> Result<QuizPersistenceResult, QuizPersistenceError> {
        let quiz = validate_arguments(&arguments)?;
        validate_context(context, tool_call_id, arguments_hash, &arguments)?;

        let execution_id = parse_uuid(&context.execution_id)?;
        let run_id = parse_uuid(&context.run_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| QuizPersistenceError::SideEffectUnknown)?;

        // Locking on the full owner scope prevents a concurrent retry from
        // racing the result-summary receipt. It also prevents one tenant's
        // execution from serializing unrelated executions in another tenant.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "generate_quiz:{}:{}:{}:{}:{}",
                context.tenant_id,
                context.user_id,
                context.session_id,
                context.run_id,
                tool_call_id
            ))
            .execute(&mut *transaction)
            .await
            .map_err(|_| QuizPersistenceError::SideEffectUnknown)?;

        let row = sqlx::query(
            "SELECT tenant_id, user_id, session_id, run_id, tool_call_id,
                    capability_id, arguments_sha256, status, result_summary
               FROM assistant_capability_executions
              WHERE execution_id = $1
              FOR UPDATE",
        )
        .bind(execution_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| QuizPersistenceError::SideEffectUnknown)?
        .ok_or(QuizPersistenceError::NotFound)?;

        let owner_matches = row.try_get::<String, _>("tenant_id").ok()
            == Some(context.tenant_id.clone())
            && row.try_get::<String, _>("user_id").ok() == Some(context.user_id.clone())
            && row.try_get::<String, _>("session_id").ok() == Some(context.session_id.clone())
            && row.try_get::<Uuid, _>("run_id").ok() == Some(run_id)
            && row.try_get::<String, _>("tool_call_id").ok() == Some(tool_call_id.to_string());
        if !owner_matches {
            return Err(QuizPersistenceError::Scope);
        }
        if row.try_get::<String, _>("capability_id").ok().as_deref() != Some("generate_quiz")
            || row.try_get::<String, _>("arguments_sha256").ok().as_deref()
                != arguments_hash.strip_prefix("sha256:")
        {
            return Err(QuizPersistenceError::IdempotencyConflict);
        }

        if let Some(result) = stored_receipt(
            row.try_get::<Option<Value>, _>("result_summary")
                .ok()
                .flatten(),
        ) {
            if result.receipt_id != context.execution_id {
                return Err(QuizPersistenceError::IdempotencyConflict);
            }
            // A prior successful invocation owns the receipt. Verify that the
            // referenced quiz is still in the same scope before replaying it.
            let quiz_id = parse_uuid(&result.quiz_id)?;
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM assistant.quizzes
                                WHERE id=$1 AND tenant_id=$2 AND created_by=$3)",
            )
            .bind(quiz_id)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| QuizPersistenceError::SideEffectUnknown)?;
            if !exists {
                return Err(QuizPersistenceError::SideEffectUnknown);
            }
            transaction
                .commit()
                .await
                .map_err(|_| QuizPersistenceError::SideEffectUnknown)?;
            return Ok(result);
        }

        let status = row.try_get::<String, _>("status").ok();
        if status.as_deref().is_some_and(|value| {
            matches!(
                value,
                "succeeded" | "failed" | "cancelled" | "timeout" | "side_effect_unknown"
            )
        }) {
            return Err(QuizPersistenceError::IdempotencyConflict);
        }

        let quiz_id = Uuid::now_v7();
        let receipt_id = context.execution_id.clone();
        let result = QuizPersistenceResult {
            quiz_id: quiz_id.to_string(),
            title: quiz.title.clone(),
            question_count: quiz.questions.len(),
            difficulty: quiz.difficulty.clone(),
            receipt_id,
        };
        let dataset_ids = Value::Array(
            context
                .bound_dataset_ids
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        );

        sqlx::query(
            "INSERT INTO assistant.quizzes
                (id, tenant_id, created_by, title, description, dataset_ids,
                 topic, question_count, difficulty, config, status, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'ready',NOW(),NOW())",
        )
        .bind(quiz_id)
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&quiz.title)
        .bind(&quiz.description)
        .bind(dataset_ids)
        .bind(&quiz.title)
        .bind(i32::try_from(quiz.questions.len()).map_err(|_| QuizPersistenceError::Failed)?)
        .bind(&quiz.difficulty)
        .bind(json!({}))
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;

        for question in &quiz.questions {
            sqlx::query(
                "INSERT INTO assistant.quiz_questions
                    (id, quiz_id, question_num, question_type, question_text,
                     options, correct_answer, explanation, source_chunks, created_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,NOW())",
            )
            .bind(Uuid::now_v7())
            .bind(quiz_id)
            .bind(question.question_num)
            .bind(&question.question_type)
            .bind(&question.question_text)
            .bind(
                serde_json::to_value(&question.options)
                    .map_err(|_| QuizPersistenceError::Failed)?,
            )
            .bind(
                serde_json::to_value(&question.correct_answer)
                    .map_err(|_| QuizPersistenceError::Failed)?,
            )
            .bind(&question.explanation)
            .bind(json!([]))
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        }

        // The receipt is stored on the already-reserved execution. It is
        // intentionally not marked terminal here; the Runtime owns event
        // sequencing and will append the terminal result exactly once.
        let receipt = json!({
            "schema_version": "ai-platform/durable-capability-receipt/v1",
            "capability_id": "generate_quiz",
            "result": result,
        });
        let updated = sqlx::query(
            "UPDATE assistant_capability_executions
                SET result_summary=$2, updated_at=NOW()
              WHERE execution_id=$1 AND tenant_id=$3 AND user_id=$4
                AND session_id=$5 AND run_id=$6 AND tool_call_id=$7
                AND status NOT IN ('succeeded','failed','cancelled','timeout','side_effect_unknown')",
        )
        .bind(execution_id)
        .bind(receipt)
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(run_id)
        .bind(tool_call_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if updated.rows_affected() != 1 {
            return Err(QuizPersistenceError::SideEffectUnknown);
        }

        transaction
            .commit()
            .await
            .map_err(|_| QuizPersistenceError::SideEffectUnknown)?;
        Ok(result)
    }
}

fn default_difficulty() -> String {
    "medium".to_string()
}

fn parse_uuid(value: &str) -> Result<Uuid, QuizPersistenceError> {
    Uuid::parse_str(value).map_err(|_| QuizPersistenceError::Scope)
}

fn validate_context(
    context: &WriteCapabilityContext,
    tool_call_id: &str,
    arguments_hash: &str,
    arguments: &Value,
) -> Result<(), QuizPersistenceError> {
    if context.capability_revision == 0
        || context.tenant_id.is_empty()
        || context.user_id.is_empty()
        || context.session_id.is_empty()
        || tool_call_id.is_empty()
        || tool_call_id.len() > 160
        || tool_call_id.bytes().any(|byte| byte.is_ascii_control())
        || !arguments_hash.starts_with("sha256:")
        || arguments_hash.len() != 71
        || canonical_json_hash(arguments).map_err(|_| QuizPersistenceError::Failed)?
            != arguments_hash
    {
        return Err(QuizPersistenceError::Failed);
    }
    parse_uuid(&context.execution_id)?;
    parse_uuid(&context.run_id)?;
    Ok(())
}

fn validate_arguments(arguments: &Value) -> Result<ValidatedQuiz, QuizPersistenceError> {
    let parsed: QuizArguments =
        serde_json::from_value(arguments.clone()).map_err(|_| QuizPersistenceError::Failed)?;
    if parsed.title.trim().is_empty()
        || parsed.title.chars().count() > MAX_TITLE_CHARS
        || parsed.description.chars().count() > MAX_DESCRIPTION_CHARS
        || !matches!(parsed.difficulty.as_str(), "easy" | "medium" | "hard")
        || parsed.questions.is_empty()
        || parsed.questions.len() > MAX_QUESTIONS
    {
        return Err(QuizPersistenceError::Failed);
    }

    let mut questions = Vec::with_capacity(parsed.questions.len());
    for (index, mut question) in parsed.questions.into_iter().enumerate() {
        if question.question_num != i32::try_from(index + 1).unwrap_or(i32::MAX)
            || question.question_text.trim().is_empty()
            || question.question_text.chars().count() > MAX_QUESTION_TEXT_CHARS
            || question.explanation.chars().count() > MAX_EXPLANATION_CHARS
        {
            return Err(QuizPersistenceError::Failed);
        }
        let question_type = question.question_type;
        if !matches!(
            question_type.as_str(),
            "mc_single" | "mc_multi" | "true_false"
        ) {
            return Err(QuizPersistenceError::Failed);
        }
        // Real models commonly materialize a true/false question as the
        // visible A=Correct/B=Incorrect pair even though the canonical quiz
        // record stores only ["true"]/["false"].  The catalog schema permits
        // options here, so normalize this one unambiguous representation
        // instead of failing an otherwise valid, user-approved quiz.
        if question_type == "true_false" && is_true_false_option_pair(&question.options) {
            question.options.clear();
        }
        let mut labels = Vec::with_capacity(question.options.len());
        for option in &question.options {
            if !matches!(option.label.as_str(), "A" | "B" | "C" | "D")
                || option.text.trim().is_empty()
                || option.text.chars().count() > MAX_OPTION_TEXT_CHARS
                || option
                    .text
                    .trim()
                    .eq_ignore_ascii_case(option.label.as_str())
                || !labels.push_if_absent(option.label.clone())
            {
                return Err(QuizPersistenceError::Failed);
            }
        }

        let mut answers = question.correct_answer;
        match question_type.as_str() {
            "mc_single" => {
                if question.options.len() != 4
                    || labels != vec!["A", "B", "C", "D"]
                    || answers.len() != 1
                    || !matches!(
                        answers.first().map(String::as_str),
                        Some("A" | "B" | "C" | "D")
                    )
                {
                    return Err(QuizPersistenceError::Failed);
                }
            }
            "mc_multi" => {
                if question.options.len() != 4
                    || labels != vec!["A", "B", "C", "D"]
                    || !(2..=3).contains(&answers.len())
                    || answers
                        .iter()
                        .any(|answer| !matches!(answer.as_str(), "A" | "B" | "C" | "D"))
                    || has_duplicates(&answers)
                {
                    return Err(QuizPersistenceError::Failed);
                }
            }
            "true_false" => {
                if !question.options.is_empty()
                    || answers.len() != 1
                    || !matches!(
                        answers
                            .first()
                            .map(|value| value.to_ascii_lowercase())
                            .as_deref(),
                        Some("true" | "false")
                    )
                {
                    return Err(QuizPersistenceError::Failed);
                }
                answers[0] = answers[0].to_ascii_lowercase();
            }
            _ => unreachable!(),
        }
        questions.push(ValidatedQuestion {
            question_num: question.question_num,
            question_type,
            question_text: question.question_text,
            options: question.options,
            correct_answer: answers,
            explanation: question.explanation,
        });
    }
    Ok(ValidatedQuiz {
        title: parsed.title,
        description: parsed.description,
        difficulty: parsed.difficulty,
        questions,
    })
}

trait PushIfAbsent {
    fn push_if_absent(&mut self, value: String) -> bool;
}

impl PushIfAbsent for Vec<String> {
    fn push_if_absent(&mut self, value: String) -> bool {
        if self.contains(&value) {
            false
        } else {
            self.push(value);
            true
        }
    }
}

fn has_duplicates(values: &[String]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
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

fn stored_receipt(value: Option<Value>) -> Option<QuizPersistenceResult> {
    let value = value?;
    if value.get("schema_version").and_then(Value::as_str)
        != Some("ai-platform/durable-capability-receipt/v1")
        || value.get("capability_id").and_then(Value::as_str) != Some("generate_quiz")
    {
        return None;
    }
    let object = value.get("result").cloned()?;
    serde_json::from_value(object).ok()
}

fn map_database_error(error: sqlx::Error) -> QuizPersistenceError {
    if let sqlx::Error::Database(database) = &error {
        if database
            .code()
            .is_some_and(|code| code.starts_with("22") || code.starts_with("23"))
        {
            return QuizPersistenceError::Failed;
        }
    }
    QuizPersistenceError::SideEffectUnknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_arguments() -> Value {
        json!({
            "title": "Transformer basics",
            "questions": [{
                "question_num": 1,
                "question_type": "mc_single",
                "question_text": "Which mechanism mixes token context?",
                "options": [
                    {"label": "A", "text": "Attention"},
                    {"label": "B", "text": "Pooling"},
                    {"label": "C", "text": "Hashing"},
                    {"label": "D", "text": "Sorting"}
                ],
                "correct_answer": ["A"],
                "explanation": "Self-attention mixes contextual information."
            }]
        })
    }

    #[test]
    fn catalog_hash_and_strict_shape_are_enforced() {
        assert_eq!(GENERATE_QUIZ_SCHEMA_HASH.len(), 71);
        assert!(validate_arguments(&valid_arguments()).is_ok());
        let mut extra = valid_arguments();
        extra["unexpected"] = json!(true);
        assert!(matches!(
            validate_arguments(&extra),
            Err(QuizPersistenceError::Failed)
        ));
    }

    #[test]
    fn question_semantics_are_fail_closed() {
        let mut invalid = valid_arguments();
        invalid["questions"][0]["correct_answer"] = json!(["A", "B"]);
        assert!(matches!(
            validate_arguments(&invalid),
            Err(QuizPersistenceError::Failed)
        ));

        let mut true_false = valid_arguments();
        true_false["questions"][0]["question_type"] = json!("true_false");
        true_false["questions"][0]["options"] = json!([]);
        true_false["questions"][0]["correct_answer"] = json!(["TRUE"]);
        let parsed = validate_arguments(&true_false).expect("true/false fixture");
        assert_eq!(parsed.questions[0].correct_answer, vec!["true"]);

        true_false["questions"][0]["options"] = json!([
            {"label": "A", "text": "正确"},
            {"label": "B", "text": "错误"}
        ]);
        let parsed = validate_arguments(&true_false).expect("visible true/false options");
        assert!(parsed.questions[0].options.is_empty());
    }

    #[test]
    fn success_payload_never_contains_answers() {
        let result = QuizPersistenceResult {
            quiz_id: Uuid::now_v7().to_string(),
            title: "Quiz".into(),
            question_count: 1,
            difficulty: "medium".into(),
            receipt_id: Uuid::now_v7().to_string(),
        };
        let encoded = serde_json::to_string(&result).expect("serializable result");
        assert!(!encoded.contains("correct_answer"));
        assert!(!encoded.contains("options"));
    }

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database with migration 096"]
    async fn postgres_contract_fixture_is_opt_in() {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        let pool = sqlx::PgPool::connect(&database_url)
            .await
            .expect("postgres");
        let _ = pool.close().await;
    }
}
