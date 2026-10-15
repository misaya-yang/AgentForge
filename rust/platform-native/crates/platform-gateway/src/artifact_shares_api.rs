//! Complete, unmounted artifact_shares.py route library. No public viewer or
//! attempt route is implemented here. Host owns canonical identity resolution,
//! secure random bytes, transport/body limits, trusted-origin trailing-slash redirects,
//! exact malformed-JSON parser wording beyond the frozen cases, and shutdown drain.
use crate::{
    artifact_share_scope::ArtifactShareScope,
    artifact_shares_store::{ArtifactSharesDatabase, NewArtifactShare},
    rate_config_api::{boolean, integer},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use serde_json::{Value, json};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub type ArtifactSharesFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ArtifactSharesError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq)]
pub enum ArtifactSharesError {
    Http(u16, Value),
    Internal,
    Cancelled,
}
impl ArtifactSharesError {
    pub fn detail(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http(status, json!({"detail":detail.into()}))
    }
    pub fn validation(field: &str, typ: &str, message: &str, input: Value) -> Self {
        Self::detail(
            422,
            json!([{"type":typ,"loc":["body",field],"msg":message,"input":input}]),
        )
    }
}
#[derive(Clone, Default)]
pub struct ArtifactSharesIdentity {
    pub user_id: String,
    pub tenant_id: Option<String>,
    pub is_authenticated: bool,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub tier: String,
}
pub trait ArtifactSharesIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
    ) -> ArtifactSharesFuture<'a, ArtifactSharesIdentity>;
}
/// Must supply independent cryptographically secure random bytes. No weak/default
/// implementation, UUID substitute, clock seed, retries, or hidden provider calls.
pub trait ArtifactSharesRandom: Send + Sync {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), ArtifactSharesError>;
}
pub trait ArtifactSharesClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
pub struct ArtifactSharesSystemClock;
impl ArtifactSharesClock for ArtifactSharesSystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
#[derive(Clone, Debug)]
pub struct ArtifactShareCreate {
    pub kind: String,
    pub quiz_id: Option<String>,
    pub expires_hours: Option<i64>,
    pub max_attempts: Option<i64>,
    pub require_name: bool,
    pub time_limit_minutes: Option<i64>,
    pub audience: String,
}
impl ArtifactShareCreate {
    pub fn parse(value: &Value) -> Result<Self, ArtifactSharesError> {
        if value.is_null() {
            return Err(ArtifactSharesError::detail(
                422,
                json!([{"type":"missing","loc":["body"],"msg":"Field required","input":null}]),
            ));
        }
        if !value.is_object() {
            return Err(ArtifactSharesError::detail(
                422,
                json!([{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":value}]),
            ));
        }
        let mut errors = Vec::new();
        let kind = match value.get("kind") {
            None => "quiz".into(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => {
                errors.push(json!({"type":"string_type","loc":["body","kind"],"msg":"Input should be a valid string","input":v}));
                String::new()
            }
        };
        let quiz_id = match value.get("quiz_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => match parse_uuid(s, "body", "quiz_id") {
                Ok(s) => Some(s),
                Err(ArtifactSharesError::Http(_, v)) => {
                    errors.extend(v["detail"].as_array().unwrap().iter().cloned());
                    None
                }
                Err(e) => return Err(e),
            },
            Some(v) => {
                errors.push(json!({"type":"uuid_type","loc":["body","quiz_id"],"msg":"UUID input should be a string, bytes or UUID object","input":v}));
                None
            }
        };
        fn positive(value: &Value, name: &str, errors: &mut Vec<Value>) -> Option<i64> {
            match value.get(name) {
                None | Some(Value::Null) => None,
                Some(v) => match integer(v) {
                    Ok(n) if n >= 1 => Some(n),
                    Ok(_) => {
                        errors.push(json!({"type":"greater_than_equal","loc":["body",name],"msg":"Input should be greater than or equal to 1","input":v,"ctx":{"ge":1}}));
                        None
                    }
                    Err(typ) => {
                        errors.push(json!({"type":typ,"loc":["body",name],"msg":validation_message(typ),"input":v}));
                        None
                    }
                },
            }
        }
        let expires_hours = positive(value, "expires_hours", &mut errors);
        let max_attempts = positive(value, "max_attempts", &mut errors);
        let require_name = match value.get("require_name") {
            None => true,
            Some(v) => match boolean(v) {
                Ok(v) => v,
                Err(typ) => {
                    errors.push(json!({"type":typ,"loc":["body","require_name"],"msg":validation_message(typ),"input":v}));
                    false
                }
            },
        };
        let time_limit_minutes = positive(value, "time_limit_minutes", &mut errors);
        let audience = match value.get("audience") {
            None => "public".into(),
            Some(Value::String(s)) if s == "public" || s == "internal" => s.clone(),
            Some(v) => {
                errors.push(json!({"type":"literal_error","loc":["body","audience"],"msg":"Input should be 'public' or 'internal'","input":v,"ctx":{"expected":"'public' or 'internal'"}}));
                String::new()
            }
        };
        if !errors.is_empty() {
            return Err(ArtifactSharesError::detail(422, json!(errors)));
        }
        Ok(Self {
            kind,
            quiz_id,
            expires_hours,
            max_attempts,
            require_name,
            time_limit_minutes,
            audience,
        })
    }
}
fn validation_message(typ: &str) -> &'static str {
    match typ {
        "int_parsing" => "Input should be a valid integer, unable to parse string as an integer",
        "int_from_float" => "Input should be a valid integer, got a number with a fractional part",
        "bool_parsing" => "Input should be a valid boolean, unable to interpret input",
        "bool_type" => "Input should be a valid boolean",
        _ => "Input should be a valid integer",
    }
}
pub fn parse_uuid(text: &str, location: &str, name: &str) -> Result<String, ArtifactSharesError> {
    if let Ok(id) = uuid::Uuid::parse_str(text) {
        return Ok(id.to_string());
    }
    let raw = text.strip_prefix("urn:uuid:").unwrap_or(text);
    let raw = raw
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(raw);
    let error = if raw.len() != 32 && !raw.contains('-') {
        format!(
            "invalid length: expected length 32 for simple format, found {}",
            raw.len()
        )
    } else if let Some((i, c)) = raw
        .chars()
        .enumerate()
        .find(|(_, c)| !c.is_ascii_hexdigit() && *c != '-')
    {
        format!("invalid character: found `{c}` at {}", i + 1)
    } else {
        let groups = raw.split('-').collect::<Vec<_>>();
        if groups.len() != 5 {
            format!("invalid group count: expected 5, found {}", groups.len())
        } else {
            let expected = [8, 4, 4, 4, 12];
            let (i, g) = groups
                .iter()
                .enumerate()
                .find(|(i, g)| g.len() != expected[*i])
                .unwrap_or((0, &""));
            format!(
                "invalid group length in group {}: expected {}, found {}",
                i,
                expected[i],
                g.len()
            )
        }
    };
    Err(ArtifactSharesError::detail(
        422,
        json!([{"type":"uuid_parsing","loc":[location,name],"msg":format!("Input should be a valid UUID, {error}"),"input":text,"ctx":{"error":error}}]),
    ))
}
fn list_query(query: &HashMap<String, String>) -> Result<(String, i64), ArtifactSharesError> {
    let mut errors = vec![];
    let id = match query.get("quiz_id") {
        None => {
            errors.push(json!({"type":"missing","loc":["query","quiz_id"],"msg":"Field required","input":null}));
            String::new()
        }
        Some(raw) => match parse_uuid(raw, "query", "quiz_id") {
            Ok(v) => v,
            Err(ArtifactSharesError::Http(_, body)) => {
                errors.extend(body["detail"].as_array().unwrap().iter().cloned());
                String::new()
            }
            Err(e) => return Err(e),
        },
    };
    let input = query.get("limit").map(|s| json!(s)).unwrap_or(json!(50));
    let limit = match integer(&input) {
        Ok(n) => {
            if !(1..=200).contains(&n) {
                let (typ, message, ctx) = if n < 1 {
                    (
                        "greater_than_equal",
                        "Input should be greater than or equal to 1",
                        json!({"ge":1}),
                    )
                } else {
                    (
                        "less_than_equal",
                        "Input should be less than or equal to 200",
                        json!({"le":200}),
                    )
                };
                errors.push(json!({"type":typ,"loc":["query","limit"],"msg":message,"input":input,"ctx":ctx}));
            }
            n
        }
        Err(typ) => {
            errors.push(json!({"type":typ,"loc":["query","limit"],"msg":validation_message(typ),"input":input}));
            50
        }
    };
    if errors.is_empty() {
        Ok((id, limit))
    } else {
        Err(ArtifactSharesError::detail(422, json!(errors)))
    }
}
/// The route first decodes a string JSON column, then the safe helper may decode
/// one more nested string. Only label/text survive; answer-bearing fields never do.
pub fn safe_options(raw: &Value) -> Value {
    let parsed;
    if let Value::String(s) = raw {
        parsed = serde_json::from_str(s).unwrap_or(Value::Null);
    } else {
        parsed = raw.clone();
    }
    json!(
        parsed
            .as_array()
            .map(|items| items
                .iter()
                .filter_map(|item| Some(
                    json!({"label":item.get("label")?.as_str()?,"text":item.get("text")?.as_str()?})
                ))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    )
}
fn decoded(value: &Value) -> Result<Value, ArtifactSharesError> {
    match value {
        Value::String(s) => serde_json::from_str(s).map_err(|_| ArtifactSharesError::Internal),
        v => Ok(v.clone()),
    }
}
#[derive(Clone)]
pub struct ArtifactSharesApi {
    pub database: Option<Arc<dyn ArtifactSharesDatabase>>,
    pub scope: Arc<dyn ArtifactShareScope>,
    pub random: Arc<dyn ArtifactSharesRandom>,
    pub clock: Arc<dyn ArtifactSharesClock>,
}
impl ArtifactSharesApi {
    fn db(&self) -> Result<&Arc<dyn ArtifactSharesDatabase>, ArtifactSharesError> {
        self.database
            .as_ref()
            .ok_or_else(|| ArtifactSharesError::detail(503, "Database not available"))
    }
    pub async fn create(
        &self,
        user: &ArtifactSharesIdentity,
        body: ArtifactShareCreate,
    ) -> Result<Value, ArtifactSharesError> {
        let db = self.db()?;
        if body.kind != "quiz" {
            return Err(ArtifactSharesError::detail(
                400,
                format!("Unsupported artifact kind: {}", body.kind),
            ));
        }
        let id = body.quiz_id.as_deref().ok_or_else(|| {
            ArtifactSharesError::detail(400, "quiz_id is required for kind='quiz'")
        })?;
        let quiz = db
            .owned_quiz(user, id)
            .await?
            .ok_or_else(|| ArtifactSharesError::detail(404, "Quiz not found or not authorized"))?;
        let references = self.scope.quiz_sources(user, id).await?;
        let source_scope = if body.audience == "public" {
            if !references.datasets.is_empty()
                || !references.documents.is_empty()
                || !references.versions.is_empty()
            {
                return Err(ArtifactSharesError::detail(
                    409,
                    "Private knowledge content cannot be shared anonymously",
                ));
            }
            None
        } else {
            self.scope.require_active(user).await?;
            let frozen = crate::artifact_share_scope::freeze_source_scope(&references)?;
            if !self.scope.allowed(user, &references).await? {
                return Err(ArtifactSharesError::detail(
                    403,
                    "Quiz source access has been revoked",
                ));
            }
            Some(frozen)
        };
        let rows = db.questions(id).await?;
        let mut questions = vec![];
        let mut keys = vec![];
        for row in rows {
            let options = decoded(&row["options"])?;
            let correct = decoded(&row["correct_answer"])?;
            questions.push(json!({"id":row["id"],"question_num":row["question_num"],"question_type":row["question_type"],"question_text":row["question_text"],"options":safe_options(&options)}));
            keys.push(json!({"id":row["id"],"question_num":row["question_num"],"question_type":row["question_type"],"correct_answer":correct,"explanation":row["explanation"]}));
        }
        let payload = json!({"quiz_id":id,"description":quiz["description"],"question_count":quiz["question_count"],"difficulty":quiz["difficulty"],"questions":questions});
        let mut bytes = [0u8; 12];
        self.random.fill(&mut bytes)?;
        let share_code = URL_SAFE_NO_PAD.encode(bytes)[..12].to_owned();
        let mut bytes = [0u8; 16];
        self.random.fill(&mut bytes)?;
        let share_id = uuid::Builder::from_random_bytes(bytes)
            .into_uuid()
            .to_string();
        let now = self.clock.now();
        let now = now
            .with_nanosecond(now.nanosecond() / 1000 * 1000)
            .ok_or(ArtifactSharesError::Internal)?;
        let expires_at = body
            .expires_hours
            .map(|hours| {
                Duration::try_hours(hours)
                    .and_then(|duration| now.checked_add_signed(duration))
                    .filter(|date| date.year() <= 9999)
                    .ok_or(ArtifactSharesError::Internal)
            })
            .transpose()?;
        let share = NewArtifactShare {
            share_id: share_id.clone(),
            share_code: share_code.clone(),
            title: quiz["title"]
                .as_str()
                .ok_or(ArtifactSharesError::Internal)?
                .to_owned(),
            payload,
            answer_keys: json!(keys),
            tenant_id: user.tenant_id.clone(),
            created_by: user.user_id.clone(),
            created_at: now,
            expires_at,
            max_attempts: body.max_attempts,
            require_name: body.require_name,
            time_limit_minutes: body.time_limit_minutes,
            audience: body.audience.clone(),
            source_scope,
        };
        db.insert(share).await?;
        Ok(
            json!({"share_id":share_id,"share_code":share_code,"kind":"quiz","title":quiz["title"],"expires_at":expires_at,"require_name":body.require_name,"max_attempts":body.max_attempts,"time_limit_minutes":body.time_limit_minutes,"quiz_id":id,"quiz_title":quiz["title"],"audience":body.audience}),
        )
    }
    pub async fn list(
        &self,
        user: &ArtifactSharesIdentity,
        quiz_id: &str,
        limit: i64,
    ) -> Result<Value, ArtifactSharesError> {
        let mut out = vec![];
        for row in self.db()?.list(user, quiz_id, limit).await? {
            let mut summary = serde_json::Map::new();
            for field in [
                "share_id",
                "share_code",
                "is_active",
                "expired",
                "created_at",
                "expires_at",
                "revoked_at",
                "require_name",
            ] {
                summary.insert(
                    field.into(),
                    row.get(field).ok_or(ArtifactSharesError::Internal)?.clone(),
                );
            }
            let audience = row.get("audience").cloned().unwrap_or(json!("public"));
            if audience != "public" && audience != "internal" {
                return Err(ArtifactSharesError::Internal);
            }
            summary.insert("audience".into(), audience);
            out.push(Value::Object(summary));
        }
        Ok(json!(out))
    }
    pub async fn revoke(
        &self,
        user: &ArtifactSharesIdentity,
        id: &str,
    ) -> Result<Value, ArtifactSharesError> {
        if !self.db()?.revoke(user, id).await? {
            return Err(ArtifactSharesError::detail(
                404,
                "Share link not found or not authorized",
            ));
        }
        Ok(json!({"revoked":true}))
    }
}
#[derive(Clone)]
struct HttpState {
    api: ArtifactSharesApi,
    identity: Arc<dyn ArtifactSharesIdentityResolver>,
}
pub fn artifact_shares_router(
    api: ArtifactSharesApi,
    identity: Arc<dyn ArtifactSharesIdentityResolver>,
) -> Router {
    Router::new()
        .route("/api/v1/artifact-shares", any(collection))
        .route("/api/v1/artifact-shares/{share_id}", any(item))
        .with_state(HttpState { api, identity })
}
async fn respond(result: Result<Value, ArtifactSharesError>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(ArtifactSharesError::Http(code, v)) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(v),
        )
            .into_response(),
        Err(ArtifactSharesError::Internal) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":{"code":2000,"message":"Internal server error"}})),
        )
            .into_response(),
        Err(ArtifactSharesError::Cancelled) => std::future::pending().await,
    }
}
async fn collection(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    bytes: Bytes,
) -> Response {
    if method != Method::GET && method != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let result=async{let raw=if method==Method::POST {Some(if bytes.is_empty(){Value::Null}else if !crate::http_content_type::is_json_content_type(&headers){Value::String(String::from_utf8(bytes.to_vec()).map_err(|_|ArtifactSharesError::Internal)?)}else{serde_json::from_slice::<Value>(&bytes).map_err(|error|{let (offset,message)=if bytes.as_ref()==b"{" {(1,"Expecting property name enclosed in double quotes".to_owned())}else{(error.column().saturating_sub(1),error.to_string())};ArtifactSharesError::detail(422,json!([{"type":"json_invalid","loc":["body",offset],"msg":"JSON decode error","input":{},"ctx":{"error":message}}]))})?})}else{None};let user=state.identity.resolve(&headers).await?;
 if let Some(raw)=raw {state.api.create(&user,ArtifactShareCreate::parse(&raw)?).await} else {let (id,limit)=list_query(&query)?;state.api.list(&user,&id,limit).await}}.await;
    respond(result).await
}
async fn item(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if method != Method::DELETE {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "DELETE")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    respond(
        async {
            let user = state.identity.resolve(&headers).await?;
            let id = parse_uuid(&id, "path", "share_id")?;
            state.api.revoke(&user, &id).await
        }
        .await,
    )
    .await
}
