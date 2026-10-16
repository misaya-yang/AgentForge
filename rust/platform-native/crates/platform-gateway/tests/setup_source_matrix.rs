//! Ports tests/api/test_setup_state.py through the actual native HTTP router.
//! Identity and provider metadata are explicit fakes; no JWT/DB integration is claimed.
use ai_platform_gateway::{health_api::{HealthApiFuture,HealthCancellationFuture,HealthHttpCancellation,HealthModelMetadata},setup_api::{self,SetupApi,SetupIdentity,SetupIdentityResolver}};
use ai_platform_auth::rbac::Rbac;
use axum::{Router,body::{Body,to_bytes},http::{HeaderMap,Method,Request,StatusCode},response::{Response,IntoResponse}};
use serde_json::{Value,json};
use std::{sync::Arc,collections::HashMap};
use tower::ServiceExt;
struct Identity(SetupIdentity);
impl SetupIdentityResolver for Identity {fn resolve<'a>(&'a self,_:&'a HeaderMap,_:&'a str)->HealthApiFuture<'a,SetupIdentity>{Box::pin(async{Ok(self.0.clone())})}}
struct Metadata(Vec<String>);
impl HealthModelMetadata for Metadata {
 fn configured<'a>(&'a self,tenant:&'a str)->HealthApiFuture<'a,Vec<String>> {Box::pin(async move{assert_eq!(tenant,"tenant_test");Ok(self.0.clone())})}
 fn model_counts<'a>(&'a self,_:&'a str)->HealthApiFuture<'a,serde_json::Map<String,Value>>{Box::pin(async{Ok(Default::default())})}
}
struct Cancellation;
impl HealthHttpCancellation for Cancellation {fn cancelled(&self)->HealthCancellationFuture<'_>{Box::pin(async{StatusCode::SERVICE_UNAVAILABLE.into_response()})}}
fn identity()->SetupIdentity {SetupIdentity{user_id:"user_test".into(),tenant_id:"tenant_test".into(),roles:vec![],permissions:vec!["console:services:view".into()],is_authenticated:true}}
fn app(configured:Option<Vec<&str>>,identity:SetupIdentity,roles:HashMap<String,Vec<String>>,mode:&str,model:&str)->Router {
 let api=SetupApi{identity:Arc::new(Identity(identity)),rbac:Rbac::new(roles),metadata:configured.map(|v|Arc::new(Metadata(v.into_iter().map(str::to_owned).collect())) as Arc<dyn HealthModelMetadata>),mode:mode.into(),default_model:model.into(),cancellation:Arc::new(Cancellation)};
 Router::new().nest("/api/v1",setup_api::setup_router(Arc::new(api)))
}
async fn invoke(app:Router,method:Method)->(StatusCode,Value){let response=app.oneshot(Request::builder().method(method).uri("/api/v1/setup/state").body(Body::empty()).unwrap()).await.unwrap();let status=response.status();let bytes=to_bytes(response.into_body(),1024*1024).await.unwrap();(status,serde_json::from_slice(&bytes).unwrap_or(Value::Null))}
#[tokio::test] async fn reports_unconfigured(){let(status,body)=invoke(app(Some(vec![]),identity(),Default::default(),"ui","qwen3.8-flash"),Method::GET).await;assert_eq!(status,StatusCode::OK);assert_eq!(body["configured"],false);assert_eq!(body["missing"].as_array().unwrap().len(),setup_api::SETUP_PROVIDERS.len());assert_eq!(body["mode"],"ui");assert_eq!(body["default_model"],"qwen3.8-flash");}
#[tokio::test] async fn reports_provider_credentials(){let(status,body)=invoke(app(Some(vec!["dashscope"]),identity(),Default::default(),"ui","m"),Method::GET).await;assert_eq!(status,StatusCode::OK);assert_eq!(body["configured"],true);assert!(!body["missing"].as_array().unwrap().contains(&json!("dashscope")));}
#[tokio::test] async fn accepts_database_provider_ids(){for provider in ["dashscope-cn","dashscope-intl","tenant-custom"] {let(status,body)=invoke(app(Some(vec![provider]),identity(),Default::default(),"ui","m"),Method::GET).await;assert_eq!(status,StatusCode::OK);assert_eq!(body["configured"],true);}}
#[tokio::test] async fn graceful_without_metadata(){let(status,body)=invoke(app(None,identity(),Default::default(),"ui","m"),Method::GET).await;assert_eq!(status,StatusCode::OK);assert_eq!(body["configured"],false);assert_eq!(body["missing"].as_array().unwrap().len(),setup_api::SETUP_PROVIDERS.len());}
#[tokio::test] async fn exposes_selected_deployment_mode_and_default(){let(status,body)=invoke(app(None,identity(),Default::default(),"environment","default-model-from-env"),Method::GET).await;assert_eq!(status,StatusCode::OK);assert_eq!(body["mode"],"environment");assert_eq!(body["default_model"],"default-model-from-env");}
#[tokio::test] async fn rejects_authenticated_without_permission(){let mut user=identity();user.permissions.clear();let(status,body)=invoke(app(None,user,Default::default(),"ui","m"),Method::GET).await;assert_eq!(status,StatusCode::FORBIDDEN);assert_eq!(body["detail"]["required_capability"],"ServiceListRead");}
#[tokio::test] async fn accepts_configured_role_permission(){let mut user=identity();user.permissions.clear();user.roles=vec!["setup_operator".into()];let(status,_)=invoke(app(None,user,HashMap::from([("setup_operator".into(),vec!["console:services:view".into()])]),"ui","m"),Method::GET).await;assert_eq!(status,StatusCode::OK);}
#[tokio::test] async fn rejects_anonymous_guests(){let mut user=identity();user.is_authenticated=false;let(status,body)=invoke(app(None,user,Default::default(),"ui","m"),Method::GET).await;assert_eq!(status,StatusCode::UNAUTHORIZED);assert_eq!(body["detail"],"Authentication required");}
#[tokio::test] async fn source_get_only_methods_and_mount(){for method in [Method::POST,Method::PUT,Method::PATCH,Method::DELETE,Method::OPTIONS,Method::HEAD]{let response=app(None,identity(),Default::default(),"ui","m").oneshot(Request::builder().method(method).uri("/api/v1/setup/state").body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),StatusCode::METHOD_NOT_ALLOWED);assert_eq!(response.headers()["allow"],"GET");}}
