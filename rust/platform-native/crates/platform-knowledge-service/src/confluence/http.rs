use super::*;
use super::{
    bindings::CreateBinding,
    connections::{ControlConnection, CreateConnection, ReplaceCredentials},
};
use axum::{
    Extension,
    body::Bytes,
    extract::{Path, Query, State},
    routing::{get, post},
};
use query::types::UserContext;
use serde::Deserialize;
#[derive(Deserialize, Default)]
struct Offset {
    #[serde(default)]
    offset: i64,
}
impl Offset {
    fn checked(self) -> Result<i64> {
        if (0..=100000).contains(&self.offset) {
            Ok(self.offset)
        } else {
            Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid pagination offset",
            ))
        }
    }
}
#[derive(Deserialize)]
struct Space {
    space_key: String,
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &Bytes, message: &str) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| Error::new(StatusCode::UNPROCESSABLE_ENTITY, message))
}
pub fn routes(sources: Arc<Sources>, auth: Arc<query::auth::GatewayAuth>) -> Router {
    Router::new()
        .route("/api/v1/knowledge/{dataset_id}/sources/confluence/{binding_id}/delta",post(delta))
        .route("/api/v1/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync", post(start_sync))
        .route("/api/v1/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync/{request_id}", get(sync_status))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/connections",get(connections).post(create_connection))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/connections/manage",get(managed))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}",get(connection_status))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/space",get(space))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/credentials",post(reconnect))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/control",get(impact).patch(control))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/bindings",post(create_binding))
    .route("/api/v1/knowledge/{dataset_id}/sources/confluence/bindings/{binding_id}",get(binding_status))
    .with_state(sources)
    .layer(axum::middleware::from_fn_with_state(auth,query::auth::verify_gateway))
}
async fn connections(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path(d): Path<String>,
    Query(q): Query<Offset>,
) -> Result<Json<Value>> {
    Ok(Json(s.connections(&u, &d, q.checked()?).await?))
}
async fn managed(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path(d): Path<String>,
    Query(q): Query<Offset>,
) -> Result<Json<Value>> {
    Ok(Json(s.managed(&u, &d, q.checked()?).await?))
}
async fn connection_status(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, c)): Path<(String, String)>,
) -> Result<Json<Value>> {
    uuid::Uuid::parse_str(&c).map_err(|_| {
        Error::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid connection identity",
        )
    })?;
    Ok(Json(s.connection_status(&u, &d, &c).await?))
}
async fn binding_status(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, b)): Path<(String, String)>,
) -> Result<Json<Value>> {
    uuid::Uuid::parse_str(&b)
        .map_err(|_| Error::new(StatusCode::UNPROCESSABLE_ENTITY, "Invalid binding identity"))?;
    Ok(Json(s.binding_status(&u, &d, &b).await?))
}
async fn space(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, c)): Path<(String, String)>,
    Query(q): Query<Space>,
) -> Result<Json<Value>> {
    Ok(Json(s.space(&u, &d, &c, &q.space_key).await?))
}
async fn impact(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, c)): Path<(String, String)>,
    Query(q): Query<Offset>,
) -> Result<Json<Value>> {
    Ok(Json(s.impact(&u, &d, &c, q.checked()?).await?))
}
async fn create_connection(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path(d): Path<String>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    let result=async {let p:CreateConnection=parse(&bytes,"Invalid connection fields; use an Atlassian Cloud hostname, account email and API token")?;s.create_connection(&u,&d,p).await}.await;
    match result {
        Ok(v) => Ok((StatusCode::CREATED, Json(v))),
        Err(e) if e.detail == "Knowledge persistence unavailable" => Err(e),
        Err(e) => Err(Error {
            status: e.status,
            detail: json!({"code":"CONFLUENCE_CONNECTION_NOT_ACCEPTED","accepted":false,"message":e.detail,"reason":if e.status==StatusCode::SERVICE_UNAVAILABLE&&e.detail.as_str().unwrap_or("").to_lowercase().contains("encrypt"){Some("encryption_unavailable")}else{None}}),
        }),
    }
}
async fn create_binding(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path(d): Path<String>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    let p: CreateBinding = parse(&bytes, "Invalid binding fields")?;
    s.create_binding(&u,&d,p).await.map(|v|(StatusCode::CREATED,Json(v))).map_err(|e|if [400,401,403,404,409,422].contains(&e.status.as_u16()){Error{status:e.status,detail:json!({"code":"CONFLUENCE_BINDING_NOT_ACCEPTED","accepted":false,"message":"Binding was not accepted; verify access and source selection"})}}else{e})
}
async fn reconnect(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, c)): Path<(String, String)>,
    bytes: Bytes,
) -> Result<Json<Value>> {
    let p: ReplaceCredentials = parse(&bytes, "Invalid replacement credential fields")?;
    Ok(Json(s.reconnect(&u, &d, &c, p).await?))
}
async fn control(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, c)): Path<(String, String)>,
    bytes: Bytes,
) -> Result<Json<Value>> {
    let p: ControlConnection = parse(&bytes, "Invalid connection control fields")?;
    Ok(Json(s.control(&u, &d, &c, p).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncRequest {
    request_id: uuid::Uuid,
}
async fn start_sync(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, b)): Path<(String, String)>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    let p: SyncRequest = parse(&bytes, "Invalid sync request identity")?;
    s.start_sync(&u,&d,&b,&p.request_id.to_string()).await.map(|v|(StatusCode::ACCEPTED,Json(v))).map_err(|e|{
        if [400,401,403,404,409,422].contains(&e.status.as_u16()){Error{status:e.status,detail:json!({"code":"CONFLUENCE_SYNC_NOT_ACCEPTED","accepted":false,"message":"Synchronization request was not accepted; refresh source status"})}}else{e}
    })
}
async fn sync_status(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((d, b, r)): Path<(String, String, String)>,
) -> Result<Json<Value>> {
    let id = uuid::Uuid::parse_str(&r).map_err(|_| {
        Error::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid sync request identity",
        )
    })?;
    Ok(Json(s.sync_status(&u, &d, &b, &id.to_string()).await?))
}

async fn delta(
    State(s): State<Arc<Sources>>,
    Extension(u): Extension<UserContext>,
    Path((dataset, binding)): Path<(String, String)>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    let event: crate::connector::DeltaEvent = parse(&bytes, "Invalid connector delta event")?;
    Ok((
        StatusCode::ACCEPTED,
        Json(s.start_page_sync(&u, &dataset, &binding, &event).await?),
    ))
}
