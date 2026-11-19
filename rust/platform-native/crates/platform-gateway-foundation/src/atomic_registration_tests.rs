//! Offline tests of production registration admission, registry and memory storage.
use super::*;
use crate::service_registry::{MemoryRegistryStorage, RegistryFuture, RegistryStorage};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Collaborators(AtomicUsize);
impl ServicesIdentityResolver for Collaborators {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str, _: bool) -> ServicesFuture<'a, ServicesIdentity> {
        Box::pin(async { Ok(admin()) })
    }
    fn context(&self, _: &HeaderMap) -> ConfigAuditRequestContext { Default::default() }
    fn permission_denied(&self, _: &ServicesIdentity, _: &ConfigAuditRequestContext, _: Capability) {}
}
impl ServicesRuntime for Collaborators {
    fn status(&self) -> ServicesRuntimeStatus { ServicesRuntimeStatus { control_present: false, image_worker_running: false } }
}
impl ServicesProxyInvalidation for Collaborators {
    fn invalidate(&self, _: &str) -> Result<(), ServicesError> { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) }
}
impl ServicesExceptionText for Collaborators {
    fn registry(&self, _: &RegistryError) -> String { "registry_failure".into() }
    fn validation(&self, _: &LangGraphValidationError) -> String { "validation_failure".into() }
    fn internal(&self) -> String { "internal_failure".into() }
}
fn admin() -> ServicesIdentity {
    ServicesIdentity { roles: vec!["platform_admin".into()], permissions: vec!["admin:*".into()], ..Default::default() }
}
fn definition() -> Value {
    json!({"service_id":"atomic-fixture","name":"Original","service_type":"custom","connector_type":"http",
        "status":"active","supported_modes":["sync","stream"],"accepted_content_types":["text"],"output_content_types":["text"],
        "session_enabled":true,"max_retries":0,"connector_config":{"sdk_provider_api_key":"synthetic-fixture-key"},
        "metadata":{"adapter_type":"openai_agents","sdk_agent":{"model":"fixture-model","instructions":"","max_turns":8}}})
}
fn api(storage: Arc<dyn RegistryStorage>) -> (ServicesApi, Arc<Collaborators>) {
    let registry = Arc::new(ServiceRegistry::new(storage));
    registry.configure_sdk_agent_key("offline-test-encryption-only".into()).unwrap();
    let c = Arc::new(Collaborators(AtomicUsize::new(0)));
    (ServicesApi::new(ServicesDependencies { identity:c.clone(), rbac:Rbac::default(), registry,
        access_resolver:ServiceAccessConstraintResolver::new(), access_database:None,providers:None,models:None,
        runtime:c.clone(),proxy_invalidation:Some(c.clone()),transparent_proxy:None,exception_text:c.clone() }), c)
}
async fn create(api: &ServicesApi, body: &Value) -> Result<Value, ServicesError> {
    api.handle(ServicesOperation::CreateRegistration, &HeaderMap::new(), body, &Default::default()).await
}
fn status(error: ServicesError) -> u16 { error.response(&Default::default()).unwrap().0 }

