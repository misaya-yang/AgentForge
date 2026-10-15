use ai_platform_gateway_foundation::connector_user_service::oauth_console_redirect;

#[test]
fn oauth_return_uses_personal_connections_and_explicit_console_origin() {
    assert_eq!(
        oauth_console_redirect("https://console.example/", "google drive").unwrap(),
        "https://console.example/connections?connected=google%20drive&status=connected"
    );
}

#[test]
fn oauth_return_stays_relative_without_a_valid_console_origin() {
    for origin in [
        "",
        "https://user:secret@example.com/path",
        "https://console.example/?next=other",
    ] {
        assert_eq!(
            oauth_console_redirect(origin, "github").unwrap(),
            "/connections?connected=github&status=connected"
        );
    }
}

use ai_platform_gateway_foundation::{
    connector_user_api::{
        ConnectorUserApi, ConnectorUserDependencies, ConnectorUserIdentityResolver, callback_router,
    },
    connector_user_service::{
        ConnectorService, ConnectorServiceDependencies, SystemConnectorClock,
    },
    connector_user_types::*,
    mcp_registry_api::{McpRegistryIdentity, McpRegistryIdentityResolver},
    service_config_api::{
        ServiceConfigCancellation, ServiceConfigCancellationFuture, ServiceConfigFuture,
    },
};
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct Forbidden;
impl ConnectorNonce for Forbidden {
    fn nonce(&self) -> Result<String, ConnectorError> {
        panic!("nonce")
    }
}
impl ConnectorTokenCipher for Forbidden {
    fn decrypt_config(&self, _: &str) -> Result<String, ConnectorError> {
        panic!("config secret")
    }
    fn encrypt_user(&self, _: Option<&str>) -> Result<Option<String>, ConnectorError> {
        panic!("token write")
    }
    fn decrypt_user(&self, _: Option<&str>) -> Result<Option<String>, ConnectorError> {
        panic!("token read")
    }
}
impl OAuthFormHttp for Forbidden {
    fn post(&self, _: OAuthFormRequest) -> ConnectorHttpFuture<'_> {
        panic!("exchange")
    }
}
impl ConnectorProviderHttp for Forbidden {
    fn get(&self, _: ConnectorGetRequest) -> ConnectorHttpFuture<'_> {
        panic!("provider")
    }
}
impl ConnectorSearchProviders for Forbidden {
    fn search<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a Value,
        _: &'a str,
        _: i64,
    ) -> ConnectorFuture<'a, Value> {
        panic!("search")
    }
}
impl ConnectorActivation for Forbidden {
    fn start_confluence<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> ConnectorFuture<'a, Vec<Value>> {
        panic!("activate")
    }
    fn stop<'a>(&'a self, _: &'a str, _: &'a str) -> ConnectorFuture<'a, ()> {
        panic!("stop")
    }
    fn status(&self, _: &str, _: &str) -> Result<(bool, Vec<Value>), ConnectorError> {
        panic!("status")
    }
}
impl ConnectorUserIdentityResolver for Forbidden {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> ServiceConfigFuture<'a, ConnectorUser> {
        panic!("bearer")
    }
}
impl McpRegistryIdentityResolver for Forbidden {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> ServiceConfigFuture<'a, McpRegistryIdentity> {
        panic!("principal")
    }
}
impl ServiceConfigCancellation for Forbidden {
    fn cancelled(&self) -> ServiceConfigCancellationFuture<'_> {
        Box::pin(std::future::pending())
    }
}
struct StateStore(Mutex<Option<Value>>);
impl OAuthStateStore for StateStore {
    fn save<'a>(&'a self, _: &'a str, _: Value, _: u64) -> ConnectorFuture<'a, ()> {
        panic!("new state")
    }
    fn consume<'a>(&'a self, key: &'a str) -> ConnectorFuture<'a, Option<OAuthStateRaw>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if state
                .as_ref()
                .is_some_and(|v| v.get("fail_consume").is_some())
            {
                return Err(ConnectorError::storage());
            }
            Ok(if key == "oauth:state:tenant:user:github:nonce" {
                state.take().map(OAuthStateRaw::Json)
            } else {
                None
            })
        })
    }
}
fn issued() -> Value {
    json!({"tenant_id":"tenant","user_id":"user","provider":"github","nonce":"nonce","expires_at":(Utc::now()+Duration::minutes(5)).to_rfc3339()})
}
fn mounted(state: Option<Value>, origin: &str) -> axum::Router {
    let no = Arc::new(Forbidden);
    let service = ConnectorService::new(ConnectorServiceDependencies {
        store: None,
        state: Some(Arc::new(StateStore(Mutex::new(state)))),
        clock: Arc::new(SystemConnectorClock),
        nonce: no.clone(),
        cipher: no.clone(),
        form_http: no.clone(),
        provider_http: no.clone(),
        search: no.clone(),
        activation: no.clone(),
        console_public_base_url: origin.into(),
    });
    callback_router(
        Arc::new(ConnectorUserApi::new(ConnectorUserDependencies {
            service: Arc::new(service),
            user_identity: no.clone(),
            principal_identity: no.clone(),
            rbac: Default::default(),
            principal_enabled: false,
            principals: None,
            audit: None,
        })),
        no,
    )
}
async fn request(router: axum::Router, query: &str) -> axum::response::Response {
    router
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/connectors/callback/github?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn mounted_denial_returns_fixed_hint_without_credentials_and_rejects_replay() {
    for (origin, expected) in [
        ("https://console.example/", "https://console.example"),
        ("", ""),
        ("https://user:secret@example.com/path", ""),
    ] {
        let router = mounted(Some(issued()), origin);
        let query = "state=tenant:user:github:nonce&error=access_denied-SECRET&error_description=SECRET&error_uri=https://evil.example/SECRET";
        let response = request(router.clone(), query).await;
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers()["location"],
            format!("{expected}/connections?status=authorization_failed")
        );
        let replay = request(router, query).await;
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        assert!(!replay.headers().contains_key("location"));
    }
}
#[tokio::test]
async fn mounted_malformed_denials_preserve_http_errors() {
    for (query, status) in [
        ("", 422),
        ("state=tenant:user:github:nonce", 422),
        ("error=", 400),
        ("code=code&error=access_denied", 400),
        ("code=&error=access_denied", 400),
        ("error=access_denied&state=bad", 400),
        ("error=access_denied&state=tenant:user:other:nonce", 400),
        ("error=access_denied&state=tenant:user:github:unissued", 400),
    ] {
        let response = request(mounted(Some(issued()), ""), query).await;
        assert_eq!(response.status().as_u16(), status, "{query}");
        assert!(!response.headers().contains_key("location"));
    }
}
#[tokio::test]
async fn mounted_expired_and_binding_mismatch_denials_do_not_redirect() {
    for (field, value) in [
        ("tenant_id", "other"),
        ("user_id", "other"),
        ("provider", "other"),
        ("nonce", "other"),
        ("expires_at", "2000-01-01T00:00:00+00:00"),
    ] {
        let mut state = issued();
        state[field] = json!(value);
        let response = request(
            mounted(Some(state), ""),
            "state=tenant:user:github:nonce&error=access_denied",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!response.headers().contains_key("location"));
    }
}
#[tokio::test]
async fn mounted_valid_success_reaches_existing_database_failure_without_redirect() {
    let response = request(
        mounted(Some(issued()), ""),
        "state=tenant:user:github:nonce&code=code",
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!response.headers().contains_key("location"));
}

