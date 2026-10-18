//! Quiz-only frozen-share lifecycle after the route's current audience/source
//! authorization. Never use this service to bypass that guard. Internal identity
//! binds token hashes; canonical shared attempts still persist user_id=NULL.
use crate::{
    quiz_entropy::{QuizEntropy, issue_token, sanitize_display_name, shuffle_options, token_hash},
    quiz_grading::{grade, python_trim},
    quiz_share_store::{NewQuizShareAttempt, NewQuizShareToken, QuizShareDatabase, share_error},
    quiz_types::QuizError,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use uuid::Uuid;

fn unavailable() -> QuizError {
    share_error(404, "share_unavailable", "Quiz not found or expired")
}
fn limit() -> QuizError {
    share_error(429, "attempt_limit_reached", "Maximum attempts reached")
}
fn input(message: &str) -> QuizError {
    share_error(400, "attempt_input_invalid", message)
}
fn conflict(message: &str) -> QuizError {
    share_error(409, "attempt_conflict", message)
}
fn decode(value: &Value) -> Result<Value, QuizError> {
    match value {
        Value::String(text) => serde_json::from_str(text).map_err(|_| QuizError::Internal),
        value => Ok(value.clone()),
    }
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn text<'a>(row: &'a Value, key: &str) -> Result<&'a str, QuizError> {
    row.get(key)
        .and_then(Value::as_str)
        .ok_or(QuizError::Internal)
}
fn expiry(row: &Value) -> Result<Option<DateTime<Utc>>, QuizError> {
    match row.get("expires_at") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => DateTime::parse_from_rfc3339(s)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| QuizError::Internal),
        _ => Err(QuizError::Internal),
    }
}
fn audience(row: &Value) -> &str {
    row.get("audience")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("public")
}
fn capped(row: &Value) -> bool {
    row.get("max_attempts")
        .and_then(Value::as_i64)
        .is_some_and(|max| {
            row.get("attempt_count")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                >= max
        })
}
fn saved(row: &Value, id_field: &str) -> Result<Option<Value>, QuizError> {
    let payload = decode(row.get("result_payload").unwrap_or(&Value::Null))?;
    let Some(payload) = payload.as_object() else {
        return Ok(None);
    };
    let mut response = Map::new();
    response.insert(
        "attempt_id".into(),
        Value::String(text(row, id_field)?.to_owned()),
    );
    // Canonical Python expansion allows stored payload to override the ID.
    response.extend(payload.clone());
    Ok(Some(Value::Object(response)))
}
fn question_id(v: Option<&Value>) -> String {
    crate::quiz_grading::python_str(v.unwrap_or(&Value::Null))
}
/// Grade answer-key records, including legacy keys missing type/text, rather
/// than all public questions. Neither current quiz content nor display order
/// affects snapshot grading.
pub fn grade_snapshot(share: &Value, answers: &Map<String, Value>) -> Result<Value, QuizError> {
    let keys = decode(share.get("answer_keys").unwrap_or(&Value::Null))?;
    let payload = decode(share.get("payload").unwrap_or(&Value::Null))?;
    let questions = payload.get("questions").and_then(Value::as_array);
    let mut by_id = std::collections::HashMap::new();
    for item in questions.into_iter().flatten().filter(|v| v.is_object()) {
        by_id.insert(question_id(item.get("id")), item);
    }
    let mut merged = Vec::new();
    // Python iterates `answer_keys or []`: falsey scalars yield no items;
    // dict keys and string characters are skipped as non-dict entries.
    // Truthy non-iterable scalars raise a source TypeError.
    let items = match &keys {
        Value::Array(items) => items.as_slice(),
        Value::Object(_) | Value::String(_) => &[][..],
        value if !truthy(value) => &[][..],
        _ => return Err(QuizError::Internal),
    };
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        // Source indexes both keys directly; absent keys raise KeyError.
        if !item.contains_key("id") || !item.contains_key("question_num") {
            return Err(QuizError::Internal);
        }
        let source = by_id.get(&question_id(item.get("id")));
        let mut item = item.clone();
        for (field, default) in [("question_type", "mc_single"), ("question_text", "")] {
            let value = item
                .get(field)
                .filter(|v| truthy(v))
                .cloned()
                .or_else(|| {
                    source
                        .and_then(|v| v.get(field))
                        .filter(|v| truthy(v))
                        .cloned()
                })
                .unwrap_or_else(|| Value::String(default.into()));
            item.insert(field.into(), value);
        }
        merged.push(Value::Object(item));
    }
    Ok(grade(&merged, answers))
}
#[derive(Clone)]
pub struct QuizShareService {
    pub database: Arc<dyn QuizShareDatabase>,
    entropy: Arc<dyn QuizEntropy>,
}
impl QuizShareService {
    /// Stores collaborators only; no database calls, tasks, entropy or I/O.
    pub fn new(database: Arc<dyn QuizShareDatabase>, entropy: Arc<dyn QuizEntropy>) -> Self {
        Self { database, entropy }
    }
    async fn active(
        &self,
        code: &str,
        expected_audience: &str,
    ) -> Result<Option<Value>, QuizError> {
        let Some(mut row) = self.database.share(code).await? else {
            return Ok(None);
        };
        if expiry(&row)?.is_some_and(|at| at < Utc::now()) || audience(&row) != expected_audience {
            return Ok(None);
        }
        let payload = decode(row.get("payload").unwrap_or(&Value::Null))?;
        row.as_object_mut()
            .ok_or(QuizError::Internal)?
            .insert("payload".into(), payload);
        Ok(Some(row))
    }
    pub async fn view(
        &self,
        code: &str,
        expected_audience: &str,
    ) -> Result<Option<Value>, QuizError> {
        let Some(row) = self.active(code, expected_audience).await? else {
            return Ok(None);
        };
        if capped(&row) {
            return Ok(None);
        }
        let mut payload = row
            .get("payload")
            .and_then(Value::as_object)
            .cloned()
            .ok_or(QuizError::Internal)?;
        for key in [
            "source_scope",
            "answer_keys",
            "audience",
            "tenant_id",
            "created_by",
        ] {
            payload.remove(key);
        }
        for key in [
            "share_code",
            "kind",
            "title",
            "require_name",
            "time_limit_minutes",
        ] {
            payload.insert(key.into(), row.get(key).cloned().unwrap_or(Value::Null));
        }
        payload.insert("audience".into(), Value::String(audience(&row).into()));
        let mut result = Value::Object(payload);
        shuffle_options(&mut result, self.entropy.as_ref())?;
        Ok(Some(result))
    }
    pub async fn start(
        &self,
        code: &str,
        expected_audience: &str,
        user: Option<&str>,
    ) -> Result<Value, QuizError> {
        let row = self
            .active(code, expected_audience)
            .await?
            .ok_or_else(unavailable)?;
        if capped(&row) {
            return Err(limit());
        }
        let started_at = DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
            .ok_or(QuizError::Internal)?;
        let minutes = row
            .get("time_limit_minutes")
            .and_then(Value::as_i64)
            .filter(|v| *v != 0)
            .unwrap_or(60);
        let ttl = Duration::try_minutes(minutes).ok_or(QuizError::Internal)?;
        let mut expires_at = started_at
            .checked_add_signed(ttl)
            .ok_or(QuizError::Internal)?;
        if let Some(at) = expiry(&row)? {
            expires_at = expires_at.min(at);
        }
        let token = issue_token(self.entropy.as_ref())?;
        let inserted = self
            .database
            .start_token(NewQuizShareToken {
                id: Uuid::new_v4().to_string(),
                share_code: code.into(),
                token_hash: token_hash(&token, expected_audience, user)?,
                started_at,
                expires_at,
            })
            .await?;
        if !inserted {
            if self
                .active(code, expected_audience)
                .await?
                .is_some_and(|row| capped(&row))
            {
                return Err(limit());
            }
            return Err(unavailable());
        }
        Ok(json!({"attempt_token":token,"started_at":started_at,"expires_at":expires_at}))
    }
    pub async fn result(
        &self,
        code: &str,
        token: &str,
        expected_audience: &str,
        user: Option<&str>,
    ) -> Result<Option<Value>, QuizError> {
        let row = self
            .active(code, expected_audience)
            .await?
            .ok_or_else(unavailable)?;
        if row.get("kind").and_then(Value::as_str) != Some("quiz") || token.is_empty() {
            return Err(input("Attempt token is required"));
        }
        let hash = token_hash(token, expected_audience, user)?;
        let Some(result) = self.database.result(text(&row, "id")?, &hash).await? else {
            return Ok(None);
        };
        saved(&result, "id")
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn submit(
        &self,
        code: &str,
        answers: Map<String, Value>,
        display_name: Option<&str>,
        client_ip: Option<&str>,
        token: Option<&str>,
        expected_audience: &str,
        user: Option<&str>,
    ) -> Result<Value, QuizError> {
        // Canonical ordering is observable: name validation precedes lookup.
        let display_name = sanitize_display_name(display_name)?;
        let row = self
            .active(code, expected_audience)
            .await?
            .ok_or_else(unavailable)?;
        if row.get("kind").and_then(Value::as_str) != Some("quiz") {
            return Err(input("Share is not a valid quiz"));
        }
        let token = token
            .filter(|token| !python_trim(token).is_empty())
            .ok_or_else(|| input("An attempt token is required"))?;
        let hash = token_hash(token, expected_audience, user)?;
        let answers = Value::Object(answers);
        if let Some(prior) = self.database.prior(text(&row, "id")?, &hash).await? {
            let old = decode(prior.get("answers").unwrap_or(&Value::Null))?;
            if old != answers
                || prior.get("display_name").unwrap_or(&Value::Null) != &json!(display_name)
            {
                return Err(conflict("Attempt token belongs to another answer"));
            }
            let mut result =
                saved(&prior, "id")?.ok_or_else(|| conflict("Attempt result is unavailable"))?;
            result
                .as_object_mut()
                .ok_or(QuizError::Internal)?
                .insert("cached".into(), Value::Bool(true));
            return Ok(result);
        }
        if capped(&row) {
            return Err(limit());
        }
        if row.get("require_name").and_then(Value::as_bool) == Some(true) && display_name.is_none()
        {
            return Err(input("This share requires a name before submitting"));
        }
        let graded = grade_snapshot(&row, answers.as_object().ok_or(QuizError::Internal)?)?;
        let quiz_id = row
            .get("payload")
            .and_then(|v| v.get("quiz_id"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| input("Shared quiz is missing its quiz id"))?;
        let quiz_id = Uuid::parse_str(quiz_id)
            .map_err(|_| QuizError::Internal)?
            .to_string();
        let recorded = self
            .database
            .record(NewQuizShareAttempt {
                share_code: text(&row, "share_code")?.into(),
                token_hash: hash,
                display_name,
                attempt_id: Uuid::new_v4().to_string(),
                quiz_id,
                answers,
                graded,
                client_ip: client_ip.map(str::to_owned),
            })
            .await?
            .ok_or_else(unavailable)?;
        saved(&recorded, "attempt_id")?.ok_or_else(|| conflict("Attempt result is unavailable"))
    }
}