#[tokio::test]
async fn atomic_registration_collision_preserves_storage_cache_and_proxy() {
    let storage=Arc::new(MemoryRegistryStorage::new()); let (api,c)=api(storage.clone());
    let mut body=definition(); let result=create(&api,&body).await.unwrap();
    assert_eq!(result,json!({"service_id":"atomic-fixture","status":"registered"}));
    let original=serde_json::to_value(storage.get("atomic-fixture").await.unwrap().unwrap().snapshot()).unwrap();
    assert!(original["connector_config"].get("sdk_provider_api_key").is_none());
    assert!(original["connector_config"]["sdk_provider_api_key_encrypted"].as_str().unwrap().starts_with("enc:"));
    body["name"]=json!("Replacement");
    assert_eq!(status(create(&api,&body).await.unwrap_err()),409);
    let stored=serde_json::to_value(storage.get("atomic-fixture").await.unwrap().unwrap().snapshot()).unwrap();
    let cached=serde_json::to_value(api.dependencies.registry.get("atomic-fixture").await.unwrap().unwrap().snapshot()).unwrap();
    assert_eq!(original,stored); assert_eq!(original,cached); assert_eq!(c.0.load(Ordering::SeqCst),1);
    // Legacy callers still intentionally upsert.
    api.handle(ServicesOperation::Register,&HeaderMap::new(),&body,&Default::default()).await.unwrap();
    assert_eq!(storage.get("atomic-fixture").await.unwrap().unwrap().snapshot().name,"Replacement");
}
#[tokio::test]
async fn atomic_registration_two_registries_have_one_winner() {
    let storage=Arc::new(RacingStore { rows:MemoryRegistryStorage::new(), barrier:tokio::sync::Barrier::new(2) }); let (a,ca)=api(storage.clone());let(b,cb)=api(storage);
    let x=definition();let mut y=x.clone();y["name"]=json!("Second");
    let (x,y)=tokio::join!(create(&a,&x),create(&b,&y));
    assert!(x.is_ok() ^ y.is_ok()); assert_eq!(status(x.err().or(y.err()).unwrap()),409);
    assert_eq!(ca.0.load(Ordering::SeqCst)+cb.0.load(Ordering::SeqCst),1);
}
struct LegacyOnly { rows:MemoryRegistryStorage, writes:AtomicUsize }
impl RegistryStorage for LegacyOnly {
    fn save(&self,s:ServiceHandle)->RegistryFuture<'_,()> { self.writes.fetch_add(1,Ordering::SeqCst); self.rows.save(s) }
    fn get<'a>(&'a self,id:&'a str)->RegistryFuture<'a,Option<ServiceHandle>> { self.rows.get(id) }
    fn list(&self)->RegistryFuture<'_,Vec<ServiceHandle>> { self.rows.list() }
    fn delete<'a>(&'a self,id:&'a str)->RegistryFuture<'a,bool> { self.rows.delete(id) }
}
struct FailingStore(LegacyOnly);
impl RegistryStorage for FailingStore {
    fn insert_if_absent(&self,_:ServiceHandle)->RegistryFuture<'_,bool> { Box::pin(async {Err(RegistryError::StorageUnavailable)}) }
    fn save(&self,s:ServiceHandle)->RegistryFuture<'_,()> {self.0.save(s)}
    fn get<'a>(&'a self,id:&'a str)->RegistryFuture<'a,Option<ServiceHandle>> {self.0.get(id)}
    fn list(&self)->RegistryFuture<'_,Vec<ServiceHandle>> {self.0.list()}
    fn delete<'a>(&'a self,id:&'a str)->RegistryFuture<'a,bool> {self.0.delete(id)}
}
#[tokio::test]
async fn atomic_registration_unsupported_and_failure_never_fall_back() {
    let legacy=Arc::new(LegacyOnly{rows:MemoryRegistryStorage::new(),writes:AtomicUsize::new(0)});
    let(a,c)=api(legacy.clone()); assert_eq!(status(create(&a,&definition()).await.unwrap_err()),501);
    assert_eq!(legacy.writes.load(Ordering::SeqCst),0);assert_eq!(c.0.load(Ordering::SeqCst),0);
    assert!(a.dependencies.registry.get("atomic-fixture").await.unwrap().is_none());
    let failed=Arc::new(FailingStore(LegacyOnly{rows:MemoryRegistryStorage::new(),writes:AtomicUsize::new(0)}));
    let(a,c)=api(failed.clone()); assert_eq!(status(create(&a,&definition()).await.unwrap_err()),400);
    assert_eq!(failed.0.writes.load(Ordering::SeqCst),0);assert_eq!(c.0.load(Ordering::SeqCst),0);
    assert!(a.dependencies.registry.get("atomic-fixture").await.unwrap().is_none());
}
#[tokio::test]
async fn atomic_registration_validation_and_permissions_precede_writes() {
    let(a,c)=api(Arc::new(MemoryRegistryStorage::new()));
    let mut body=definition();body["create_only"]=json!(true);
    assert_eq!(status(create(&a,&body).await.unwrap_err()),422);
    for actor in [ServicesIdentity::default(),ServicesIdentity{permissions:vec!["admin:*".into()],..Default::default()}] {
        assert_eq!(status(a.execute(ServicesOperation::CreateRegistration,&actor,&definition(),&Default::default()).await.unwrap_err()),403);
    }
    assert_eq!(c.0.load(Ordering::SeqCst),0);
    assert!(a.dependencies.registry.storage.list().await.unwrap().is_empty());
}

