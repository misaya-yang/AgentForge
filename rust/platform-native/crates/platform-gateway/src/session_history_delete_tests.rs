use super::*;
use ai_platform_gateway_foundation::sessions_api::SessionsApiFuture;
use std::sync::Mutex;

struct Cleanup {
    deleted: Mutex<bool>,
    calls: Mutex<Vec<String>>,
    delete_result: Result<bool, SessionsApiError>,
    barrier_result: Result<bool, SessionsApiError>,
}
impl Cleanup {
    fn new(delete_result: Result<bool, SessionsApiError>, barrier_result: Result<bool, SessionsApiError>) -> Self {
        Self { deleted: Mutex::new(false), calls: Mutex::new(Vec::new()), delete_result, barrier_result }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
impl SessionsRuntimeCleanup for Cleanup {
    fn delete_owned_session<'a>(&'a self, tenant: &'a str, user: &'a str, session: &'a str) -> SessionsApiFuture<'a, bool> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("delete:{tenant}:{user}:{session}"));
            if (tenant, user, session) != ("tenant", "user", "session") || *self.deleted.lock().unwrap() {
                return Err(SessionsApiError::http(404, "session not found"));
            }
            if self.delete_result == Ok(true) {
                *self.deleted.lock().unwrap() = true;
            }
            self.delete_result.clone()
        })
    }
    fn session_deleted<'a>(&'a self, tenant: &'a str, user: &'a str, session: &'a str) -> SessionsApiFuture<'a, bool> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("barrier:{tenant}:{user}:{session}"));
            self.barrier_result.clone()
        })
    }
    fn cleanup_session<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str) -> SessionsApiFuture<'a, bool> {
        Box::pin(async { panic!("assistant DELETE must not perform a second/fallback cleanup") })
    }
}
async fn assert_response(response: Response, status: StatusCode, body: Value) {
    assert_eq!(response.status(), status);
    let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), body);
}
async fn delete_for(cleanup: Option<&dyn SessionsRuntimeCleanup>) -> Response {
    delete_owned_response(cleanup, "tenant", "user", "session").await
}

#[tokio::test]
async fn success_requires_durable_barrier_and_passes_exact_scope() {
    let cleanup = Cleanup::new(Ok(true), Ok(true));
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::OK,
        json!({"session_id":"session","status":"deleted"})).await;
    assert_eq!(cleanup.calls(), ["delete:tenant:user:session", "barrier:tenant:user:session"]);
}

#[tokio::test]
async fn missing_collaborator_cannot_delete() {
    assert_response(delete_for(None).await, StatusCode::SERVICE_UNAVAILABLE,
        json!({"detail":"Session deletion was not completed"})).await;
}

#[tokio::test]
async fn non_durable_collaborator_cannot_fall_back_or_acknowledge() {
    let cleanup = Cleanup::new(Ok(false), Ok(true));
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::SERVICE_UNAVAILABLE,
        json!({"detail":"Session deletion was not completed"})).await;
    assert_eq!(cleanup.calls(), ["delete:tenant:user:session"]);
}

#[tokio::test]
async fn missing_postcondition_cannot_acknowledge() {
    let cleanup = Cleanup::new(Ok(true), Ok(false));
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::SERVICE_UNAVAILABLE,
        json!({"detail":"Session deletion was not completed"})).await;
}

#[tokio::test]
async fn foreign_tenant_user_and_missing_session_are_indistinguishable() {
    for (tenant, user, session) in [("foreign", "user", "session"), ("tenant", "foreign", "session"), ("tenant", "user", "missing")] {
        let cleanup = Cleanup::new(Ok(true), Ok(true));
        assert_response(delete_owned_response(Some(&cleanup), tenant, user, session).await,
            StatusCode::NOT_FOUND, json!({"detail":"Session not found"})).await;
        assert!(!*cleanup.deleted.lock().unwrap());
        assert_eq!(cleanup.calls().len(), 1);
    }
}

#[tokio::test]
async fn retry_after_success_is_not_found_and_does_not_repeat_cleanup() {
    let cleanup = Cleanup::new(Ok(true), Ok(true));
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::OK,
        json!({"session_id":"session","status":"deleted"})).await;
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::NOT_FOUND,
        json!({"detail":"Session not found"})).await;
    assert_eq!(cleanup.calls(), ["delete:tenant:user:session", "barrier:tenant:user:session", "delete:tenant:user:session"]);
}

#[tokio::test]
async fn cleanup_errors_are_sanitized_and_do_not_read_postcondition() {
    for failure in [SessionsApiError::Internal, SessionsApiError::http(503, "sdk_session_cleanup_unavailable"), SessionsApiError::http(409, "runtime_session_cleanup_failed")] {
        let cleanup = Cleanup::new(Err(failure), Ok(true));
        assert_response(delete_for(Some(&cleanup)).await, StatusCode::INTERNAL_SERVER_ERROR,
            json!({"detail":"Failed to delete session"})).await;
        assert_eq!(cleanup.calls(), ["delete:tenant:user:session"]);
        assert!(!*cleanup.deleted.lock().unwrap());
    }
}

#[tokio::test]
async fn postcommit_read_error_is_not_success_and_retry_does_not_recreate() {
    let cleanup = Cleanup::new(Ok(true), Err(SessionsApiError::http(503, "private database error")));
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::INTERNAL_SERVER_ERROR,
        json!({"detail":"Failed to delete session"})).await;
    assert!(*cleanup.deleted.lock().unwrap());
    assert_response(delete_for(Some(&cleanup)).await, StatusCode::NOT_FOUND,
        json!({"detail":"Session not found"})).await;
}
