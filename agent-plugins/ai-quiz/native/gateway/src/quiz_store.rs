//! Authenticated quiz persistence. Construction is inert; each SQL statement is
//! owned by PgOperationStorage. Source authorization belongs to the route scope
//! adapter and deliberately does not apply to creator-only deletion.
use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    quiz_types::{QuizError, QuizFuture, QuizIdentity},
};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct NewQuizAttempt {
    pub attempt_id: String,
    pub quiz_id: String,
    pub user_id: String,
    pub answers: Value,
    pub total_score: f64,
    pub correct_count: i32,
    pub total_count: i32,
    pub now: DateTime<Utc>,
    pub result_payload: Value,
}
/// Queries expose private answers only inside the service; the public projection
/// always reconstructs questions from an allowlist.
pub trait QuizDatabase: Send + Sync {
    fn quiz<'a>(&'a self, user: &'a QuizIdentity, id: &'a str) -> QuizFuture<'a, Option<Value>>;
    fn questions<'a>(&'a self, id: &'a str) -> QuizFuture<'a, Vec<Value>>;
    fn prior<'a>(
        &'a self,
        user: &'a QuizIdentity,
        attempt: &'a str,
    ) -> QuizFuture<'a, Option<Value>>;
    fn insert<'a>(&'a self, attempt: NewQuizAttempt) -> QuizFuture<'a, bool>;
    fn result<'a>(
        &'a self,
        user: &'a QuizIdentity,
        quiz: &'a str,
        attempt: &'a str,
    ) -> QuizFuture<'a, Option<Value>>;
    fn creator<'a>(
        &'a self,
        user: &'a QuizIdentity,
        quiz: &'a str,
    ) -> QuizFuture<'a, Option<String>>;
    fn count<'a>(&'a self, quiz: &'a str, user: Option<&'a str>) -> QuizFuture<'a, i64>;
    fn page<'a>(
        &'a self,
        quiz: &'a str,
        user: Option<&'a str>,
        limit: i64,
        offset: i64,
    ) -> QuizFuture<'a, Vec<Value>>;
    fn delete<'a>(&'a self, user: &'a QuizIdentity, quiz: &'a str) -> QuizFuture<'a, bool>;
}
pub const QUIZ_READ_SQL: &str =
    "SELECT to_jsonb(q) FROM assistant.quizzes q WHERE id=$1::text::uuid AND tenant_id=$2";
pub const QUIZ_QUESTIONS_SQL: &str = "SELECT to_jsonb(q) FROM assistant.quiz_questions q WHERE quiz_id=$1::text::uuid ORDER BY question_num";
pub const QUIZ_PRIOR_SQL: &str = "SELECT jsonb_build_object('quiz_id',a.quiz_id::text,'user_id',a.user_id,'answers',a.answers,'result_payload',a.result_payload) FROM assistant.quiz_attempts a JOIN assistant.quizzes q ON q.id=a.quiz_id WHERE a.id=$1::text::uuid AND q.tenant_id=$2";
pub const QUIZ_INSERT_SQL: &str = "INSERT INTO assistant.quiz_attempts (id,quiz_id,user_id,answers,total_score,correct_count,total_count,started_at,completed_at,status,result_payload) VALUES ($1::text::uuid,$2::text::uuid,$3,$4::jsonb,$5,$6,$7,$8,$8,'completed',$9::jsonb) ON CONFLICT (id) DO NOTHING RETURNING id::text";
pub const QUIZ_RESULT_SQL: &str = "SELECT a.result_payload FROM assistant.quiz_attempts a JOIN assistant.quizzes q ON q.id=a.quiz_id WHERE a.id=$1::text::uuid AND a.quiz_id=$2::text::uuid AND a.user_id=$3 AND q.tenant_id=$4";
pub const QUIZ_CREATOR_SQL: &str =
    "SELECT created_by FROM assistant.quizzes WHERE id=$1::text::uuid AND tenant_id=$2";
pub const QUIZ_DELETE_SQL: &str =
    "DELETE FROM assistant.quizzes WHERE id=$1::text::uuid AND tenant_id=$2 AND created_by=$3";
