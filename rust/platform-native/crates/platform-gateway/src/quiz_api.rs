//! Inert, unmounted canonical nine-route quiz API. Host supplies its existing
//! user-context resolver (including configured guests), current source adapter,
//! pools, trusted proxies, and middleware-counted dimensions. No startup I/O.
use crate::{
    artifact_shares_api::{ArtifactSharesIdentityResolver, parse_uuid},
    client_ip::TrustedProxies,
    quiz_rate::{QuizRateAdmission, request_context},
    quiz_scope::QuizScope,
    quiz_share_service::QuizShareService,
    quiz_store::QuizService,
    quiz_types::*,
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
#[derive(Clone)]
pub struct QuizApi {
    pub service: Option<Arc<QuizService>>,
    pub shares: Option<Arc<QuizShareService>>,
    pub scope: Arc<dyn QuizScope>,
    pub identity: Arc<dyn ArtifactSharesIdentityResolver>,
    pub rate: Option<Arc<QuizRateAdmission>>,
    pub trusted_proxies: TrustedProxies,
}
impl QuizApi {
    fn service(&self) -> Result<&QuizService, QuizError> {
        self.service
            .as_deref()
            .ok_or_else(|| QuizError::detail(503, "Database not available"))
    }
    fn shares(&self) -> Result<&QuizShareService, QuizError> {
        self.shares
            .as_deref()
            .ok_or_else(|| QuizError::detail(503, "Database not available"))
    }
}
pub const QUIZ_ROUTES: [(&str, &str); 9] = [
    ("GET", "/api/v1/assistant/quiz/{quiz_id}"),
    ("POST", "/api/v1/assistant/quiz/{quiz_id}/submit"),
    (
        "GET",
        "/api/v1/assistant/quiz/{quiz_id}/attempts/{attempt_id}",
    ),
    ("GET", "/api/v1/assistant/quiz/{quiz_id}/attempts"),
    ("DELETE", "/api/v1/assistant/quiz/{quiz_id}"),
    ("GET", "/api/v1/quiz/shared/{share_code}"),
    ("POST", "/api/v1/quiz/public/{share_code}/attempts/start"),
    ("POST", "/api/v1/quiz/public/{share_code}/attempts/result"),
    ("POST", "/api/v1/quiz/shared/{share_code}/submit"),
];
/// Trusted deployment origin set by the host, never constructed from forwarding
/// or Host headers. Without it, slash redirects remain same-origin relative URLs.
#[derive(Clone, Debug)]
pub struct QuizTrustedOrigin(String);
impl QuizTrustedOrigin {
    pub fn parse(origin: &str) -> Result<Self, QuizError> {
        let url = reqwest::Url::parse(origin).map_err(|_| QuizError::Internal)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(QuizError::Internal);
        }
        Ok(Self(url.origin().ascii_serialization()))
    }
}
async fn slash_redirect(request: Request) -> Response {
    let path = request.uri().path().trim_end_matches('/');
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let origin = request
        .extensions()
        .get::<QuizTrustedOrigin>()
        .map(|o| o.0.as_str())
        .unwrap_or("");
    let location = format!("{origin}{path}{query}");
    (StatusCode::TEMPORARY_REDIRECT, [("location", location)]).into_response()
}
pub fn quiz_router(api: QuizApi) -> Router {
    Router::new()
        .route("/api/v1/assistant/quiz/{quiz_id}", any(dispatch))
        .route("/api/v1/assistant/quiz/{quiz_id}/submit", any(dispatch))
        .route(
            "/api/v1/assistant/quiz/{quiz_id}/attempts/{attempt_id}",
            any(dispatch),
        )
        .route("/api/v1/assistant/quiz/{quiz_id}/attempts", any(dispatch))
        .route("/api/v1/quiz/shared/{share_code}", any(dispatch))
        .route(
            "/api/v1/quiz/public/{share_code}/attempts/start",
            any(dispatch),
        )
        .route(
            "/api/v1/quiz/public/{share_code}/attempts/result",
            any(dispatch),
        )
        .route("/api/v1/quiz/shared/{share_code}/submit", any(dispatch))
        .route("/api/v1/assistant/quiz/{quiz_id}/", any(slash_redirect))
        .route(
            "/api/v1/assistant/quiz/{quiz_id}/submit/",
            any(slash_redirect),
        )
        .route(
            "/api/v1/assistant/quiz/{quiz_id}/attempts/{attempt_id}/",
            any(slash_redirect),
        )
        .route(
            "/api/v1/assistant/quiz/{quiz_id}/attempts/",
            any(slash_redirect),
        )
        .route("/api/v1/quiz/shared/{share_code}/", any(slash_redirect))
        .route(
            "/api/v1/quiz/public/{share_code}/attempts/start/",
            any(slash_redirect),
        )
        .route(
            "/api/v1/quiz/public/{share_code}/attempts/result/",
            any(slash_redirect),
        )
        .route(
            "/api/v1/quiz/shared/{share_code}/submit/",
            any(slash_redirect),
        )
        .with_state(api)
}
async fn respond(result: Result<Value, QuizError>, internal: bool) -> Response {
    match result {
        Ok(v) => {
            let mut r = Json(v).into_response();
            if internal {
                r.headers_mut().insert(
                    "cache-control",
                    axum::http::HeaderValue::from_static("no-store"),
                );
            }
            r
        }
        Err(QuizError::Http(code, v)) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(v),
        )
            .into_response(),
        Err(QuizError::Internal) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":{"code":2000,"message":"Internal server error"}})),
        )
            .into_response(),
        Err(QuizError::Cancelled) => std::future::pending().await,
    }
}
fn method_error(allow: &'static str) -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [("allow", allow)],
        Json(json!({"detail":"Method Not Allowed"})),
    )
        .into_response()
}
fn parse_body(bytes: &[u8], headers: &HeaderMap) -> Result<Value, QuizError> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    if !crate::http_content_type::is_json_content_type(headers) {
        return Ok(Value::String(String::from_utf8_lossy(bytes).into_owned()));
    }
    serde_json::from_slice(bytes).map_err(|error|{let (offset,message)=if bytes==b"{"{(1,"Expecting property name enclosed in double quotes".into())}else{(error.column().saturating_sub(1),error.to_string())};QuizError::detail(422,json!([{"type":"json_invalid","loc":["body",offset],"msg":"JSON decode error","input":{},"ctx":{"error":message}}]))})
}
fn validated<T>(
    value: Result<T, QuizError>,
    errors: &mut Vec<Value>,
) -> Result<Option<T>, QuizError> {
    match value {
        Ok(v) => Ok(Some(v)),
        Err(QuizError::Http(422, body)) => {
            errors.extend(
                body["detail"]
                    .as_array()
                    .ok_or(QuizError::Internal)?
                    .iter()
                    .cloned(),
            );
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
async fn dispatch(
    State(api): State<QuizApi>,
    Path(paths): Path<HashMap<String, String>>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let auth = paths.contains_key("quiz_id");
    let submit = path.ends_with("/submit");
    let start = path.ends_with("/attempts/start");
    let result = path.ends_with("/attempts/result") || paths.contains_key("attempt_id");
    let list = auth && path.ends_with("/attempts");
    let allow = if submit || start || (!auth && result) {
        "POST"
    } else {
        "GET"
    };
    let delete = auth && !submit && !result && !list && method == Method::DELETE;
    if method.as_str() != allow && !delete {
        return method_error(allow);
    }
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let (ip, counted) = request_context(&headers, &parts.extensions, &api.trusted_proxies);
    let raw = if submit || (!auth && result) {
        match to_bytes(body, usize::MAX)
            .await
            .map_err(|_| QuizError::Internal)
            .and_then(|b| parse_body(&b, &headers))
        {
            Ok(v) => Some(v),
            Err(e) => return respond(Err(e), false).await,
        }
    } else {
        None
    };
    if auth {
        return respond(
            async {
                let user = api.identity.resolve(&headers).await?;
                let mut errors = vec![];
                let id = validated(
                    parse_uuid(&paths["quiz_id"], "path", "quiz_id"),
                    &mut errors,
                )?;
                let aid = if result {
                    validated(
                        parse_uuid(&paths["attempt_id"], "path", "attempt_id"),
                        &mut errors,
                    )?
                } else {
                    None
                };
                let page = if list {
                    validated(pagination(&query), &mut errors)?
                } else {
                    None
                };
                let body = if let Some(raw) = raw.as_ref() {
                    validated(QuizSubmit::parse(raw, false), &mut errors)?
                } else {
                    None
                };
                if !errors.is_empty() {
                    return Err(QuizError::detail(422, json!(errors)));
                }
                let id = id.ok_or(QuizError::Internal)?;
                if delete {
                    if !api.service()?.delete_quiz(&user, &id).await? {
                        return Err(QuizError::detail(
                            404,
                            "Quiz not found or not authorized to delete",
                        ));
                    }
                    return Ok(json!({"deleted":true}));
                }
                api.scope.authenticated(&user, &id).await?;
                let service = api.service()?;
                if let Some(body) = body {
                    project_attempt(
                        &service
                            .submit_attempt(&user, &id, body.answers, body.attempt_id.as_deref())
                            .await?,
                    )
                } else if let Some(aid) = aid {
                    project_attempt(
                        &service
                            .get_attempt_result(&user, &id, &aid)
                            .await?
                            .ok_or_else(|| QuizError::detail(404, "Attempt not found"))?,
                    )
                } else if let Some((limit, offset)) = page {
                    project_attempt_list(&service.list_attempts(&user, &id, limit, offset).await?)
                } else {
                    project_quiz(
                        &service
                            .get_quiz(&user, &id)
                            .await?
                            .ok_or_else(|| QuizError::detail(404, "Quiz not found"))?,
                        false,
                    )
                }
            }
            .await,
            false,
        )
        .await;
    }
    let parsed = if submit {
        match QuizSubmit::parse(raw.as_ref().unwrap(), true) {
            Ok(v) => Some(v),
            Err(e) => return respond(Err(e), false).await,
        }
    } else {
        None
    };
    let token = if result {
        match result_token(raw.as_ref().unwrap()) {
            Ok(v) => Some(v),
            Err(e) => return respond(Err(e), false).await,
        }
    } else {
        None
    };
    if (start || submit || result)
        && let Some(rate) = &api.rate
    {
        let operation = if start {
            "quiz_attempt_start_public"
        } else {
            "quiz_submit_public"
        };
        match rate.check(&ip, operation, &counted).await {
            Err(e) => return respond(Err(e), false).await,
            Ok(r) if !r.allowed => {
                let mut response = respond(
                    Err(QuizError::detail(
                        429,
                        format!("Rate limit exceeded ({operation})"),
                    )),
                    false,
                )
                .await;
                for (k, v) in [
                    ("x-ratelimit-limit", r.limit),
                    ("x-ratelimit-remaining", r.remaining),
                    ("retry-after", r.retry_after),
                ] {
                    if let Ok(v) = v.to_string().parse() {
                        response.headers_mut().insert(k, v);
                    }
                }
                return response;
            }
            _ => {}
        }
    }
    let code = &paths["share_code"];
    let audience = match api.scope.shared(&headers, code).await {
        Ok(v) => v,
        Err(e) => return respond(Err(e), false).await,
    };
    let internal = audience.audience == "internal";
    respond(
        async {
            let shares = api.shares()?;
            let a = audience.audience.as_str();
            let user = audience.user_id.as_deref();
            if start {
                project_start(&shares.start(code, a, user).await?)
            } else if let Some(token) = token {
                project_attempt(
                    &shares
                        .result(code, &token, a, user)
                        .await?
                        .ok_or_else(|| QuizError::detail(404, "Attempt not found"))?,
                )
            } else if let Some(body) = parsed {
                project_attempt(
                    &shares
                        .submit(
                            code,
                            body.answers,
                            body.display_name.as_deref(),
                            Some(&ip),
                            body.attempt_token.as_deref(),
                            a,
                            user,
                        )
                        .await?,
                )
            } else {
                project_quiz(
                    &shares.view(code, a).await?.ok_or_else(|| {
                        QuizError::detail(404, "Quiz not found, expired, or max attempts reached")
                    })?,
                    true,
                )
            }
        }
        .await,
        internal,
    )
    .await
}
