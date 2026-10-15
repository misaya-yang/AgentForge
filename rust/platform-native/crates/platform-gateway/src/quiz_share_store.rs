//! Frozen quiz-share persistence. Factories are inert; each statement is owned by
//! the existing PostgreSQL supervisor. Submission calls canonical v2 exactly once.
use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    quiz_types::{QuizError, QuizFuture},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};

pub const SHARE_SQL: &str = "SELECT to_jsonb(s) FROM (SELECT id, share_code, kind, title, payload, answer_keys, audience, tenant_id, created_by, is_active, max_attempts, expires_at, require_name, time_limit_minutes, attempt_count, created_at FROM assistant.artifact_shares WHERE share_code=$1 AND is_active=TRUE) s";
pub const GUARD_SQL: &str = "SELECT to_jsonb(s) FROM (SELECT audience, source_scope, tenant_id, is_active, expires_at, payload FROM assistant.artifact_shares WHERE share_code=$1 AND kind='quiz') s";
pub const START_SQL: &str = r#"WITH stale_tokens AS (
 SELECT id FROM assistant.artifact_share_attempt_tokens
 WHERE expires_at < NOW() - ($6 * INTERVAL '1 hour')
 OR consumed_at < NOW() - ($6 * INTERVAL '1 hour')
 ORDER BY LEAST(expires_at, COALESCE(consumed_at, expires_at)) LIMIT $7
), cleanup AS (
 DELETE FROM assistant.artifact_share_attempt_tokens WHERE id IN (SELECT id FROM stale_tokens)
)
INSERT INTO assistant.artifact_share_attempt_tokens (id,share_id,token_hash,started_at,expires_at)
SELECT $1::text::uuid,id,$2,$3,$4 FROM assistant.artifact_shares
WHERE share_code=$5 AND is_active=TRUE
AND (expires_at IS NULL OR expires_at>NOW())
AND (max_attempts IS NULL OR attempt_count<max_attempts)"#;
pub const RESULT_SQL: &str = "SELECT jsonb_build_object('id',a.id::text,'result_payload',a.result_payload) FROM assistant.quiz_attempts AS a JOIN assistant.artifact_share_attempt_tokens AS token ON token.token_hash=a.attempt_token_hash WHERE a.share_id=$1::text::uuid AND token.token_hash=$2";
pub const PRIOR_SQL: &str = "SELECT jsonb_build_object('id',a.id::text,'answers',a.answers,'display_name',a.display_name,'result_payload',a.result_payload) FROM assistant.quiz_attempts AS a JOIN assistant.artifact_share_attempt_tokens AS token ON token.token_hash=a.attempt_token_hash WHERE a.share_id=$1::text::uuid AND token.token_hash=$2";
pub const RECORD_V2_SQL: &str = "SELECT jsonb_build_object('attempt_id',attempt_id::text,'result_payload',result_payload) FROM assistant.record_artifact_share_quiz_attempt_v2($1::varchar,$2::char(64),$3::varchar,$4::text::uuid,$5::text::uuid,$6::jsonb,$7::double precision,$8::int,$9::int,$10::varchar,$11::jsonb)";

pub struct NewQuizShareToken {
    pub id: String,
    pub share_code: String,
    pub token_hash: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
pub struct NewQuizShareAttempt {
    pub share_code: String,
    pub token_hash: String,
    pub display_name: Option<String>,
    pub attempt_id: String,
    pub quiz_id: String,
    pub answers: Value,
    pub graded: Value,
    pub client_ip: Option<String>,
}
pub trait QuizShareDatabase: Send + Sync {
    fn guard_row<'a>(&'a self, code: &'a str) -> QuizFuture<'a, Option<Value>>;
    fn share<'a>(&'a self, code: &'a str) -> QuizFuture<'a, Option<Value>>;
    fn start_token(&self, token: NewQuizShareToken) -> QuizFuture<'_, bool>;
    fn prior<'a>(&'a self, share_id: &'a str, hash: &'a str) -> QuizFuture<'a, Option<Value>>;
    fn result<'a>(&'a self, share_id: &'a str, hash: &'a str) -> QuizFuture<'a, Option<Value>>;
    fn record(&self, attempt: NewQuizShareAttempt) -> QuizFuture<'_, Option<Value>>;
}

