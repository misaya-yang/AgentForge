//! Execution event listing endpoint.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;

use super::{EventsQuery, HttpError, WorkerState, authorize, store_error, validate_execution_id};

pub(super) async fn get_events(
    State(state): State<WorkerState>,
    headers: HeaderMap,
    Path(execution_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<impl IntoResponse, HttpError> {
    let scope = authorize(&headers, &state)?;
    validate_execution_id(&execution_id)?;
    Ok(Json(
        state
            .store
            .events(&scope, &execution_id, query.after_sequence)
            .await
            .map_err(store_error)?,
    ))
}