struct Success(Mutex<Vec<&'static str>>, u16);
impl ConnectorUserStore for Success {
    fn run(&self, operation: ConnectorUserOperation) -> ConnectorFuture<'_, Vec<ConnectorRecord>> {
        Box::pin(async move {
            match operation {
                ConnectorUserOperation::EffectiveEnabledConfig {
                    provider,
                    tenant_id,
                } => {
                    assert_eq!(provider, "github");
                    assert_eq!(tenant_id, "tenant");
                    self.0.lock().unwrap().push("config");
                    Ok(vec![ConnectorRecord(
                        json!({"client_id":"fixture-client","token_url":"https://fixture.example/token","scopes":"read"}),
                    )])
                }
                ConnectorUserOperation::UpsertTokens(write) => {
                    assert_eq!(write.user.tenant_id, "tenant");
                    assert_eq!(write.user.user_id, "user");
                    assert_eq!(write.provider, "github");
                    self.0.lock().unwrap().push("save");
                    Ok(vec![])
                }
                _ => panic!("unexpected store operation"),
            }
        })
    }
}
impl OAuthFormHttp for Success {
    fn post(&self, request: OAuthFormRequest) -> ConnectorHttpFuture<'_> {
        assert!(request.fields.contains(&("code".into(), "code".into())));
        self.0.lock().unwrap().push("exchange");
        Box::pin(async move {
            Ok(ConnectorHttpResponse {
                status: self.1,
                body: br#"{"access_token":"fixture-access","expires_in":3600}"#.to_vec(),
            })
        })
    }
}
impl ConnectorTokenCipher for Success {
    fn decrypt_config(&self, _: &str) -> Result<String, ConnectorError> {
        panic!("unexpected encrypted config")
    }
    fn encrypt_user(&self, value: Option<&str>) -> Result<Option<String>, ConnectorError> {
        Ok(value.map(str::to_string))
    }
    fn decrypt_user(&self, _: Option<&str>) -> Result<Option<String>, ConnectorError> {
        panic!("unexpected existing token")
    }
}
#[tokio::test]
async fn mounted_success_still_exchanges_once_and_saves_issued_identity() {
    for status in [200, 503] {
        let no = Arc::new(Forbidden);
        let success = Arc::new(Success(Mutex::new(vec![]), status));
        let service = ConnectorService::new(ConnectorServiceDependencies {
            store: Some(success.clone()),
            state: Some(Arc::new(StateStore(Mutex::new(Some(issued()))))),
            clock: Arc::new(SystemConnectorClock),
            nonce: no.clone(),
            cipher: success.clone(),
            form_http: success.clone(),
            provider_http: no.clone(),
            search: no.clone(),
            activation: no.clone(),
            console_public_base_url: String::new(),
        });
        let router = callback_router(
            Arc::new(ConnectorUserApi::new(ConnectorUserDependencies {
                service: Arc::new(service),
                user_identity: no.clone(),
                principal_identity: no.clone(),
                rbac: Default::default(),
                principal_enabled: false,
                principals: None,
                audit: None,
            })),
            no,
        );
        let response = request(router, "state=tenant:user:github:nonce&code=code").await;
        if status == 200 {
            assert_eq!(response.status(), StatusCode::FOUND);
            assert_eq!(
                response.headers()["location"],
                "/connections?connected=github&status=connected"
            );
            assert_eq!(
                *success.0.lock().unwrap(),
                vec!["config", "exchange", "save"]
            );
        } else {
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(!response.headers().contains_key("location"));
            assert_eq!(*success.0.lock().unwrap(), vec!["config", "exchange"]);
        }
    }
}