pub(crate) fn share_error(status: u16, code: &str, message: &str) -> QuizError {
    QuizError::detail(status, json!({"code":code,"message":message}))
}
fn pg_error(error: PgOperationError) -> QuizError {
    match error {
        PgOperationError::Cancelled => QuizError::Cancelled,
        _ => QuizError::Internal,
    }
}
/// No arbitrary backend text is retained. Known errors are captured privately
/// per operation before the supervisor sanitizes the original sqlx failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuizDatabaseFailure {
    Unavailable,
    Limit,
    InvalidQuiz,
    RequiredName,
    RequiredToken,
    InvalidToken,
    DifferentAnswer,
    UnavailableToken,
    DuplicateName,
}
impl QuizDatabaseFailure {
    pub fn capture(sqlstate: &str, message: &str) -> Option<Self> {
        Some(match (sqlstate, message) {
            ("P4040", _) => Self::Unavailable,
            ("P4290", _) => Self::Limit,
            ("P4000", "Share is not a valid quiz") => Self::InvalidQuiz,
            ("P4000", "This share requires a name before submitting") => Self::RequiredName,
            ("P4000", "Attempt token is required") => Self::RequiredToken,
            ("P4000", "Attempt token is invalid or expired") => Self::InvalidToken,
            ("P4090", "Attempt token belongs to another answer") => Self::DifferentAnswer,
            ("P4090", "Attempt token is no longer available") => Self::UnavailableToken,
            ("P4090", "This display name has already submitted") => Self::DuplicateName,
            _ => return None,
        })
    }
    fn sqlstate(self) -> &'static str {
        match self {
            Self::Unavailable => "P4040",
            Self::Limit => "P4290",
            Self::InvalidQuiz | Self::RequiredName | Self::RequiredToken | Self::InvalidToken => {
                "P4000"
            }
            _ => "P4090",
        }
    }
    pub fn error(self) -> QuizError {
        let (status, code, message) = match self {
            Self::Unavailable => (404, "share_unavailable", "Quiz not found or expired"),
            Self::Limit => (429, "attempt_limit_reached", "Maximum attempts reached"),
            Self::InvalidQuiz => (400, "attempt_input_invalid", "Share is not a valid quiz"),
            Self::RequiredName => (
                400,
                "attempt_input_invalid",
                "This share requires a name before submitting",
            ),
            Self::RequiredToken => (400, "attempt_input_invalid", "Attempt token is required"),
            Self::InvalidToken => (
                400,
                "attempt_input_invalid",
                "Attempt token is invalid or expired",
            ),
            Self::DifferentAnswer => (
                409,
                "attempt_conflict",
                "Attempt token belongs to another answer",
            ),
            Self::UnavailableToken => (
                409,
                "attempt_conflict",
                "Attempt token is no longer available",
            ),
            Self::DuplicateName => (
                409,
                "attempt_conflict",
                "This display name has already submitted",
            ),
        };
        share_error(status, code, message)
    }
}
fn captured_error(error: PgOperationError, captured: Option<QuizDatabaseFailure>) -> QuizError {
    // Cancellation, indeterminate outcomes, control failures and unrelated
    // database errors cannot be overwritten by a captured statement failure.
    if let (
        PgOperationError::Database {
            sqlstate: Some(state),
        },
        Some(failure),
    ) = (&error, captured)
        && state == failure.sqlstate()
    {
        return failure.error();
    }
    pg_error(error)
}
#[derive(Clone)]
pub struct PgQuizShareDatabase {
    operations: PgOperationStorage,
}
impl PgQuizShareDatabase {
    pub fn new(pool: PgPool, independent_control_pool: PgPool) -> Result<Self, QuizError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, independent_control_pool)
                .map_err(pg_error)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), QuizError> {
        self.operations.drain_cancelled().await.map_err(pg_error)
    }
    fn one<'a>(&'a self, sql: &'static str, code: &'a str) -> QuizFuture<'a, Option<Value>> {
        let code = code.to_owned();
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query_scalar(sql)
                            .bind(code)
                            .fetch_optional(connection)
                            .await
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn token_row<'a>(
        &'a self,
        sql: &'static str,
        share_id: &'a str,
        hash: &'a str,
    ) -> QuizFuture<'a, Option<Value>> {
        let (share_id, hash) = (share_id.to_owned(), hash.to_owned());
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query_scalar(sql)
                            .bind(share_id)
                            .bind(hash)
                            .fetch_optional(connection)
                            .await
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
}
impl QuizShareDatabase for PgQuizShareDatabase {
    fn guard_row<'a>(&'a self, code: &'a str) -> QuizFuture<'a, Option<Value>> {
        self.one(GUARD_SQL, code)
    }
    fn share<'a>(&'a self, code: &'a str) -> QuizFuture<'a, Option<Value>> {
        self.one(SHARE_SQL, code)
    }
    fn prior<'a>(&'a self, id: &'a str, hash: &'a str) -> QuizFuture<'a, Option<Value>> {
        self.token_row(PRIOR_SQL, id, hash)
    }
    fn result<'a>(&'a self, id: &'a str, hash: &'a str) -> QuizFuture<'a, Option<Value>> {
        self.token_row(RESULT_SQL, id, hash)
    }
    fn start_token(&self, t: NewQuizShareToken) -> QuizFuture<'_, bool> {
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |connection| {
                    Box::pin(async move {
                        Ok(sqlx::query(START_SQL)
                            .bind(t.id)
                            .bind(t.token_hash)
                            .bind(t.started_at)
                            .bind(t.expires_at)
                            .bind(t.share_code)
                            .bind(24_f64)
                            .bind(100_i64)
                            .execute(connection)
                            .await?
                            .rows_affected()
                            == 1)
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn record(&self, a: NewQuizShareAttempt) -> QuizFuture<'_, Option<Value>> {
        Box::pin(async move {
            let score = a
                .graded
                .get("total_score")
                .and_then(Value::as_f64)
                .ok_or(QuizError::Internal)?;
            let correct = a
                .graded
                .get("correct_count")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .ok_or(QuizError::Internal)?;
            let total = a
                .graded
                .get("total_count")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .ok_or(QuizError::Internal)?;
            let captured = Arc::new(Mutex::new(None));
            let writer = captured.clone();
            let outcome = self
                .operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |connection| {
                    Box::pin(async move {
                        let result = sqlx::query_scalar(RECORD_V2_SQL)
                            .bind(a.share_code)
                            .bind(a.token_hash)
                            .bind(a.display_name)
                            .bind(a.attempt_id)
                            .bind(a.quiz_id)
                            .bind(a.answers)
                            .bind(score)
                            .bind(correct)
                            .bind(total)
                            .bind(a.client_ip)
                            .bind(a.graded)
                            .fetch_optional(connection)
                            .await;
                        if let Err(error) = &result
                            && let Some(db) = error.as_database_error()
                            && let Some(state) = db.code()
                            && let Ok(mut cell) = writer.lock()
                        {
                            *cell = QuizDatabaseFailure::capture(&state, db.message());
                        }
                        // Return the original failure to preserve supervisor rollback.
                        result
                    })
                })
                .await;
            outcome.map_err(|error| {
                captured_error(error, captured.lock().ok().and_then(|mut cell| cell.take()))
            })
        })
    }
}