fn pg_error(error: PgOperationError) -> QuizError {
    match error {
        PgOperationError::Cancelled => QuizError::Cancelled,
        _ => QuizError::Internal,
    }
}
#[derive(Clone)]
pub struct PgQuizDatabase {
    operations: PgOperationStorage,
}
impl PgQuizDatabase {
    pub fn new(pool: PgPool, independent_control_pool: PgPool) -> Result<Self, QuizError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, independent_control_pool)
                .map_err(pg_error)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), QuizError> {
        self.operations.drain_cancelled().await.map_err(pg_error)
    }
}
impl QuizDatabase for PgQuizDatabase {
    fn quiz<'a>(&'a self, user: &'a QuizIdentity, id: &'a str) -> QuizFuture<'a, Option<Value>> {
        let (id, tenant) = (id.to_owned(), user.tenant_id.clone());
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        sqlx::query_scalar(QUIZ_READ_SQL)
                            .bind(id)
                            .bind(tenant)
                            .fetch_optional(c)
                            .await
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn questions<'a>(&'a self, id: &'a str) -> QuizFuture<'a, Vec<Value>> {
        let id = id.to_owned();
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        sqlx::query_scalar(QUIZ_QUESTIONS_SQL)
                            .bind(id)
                            .fetch_all(c)
                            .await
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn prior<'a>(
        &'a self,
        user: &'a QuizIdentity,
        attempt: &'a str,
    ) -> QuizFuture<'a, Option<Value>> {
        let (id, tenant) = (attempt.to_owned(), user.tenant_id.clone());
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        sqlx::query_scalar(QUIZ_PRIOR_SQL)
                            .bind(id)
                            .bind(tenant)
                            .fetch_optional(c)
                            .await
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn insert<'a>(&'a self, a: NewQuizAttempt) -> QuizFuture<'a, bool> {
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        let inserted: Option<String> = sqlx::query_scalar(QUIZ_INSERT_SQL)
                            .bind(a.attempt_id)
                            .bind(a.quiz_id)
                            .bind(a.user_id)
                            .bind(a.answers)
                            .bind(a.total_score)
                            .bind(a.correct_count)
                            .bind(a.total_count)
                            .bind(a.now)
                            .bind(a.result_payload)
                            .fetch_optional(c)
                            .await?;
                        Ok(inserted.is_some())
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn result<'a>(
        &'a self,
        user: &'a QuizIdentity,
        quiz: &'a str,
        attempt: &'a str,
    ) -> QuizFuture<'a, Option<Value>> {
        let (id, qid, uid, tenant) = (
            attempt.to_owned(),
            quiz.to_owned(),
            user.user_id.clone(),
            user.tenant_id.clone(),
        );
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        let row: Option<Option<Value>> = sqlx::query_scalar(QUIZ_RESULT_SQL)
                            .bind(id)
                            .bind(qid)
                            .bind(uid)
                            .bind(tenant)
                            .fetch_optional(c)
                            .await?;
                        Ok(row.flatten())
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn creator<'a>(
        &'a self,
        user: &'a QuizIdentity,
        quiz: &'a str,
    ) -> QuizFuture<'a, Option<String>> {
        let (id, tenant) = (quiz.to_owned(), user.tenant_id.clone());
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        sqlx::query_scalar(QUIZ_CREATOR_SQL)
                            .bind(id)
                            .bind(tenant)
                            .fetch_optional(c)
                            .await
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
    fn count<'a>(&'a self, quiz: &'a str, user: Option<&'a str>) -> QuizFuture<'a, i64> {
        let (quiz, user) = (quiz.to_owned(), user.map(str::to_owned));
        Box::pin(async move {
            self.operations.acquire().await.map_err(pg_error)?.execute(move |c| Box::pin(async move {
            // A tenant-scoped creator read precedes this statement. UUID quiz IDs
            // are globally unique. NULL here means the verified creator sees all.
            sqlx::query_scalar("SELECT count(*) FROM assistant.quiz_attempts WHERE quiz_id=$1::text::uuid AND ($2::text IS NULL OR user_id=$2)").bind(quiz).bind(user).fetch_one(c).await
        })).await.map_err(pg_error)
        })
    }
    fn page<'a>(
        &'a self,
        quiz: &'a str,
        user: Option<&'a str>,
        limit: i64,
        offset: i64,
    ) -> QuizFuture<'a, Vec<Value>> {
        let (quiz, user) = (quiz.to_owned(), user.map(str::to_owned));
        Box::pin(async move {
            self.operations.acquire().await.map_err(pg_error)?.execute(move |c| Box::pin(async move {
            sqlx::query_scalar("SELECT jsonb_build_object('attempt_id',id::text,'user_id',user_id,'display_name',display_name,'total_score',total_score::double precision,'correct_count',correct_count,'total_count',total_count,'started_at',started_at,'completed_at',completed_at,'status',status) FROM assistant.quiz_attempts WHERE quiz_id=$1::text::uuid AND ($2::text IS NULL OR user_id=$2) ORDER BY started_at DESC LIMIT $3 OFFSET $4").bind(quiz).bind(user).bind(limit).bind(offset).fetch_all(c).await
        })).await.map_err(pg_error)
        })
    }
    fn delete<'a>(&'a self, user: &'a QuizIdentity, quiz: &'a str) -> QuizFuture<'a, bool> {
        let (id, tenant, user) = (
            quiz.to_owned(),
            user.tenant_id.clone(),
            user.user_id.clone(),
        );
        Box::pin(async move {
            self.operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |c| {
                    Box::pin(async move {
                        Ok(sqlx::query(QUIZ_DELETE_SQL)
                            .bind(id)
                            .bind(tenant)
                            .bind(user)
                            .execute(c)
                            .await?
                            .rows_affected()
                            == 1)
                    })
                })
                .await
                .map_err(pg_error)
        })
    }
}
fn canonical_id(id: &str) -> Result<String, QuizError> {
    Uuid::parse_str(id)
        .map(|id| id.to_string())
        .map_err(|_| QuizError::Internal)
}
fn decoded(value: &Value) -> Result<Value, QuizError> {
    match value {
        Value::String(raw) => serde_json::from_str(raw).map_err(|_| QuizError::Internal),
        _ => Ok(value.clone()),
    }
}
pub fn safe_quiz_options(raw: &Value) -> Value {
    let decoded = decoded(raw).unwrap_or(Value::Null);
    Value::Array(decoded.as_array().into_iter().flatten().filter_map(|item| {
        Some(json!({"label":item.get("label")?.as_str()?,"text":item.get("text")?.as_str()?}))
    }).collect())
}
#[derive(Clone)]
pub struct QuizService {
    database: Arc<dyn QuizDatabase>,
}
impl QuizService {
    pub fn new(database: Arc<dyn QuizDatabase>) -> Self {
        Self { database }
    }
    async fn load(
        &self,
        user: &QuizIdentity,
        id: &str,
        include_answers: bool,
    ) -> Result<Option<Value>, QuizError> {
        let Some(row) = self.database.quiz(user, id).await? else {
            return Ok(None);
        };
        let mut questions = Vec::new();
        for row in self.database.questions(id).await? {
            let mut q = json!({"id":row["id"],"question_num":row["question_num"],"question_type":row["question_type"],"question_text":row["question_text"],"options":safe_quiz_options(&row["options"])});
            if include_answers {
                let raw = &row["correct_answer"];
                q["correct_answer"] = match raw {
                    Value::Array(_) => raw.clone(),
                    Value::String(s) if !s.is_empty() => decoded(raw)?,
                    Value::Null | Value::Bool(false) => json!([]),
                    Value::String(_) => json!([]),
                    Value::Number(n) if n.as_f64() == Some(0.0) => json!([]),
                    Value::Object(m) if m.is_empty() => json!([]),
                    _ => return Err(QuizError::Internal),
                };
                q["explanation"] = row["explanation"].clone();
            }
            questions.push(q);
        }
        Ok(Some(
            json!({"quiz_id":row["id"],"title":row["title"],"description":row["description"],"topic":row["topic"],"difficulty":row["difficulty"],"question_count":row["question_count"],"status":row["status"],"created_at":row["created_at"],"questions":questions}),
        ))
    }
    pub async fn get_quiz(
        &self,
        user: &QuizIdentity,
        id: &str,
    ) -> Result<Option<Value>, QuizError> {
        self.load(user, &canonical_id(id)?, false).await
    }
    async fn replay(
        &self,
        user: &QuizIdentity,
        quiz: &str,
        attempt: &str,
        answers: &Value,
    ) -> Result<Option<Value>, QuizError> {
        let Some(prior) = self.database.prior(user, attempt).await? else {
            return Ok(None);
        };
        if prior["quiz_id"].as_str() != Some(quiz)
            || prior["user_id"].as_str() != Some(user.user_id.as_str())
            || decoded(&prior["answers"])? != *answers
        {
            return Err(QuizError::detail(
                409,
                "Attempt id belongs to a different submission",
            ));
        }
        let mut result = decoded(&prior["result_payload"])?;
        if !result.is_object() {
            return Err(QuizError::detail(409, "Attempt result is unavailable"));
        }
        result["attempt_id"] = json!(attempt);
        result["cached"] = json!(true);
        Ok(Some(result))
    }
    pub async fn submit_attempt(
        &self,
        user: &QuizIdentity,
        quiz: &str,
        answers: Map<String, Value>,
        attempt: Option<&str>,
    ) -> Result<Value, QuizError> {
        let quiz = canonical_id(quiz)?;
        let attempt = match attempt {
            Some(id) => canonical_id(id)?,
            None => Uuid::new_v4().to_string(),
        };
        let answers_value = Value::Object(answers.clone());
        if let Some(saved) = self.replay(user, &quiz, &attempt, &answers_value).await? {
            return Ok(saved);
        }
        let data = self
            .load(user, &quiz, true)
            .await?
            .ok_or_else(|| QuizError::detail(404, format!("Quiz {quiz} not found")))?;
        let questions = data["questions"].as_array().ok_or(QuizError::Internal)?;
        // The canonical endpoint supplies no model registry. Short answers retain
        // the synchronous pending-AI grade even through its grade_async branch.
        let mut result = crate::quiz_grading::grade(questions, &answers);
        if !result.is_object() {
            return Err(QuizError::Internal);
        }
        result["attempt_id"] = json!(attempt);
        let new = NewQuizAttempt {
            attempt_id: attempt.clone(),
            quiz_id: quiz.clone(),
            user_id: user.user_id.clone(),
            answers: answers_value.clone(),
            total_score: result["total_score"].as_f64().ok_or(QuizError::Internal)?,
            correct_count: i32::try_from(
                result["correct_count"]
                    .as_i64()
                    .ok_or(QuizError::Internal)?,
            )
            .map_err(|_| QuizError::Internal)?,
            total_count: i32::try_from(result["total_count"].as_i64().ok_or(QuizError::Internal)?)
                .map_err(|_| QuizError::Internal)?,
            now: Utc::now(),
            result_payload: result.clone(),
        };
        if self.database.insert(new).await? {
            return Ok(result);
        }
        self.replay(user, &quiz, &attempt, &answers_value)
            .await?
            .ok_or_else(|| QuizError::detail(409, "Attempt result is unavailable"))
    }
    pub async fn get_attempt_result(
        &self,
        user: &QuizIdentity,
        quiz: &str,
        attempt: &str,
    ) -> Result<Option<Value>, QuizError> {
        match self
            .database
            .result(user, &canonical_id(quiz)?, &canonical_id(attempt)?)
            .await?
        {
            None | Some(Value::Null) => Ok(None),
            Some(value) => Ok(Some(decoded(&value)?)),
        }
    }
    pub async fn list_attempts(
        &self,
        user: &QuizIdentity,
        quiz: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value, QuizError> {
        let quiz = canonical_id(quiz)?;
        let Some(creator) = self.database.creator(user, &quiz).await? else {
            return Ok(json!({"attempts":[],"total":0}));
        };
        let filter = if creator == user.user_id {
            None
        } else {
            Some(user.user_id.as_str())
        };
        let total = self.database.count(&quiz, filter).await?;
        let attempts = self.database.page(&quiz, filter, limit, offset).await?;
        Ok(json!({"attempts":attempts,"total":total}))
    }
    pub async fn delete_quiz(&self, user: &QuizIdentity, quiz: &str) -> Result<bool, QuizError> {
        self.database.delete(user, &canonical_id(quiz)?).await
    }
}

#[cfg(test)]
mod plugin_service_tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex};
    struct Memory {
        attempts: Mutex<BTreeMap<String, Value>>,
        writes: Mutex<usize>,
    }
    impl Memory {
        fn new() -> Self {
            Self {
                attempts: Mutex::new(BTreeMap::new()),
                writes: Mutex::new(0),
            }
        }
    }
    impl QuizDatabase for Memory {
        fn quiz<'a>(&'a self, u: &'a QuizIdentity, id: &'a str) -> QuizFuture<'a, Option<Value>> {
            Box::pin(async move {
                Ok((u.tenant_id.as_deref()==Some("tenant-one")).then(||json!({"id":id,"title":"fixture","description":null,"topic":null,"difficulty":"easy","question_count":1,"status":"ready","created_at":"2026-10-01T00:00:00Z"})))
            })
        }
        fn questions<'a>(&'a self, _id: &'a str) -> QuizFuture<'a, Vec<Value>> {
            Box::pin(async {
                Ok(vec![
                    json!({"id":"00000000-0000-4000-8000-000000000010","question_num":1,"question_type":"mc_single","question_text":"fixture?","options":[{"label":"A","text":"option","private":"must not leak"}],"correct_answer":["A"],"explanation":"answer key"}),
                ])
            })
        }
        fn prior<'a>(
            &'a self,
            u: &'a QuizIdentity,
            attempt: &'a str,
        ) -> QuizFuture<'a, Option<Value>> {
            Box::pin(async move {
                Ok(if u.tenant_id.as_deref() == Some("tenant-one") {
                    self.attempts.lock().unwrap().get(attempt).cloned()
                } else {
                    None
                })
            })
        }
        fn insert<'a>(&'a self, a: NewQuizAttempt) -> QuizFuture<'a, bool> {
            Box::pin(async move {
                let mut records = self.attempts.lock().unwrap();
                if records.contains_key(&a.attempt_id) {
                    return Ok(false);
                }
                records.insert(a.attempt_id,json!({"quiz_id":a.quiz_id,"user_id":a.user_id,"answers":a.answers,"result_payload":a.result_payload}));
                *self.writes.lock().unwrap() += 1;
                Ok(true)
            })
        }
        fn result<'a>(
            &'a self,
            _u: &'a QuizIdentity,
            _q: &'a str,
            _a: &'a str,
        ) -> QuizFuture<'a, Option<Value>> {
            Box::pin(async { Ok(None) })
        }
        fn creator<'a>(
            &'a self,
            _u: &'a QuizIdentity,
            _q: &'a str,
        ) -> QuizFuture<'a, Option<String>> {
            Box::pin(async { Ok(Some("user-one".into())) })
        }
        fn count<'a>(&'a self, _q: &'a str, _u: Option<&'a str>) -> QuizFuture<'a, i64> {
            Box::pin(async { Ok(0) })
        }
        fn page<'a>(
            &'a self,
            _q: &'a str,
            _u: Option<&'a str>,
            _l: i64,
            _o: i64,
        ) -> QuizFuture<'a, Vec<Value>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn delete<'a>(&'a self, _u: &'a QuizIdentity, _q: &'a str) -> QuizFuture<'a, bool> {
            Box::pin(async { Ok(false) })
        }
    }
    #[tokio::test]
    async fn plugin_reads_redact_answers_and_submissions_remain_owner_bound_idempotent() {
        let db = Arc::new(Memory::new());
        let service = QuizService::new(db.clone());
        let u = QuizIdentity {
            user_id: "user-one".into(),
            tenant_id: Some("tenant-one".into()),
            ..Default::default()
        };
        let q = "00000000-0000-4000-8000-000000000001";
        let attempt = "00000000-0000-4000-8000-000000000002";
        let read = service.get_quiz(&u, q).await.unwrap().unwrap();
        let encoded = read.to_string();
        assert!(!encoded.contains("correct_answer"));
        assert!(!encoded.contains("answer key"));
        assert!(!encoded.contains("must not leak"));
        let answers =
            serde_json::from_value(json!({"00000000-0000-4000-8000-000000000010":"A"})).unwrap();
        let first = service
            .submit_attempt(&u, q, answers, Some(attempt))
            .await
            .unwrap();
        assert_eq!(first["correct_count"], 1);
        let same =
            serde_json::from_value(json!({"00000000-0000-4000-8000-000000000010":"A"})).unwrap();
        assert_eq!(
            service
                .submit_attempt(&u, q, same, Some(attempt))
                .await
                .unwrap()["cached"],
            true
        );
        assert_eq!(*db.writes.lock().unwrap(), 1);
        let changed =
            serde_json::from_value(json!({"00000000-0000-4000-8000-000000000010":"B"})).unwrap();
        assert!(matches!(
            service.submit_attempt(&u, q, changed, Some(attempt)).await,
            Err(QuizError::Http(409, _))
        ));
        let foreign = QuizIdentity {
            tenant_id: Some("tenant-two".into()),
            ..u.clone()
        };
        assert!(service.get_quiz(&foreign, q).await.unwrap().is_none());
        let other = QuizIdentity {
            user_id: "user-two".into(),
            ..u
        };
        let same =
            serde_json::from_value(json!({"00000000-0000-4000-8000-000000000010":"A"})).unwrap();
        assert!(matches!(
            service.submit_attempt(&other, q, same, Some(attempt)).await,
            Err(QuizError::Http(409, _))
        ));
    }
}