#[tokio::test]
async fn mounted_denial_storage_failures_remain_unavailable() {
    for field in ["fail_consume"] {
        let mut state = issued();
        state[field] = json!(true);
        let response = request(
            mounted(Some(state), ""),
            "state=tenant:user:github:nonce&error=access_denied",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key("location"));
    }
}

struct ContendedState {
    barrier: tokio::sync::Barrier,
    state: StateStore,
}
impl OAuthStateStore for ContendedState {
    fn save<'a>(&'a self, _: &'a str, _: Value, _: u64) -> ConnectorFuture<'a, ()> {
        panic!("new state")
    }
    fn consume<'a>(&'a self, key: &'a str) -> ConnectorFuture<'a, Option<OAuthStateRaw>> {
        Box::pin(async move {
            self.barrier.wait().await;
            self.state.consume(key).await
        })
    }
}
#[tokio::test]
async fn concurrent_callbacks_consume_once_before_exchange() {
    for second_denial in [false, true] {
        let no = Arc::new(Forbidden);
        let success = Arc::new(Success(Mutex::new(vec![]), 200));
        let service = ConnectorService::new(ConnectorServiceDependencies {
            store: Some(success.clone()),
            state: Some(Arc::new(ContendedState {
                barrier: tokio::sync::Barrier::new(2),
                state: StateStore(Mutex::new(Some(issued()))),
            })),
            clock: Arc::new(SystemConnectorClock),
            nonce: no.clone(),
            cipher: success.clone(),
            form_http: success.clone(),
            provider_http: no.clone(),
            search: no.clone(),
            activation: no,
            console_public_base_url: String::new(),
        });
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(
                service.callback(
                    "github",
                    Some("code"),
                    None,
                    "tenant:user:github:nonce",
                    "http://test/"
                ),
                service.callback(
                    "github",
                    if second_denial { None } else { Some("code") },
                    if second_denial {
                        Some("access_denied")
                    } else {
                        None
                    },
                    "tenant:user:github:nonce",
                    "http://test/"
                )
            )
        })
        .await
        .unwrap();
        let (winner, loser) = if a.is_ok() {
            (a.unwrap(), b.err().unwrap())
        } else {
            (b.unwrap(), a.err().unwrap())
        };
        assert_eq!(loser.status, 400);
        let expected = if winner.contains("connected=github") {
            vec!["config", "exchange", "save"]
        } else {
            vec![]
        };
        assert_eq!(*success.0.lock().unwrap(), expected);
    }
}