struct RacingStore { rows:MemoryRegistryStorage, barrier:tokio::sync::Barrier }
impl RegistryStorage for RacingStore {
    fn insert_if_absent(&self,s:ServiceHandle)->RegistryFuture<'_,bool> {
        Box::pin(async move { self.barrier.wait().await; self.rows.insert_if_absent(s).await })
    }
    fn save(&self,s:ServiceHandle)->RegistryFuture<'_,()> {self.rows.save(s)}
    fn get<'a>(&'a self,id:&'a str)->RegistryFuture<'a,Option<ServiceHandle>> {self.rows.get(id)}
    fn list(&self)->RegistryFuture<'_,Vec<ServiceHandle>> {self.rows.list()}
    fn delete<'a>(&'a self,id:&'a str)->RegistryFuture<'a,bool> {self.rows.delete(id)}
}
struct BrokenInvalidation;
impl ServicesProxyInvalidation for BrokenInvalidation {
    fn invalidate(&self,_:&str)->Result<(),ServicesError> {Err(ServicesError::Internal)}
}
#[tokio::test]
async fn atomic_registration_post_insert_failure_does_not_undo_or_replay() {
    let storage=Arc::new(MemoryRegistryStorage::new()); let (mut a,_)=api(storage.clone());
    a.dependencies.proxy_invalidation=Some(Arc::new(BrokenInvalidation));
    assert_eq!(status(create(&a,&definition()).await.unwrap_err()),400);
    assert!(storage.get("atomic-fixture").await.unwrap().is_some());
    let stored=serde_json::to_value(storage.get("atomic-fixture").await.unwrap().unwrap().snapshot()).unwrap();
    assert_eq!(status(create(&a,&definition()).await.unwrap_err()),409);
    assert_eq!(stored,serde_json::to_value(storage.get("atomic-fixture").await.unwrap().unwrap().snapshot()).unwrap());
}

#[tokio::test]
async fn atomic_registration_invalid_sdk_credentials_and_selections_do_not_write() {
    let(a,c)=api(Arc::new(MemoryRegistryStorage::new()));
    let mut cases=Vec::new();
    let mut b=definition(); b["connector_config"]["sdk_provider_api_key_encrypted"]=json!("enc:synthetic"); cases.push(b);
    let mut b=definition(); b["metadata"]["sdk_agent"]["max_turns"]=json!(33); cases.push(b);
    let mut b=definition(); b["metadata"]["sdk_agent"]["model"]=json!(" "); cases.push(b);
    let mut b=definition(); b["connector_config"]["sdk_provider_api_key"]=json!("enc:synthetic-long-key"); cases.push(b);
    let mut b=definition(); b["metadata"]["sdk_tools"]=json!({"knowledge_dataset_ids":["not-a-uuid"]}); cases.push(b);
    for body in cases {
        assert!(matches!(status(create(&a,&body).await.unwrap_err()),400|422));
        assert!(a.dependencies.registry.storage.list().await.unwrap().is_empty());
    }
    assert_eq!(c.0.load(Ordering::SeqCst),0);
}

#[tokio::test]
async fn atomic_registration_non_object_errors_never_echo_credentials() {
    let(a,c)=api(Arc::new(MemoryRegistryStorage::new()));
    for body in [json!([{"sdk_provider_api_key":"SYNTHETIC_DO_NOT_ECHO"}]),json!("SYNTHETIC_DO_NOT_ECHO")] {
        let error=create(&a,&body).await.unwrap_err();
        let(status,output)=error.response(&Default::default()).unwrap();
        assert_eq!(status,422); assert!(!output.to_string().contains("SYNTHETIC_DO_NOT_ECHO"));
    }
    assert_eq!(c.0.load(Ordering::SeqCst),0);
    assert!(a.dependencies.registry.storage.list().await.unwrap().is_empty());
}
