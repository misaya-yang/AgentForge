//! Private workload routes, bounded body and exact raw-body authentication.
use super::*;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::stream;
use tokio::sync::mpsc;
pub fn router(plane: KnowledgeModelPlane) -> Router {
    Router::new()
        .route(wire::RESERVE_PATH, post(reserve))
        .route(
            "/internal/v2/knowledge/model-leases/{lease_id}/execute",
            post(execute),
        )
        .with_state(plane)
}
impl IntoResponse for PlaneError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            Json(json!({"code":self.code})),
        )
            .into_response()
    }
}
async fn reserve(
    State(plane): State<KnowledgeModelPlane>,
    request: Request,
) -> Result<Json<ModelLease>, PlaneError> {
    let (parts, body) = request.into_parts();
    if parts.uri.query().is_some() {
        return Err(PlaneError::new(422, "knowledge_model_query_forbidden"));
    }
    let bytes = to_bytes(body, 34 * 1024 * 1024)
        .await
        .map_err(|_| PlaneError::new(413, "knowledge_model_request_limit"))?;
    let request: ReserveRequest = serde_json::from_slice(&bytes)
        .map_err(|_| PlaneError::new(422, "knowledge_model_request_invalid"))?;
    plane
        .workload
        .verify(&parts.headers, parts.uri.path(), &bytes, &request.call)
        .await?;
    Ok(Json(plane.reserve(request).await?))
}
async fn execute(
    State(plane): State<KnowledgeModelPlane>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response, PlaneError> {
    let (parts, body) = request.into_parts();
    if parts.uri.query().is_some() {
        return Err(PlaneError::new(422, "knowledge_model_query_forbidden"));
    }
    let bytes = to_bytes(body, 34 * 1024 * 1024)
        .await
        .map_err(|_| PlaneError::new(413, "knowledge_model_request_limit"))?;
    let request: ExecuteRequest = serde_json::from_slice(&bytes)
        .map_err(|_| PlaneError::new(422, "knowledge_model_request_invalid"))?;
    plane
        .workload
        .verify(&parts.headers, parts.uri.path(), &bytes, &request.call)
        .await?;
    if !request.call.stream {
        return Ok(Json(plane.execute(&id, request).await?).into_response());
    }
    let (tx, rx) = mpsc::channel::<ModelStreamEvent>(16);
    tokio::spawn(async move {
        let result = plane.execute_events(&id, request, Some(tx.clone())).await;
        let event = match result {
            Ok(execution) => ModelStreamEvent::Receipt { execution },
            Err(error) => ModelStreamEvent::Error {
                code: error.code.into(),
            },
        };
        let _ = tx.send(event).await;
    });
    let body = Body::from_stream(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| {
            let data = serde_json::to_string(&event).expect("typed SSE event serializes");
            (
                Ok::<_, std::convert::Infallible>(format!("data: {data}\n\n")),
                rx,
            )
        })
    }));
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}