// Contract fixtures call the actual service and mounted callback; no providers or listeners.
use ai_platform_gateway_foundation::native_contract::connectors as connector_contract;
struct ContractRows;
impl ConnectorUserStore for ContractRows {
    fn run(&self, op: ConnectorUserOperation) -> ConnectorFuture<'_, Vec<ConnectorRecord>> {
        Box::pin(async move {
            let rows = match op {
                ConnectorUserOperation::AvailableConfigs { .. } => vec![json!({"provider":"github","display_name":"GitHub","description":null,"icon_url":null,"enabled":true})],
                ConnectorUserOperation::UserStatuses { .. } => vec![json!({"provider":"github","status":"connected"})],
                ConnectorUserOperation::ListMine { .. } => vec![json!({"provider":"github","provider_metadata":{"arbitrary":[null,7]},"last_used_at":null})],
                ConnectorUserOperation::EffectiveEnabledConfig { .. } => vec![json!({"client_id":"fixture","auth_url":"https://fixture.example/auth","scopes":"read"})],
                ConnectorUserOperation::Revoke { .. } => vec![],
                _ => panic!("unexpected operation"),
            };
            Ok(rows.into_iter().map(ConnectorRecord).collect())
        })
    }
}
struct ContractAux;
impl ConnectorNonce for ContractAux {
    fn nonce(&self) -> Result<String, ConnectorError> { Ok("nonce".into()) }
}
impl OAuthStateStore for ContractAux {
    fn save<'a>(&'a self, _: &'a str, _: Value, _: u64) -> ConnectorFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn consume<'a>(&'a self, _: &'a str) -> ConnectorFuture<'a, Option<OAuthStateRaw>> { panic!("unexpected consume") }
}
impl ConnectorActivation for ContractAux {
    fn start_confluence<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str, _: &'a str) -> ConnectorFuture<'a, Vec<Value>> { panic!("start") }
    fn stop<'a>(&'a self, _: &'a str, _: &'a str) -> ConnectorFuture<'a, ()> { Box::pin(async { Err(ConnectorError::source()) }) }
    fn status(&self, _: &str, _: &str) -> Result<(bool, Vec<Value>), ConnectorError> { panic!("status") }
}
fn assert_closed_object(value: &Value, schema: &Value) {
    let fields = schema["properties"].as_object().unwrap();
    assert_eq!(value.as_object().unwrap().keys().collect::<Vec<_>>(), fields.keys().collect::<Vec<_>>());
    for (key, field) in fields {
        let types = field["type"].as_array().cloned().unwrap_or_else(|| vec![field["type"].clone()]);
        let actual = if value[key].is_string() { "string" } else if value[key].is_boolean() { "boolean" } else if value[key].is_null() { "null" } else { panic!("unexpected field type") };
        assert!(types.contains(&json!(actual)), "{key}");
        if let Some(expected) = field.get("const") { assert_eq!(&value[key], expected); }
    }
}
#[tokio::test]
async fn source_owned_personal_success_shapes_match_service_projection() {
    let no=Arc::new(Forbidden); let aux=Arc::new(ContractAux);
    let service=ConnectorService::new(ConnectorServiceDependencies {
        store:Some(Arc::new(ContractRows)),state:Some(aux.clone()),clock:Arc::new(SystemConnectorClock),
        nonce:aux.clone(),cipher:no.clone(),form_http:no.clone(),provider_http:no.clone(),search:no,
        activation:aux,console_public_base_url:String::new(),
    });
    let user=ConnectorUser{user_id:"user".into(),tenant_id:"tenant".into()};
    let mut doc=json!({"paths":{}}); connector_contract::extend_document(&mut doc);
    let available=service.available(&user).await.unwrap();
    assert_closed_object(&available[0], &connector_contract::available_schema()["items"]);
    let mine=service.mine(&user).await.unwrap();
    assert_eq!(mine[0]["provider_metadata"]["arbitrary"],json!([null,7]));
    assert_eq!(doc["paths"]["/api/v1/connectors/mine"]["get"]["responses"]["200"]["content"]["application/json"]["schema"],json!({"type":"array","items":{}}));
    for (path,method,value) in [
        ("/api/v1/connectors/auth/{provider}","get",service.begin(&user,"github","https://gateway.example").await.unwrap()),
        ("/api/v1/connectors/{provider}","delete",service.disconnect(&user,"github").await.unwrap()),
    ] { assert_closed_object(&value,&doc["paths"][path][method]["responses"]["200"]["content"]["application/json"]["schema"]); }
}
#[tokio::test]
async fn source_owned_callback_status_and_media_match_actual_router() {
    let mut doc=json!({"paths":{}});connector_contract::extend_document(&mut doc);
    let op=&doc["paths"]["/api/v1/connectors/callback/{provider}"]["get"];
    assert_eq!(op["security"],json!([]));assert!(op["responses"].get("200").is_none());
    for query in ["error=denied&state=tenant:user:github:nonce","", "error=", "code=code&state=tenant:user:github:nonce"] {
        let response=request(mounted(Some(issued()),""),query).await;
        let declaration=&op["responses"][response.status().as_str()];
        assert!(declaration.is_object());
        if response.status()==StatusCode::FOUND {
            assert!(response.headers().contains_key("location"));
            assert_eq!(declaration["headers"]["Location"]["schema"]["type"],"string");
        } else {
            let media=response.headers()["content-type"].to_str().unwrap().split(';').next().unwrap();
            assert!(declaration["content"].get(media).is_some());
        }
    }
}

#[tokio::test]
async fn source_owned_callback_retains_plaintext_internal_error() {
    let mut doc=json!({"paths":{}});connector_contract::extend_document(&mut doc);
    let response=request(mounted(Some(json!("[1]")),""),"error=denied&state=tenant:user:github:nonce").await;
    assert_eq!(response.status(),StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.headers()["content-type"].to_str().unwrap().starts_with("text/plain"));
    assert_eq!(doc["paths"]["/api/v1/connectors/callback/{provider}"]["get"]["responses"]["500"]["content"]["text/plain"]["schema"],json!({"type":"string"}));
}
