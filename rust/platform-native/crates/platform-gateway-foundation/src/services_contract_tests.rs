//! No listener/provider/database: real ServicesApi, registry, memory store,
//! ServiceDefinition serializer, router construction and dispatch selection.
use super::*;
use crate::service_registry::{MemoryRegistryStorage, RegistryStorage};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Collaborators {
    actor: ServicesIdentity,
    calls: Mutex<Vec<(String, bool)>>,
    invalidations: AtomicUsize,
    denied: AtomicUsize,
}
impl ServicesIdentityResolver for Collaborators {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        path: &'a str,
        needs_user: bool,
    ) -> ServicesFuture<'a, ServicesIdentity> {
        self.calls.lock().unwrap().push((path.into(), needs_user));
        Box::pin(async { Ok(self.actor.clone()) })
    }
    fn context(&self, _: &HeaderMap) -> ConfigAuditRequestContext {
        Default::default()
    }
    fn permission_denied(
        &self,
        _: &ServicesIdentity,
        _: &ConfigAuditRequestContext,
        _: Capability,
    ) {
        self.denied.fetch_add(1, Ordering::SeqCst);
    }
}
impl ServicesRuntime for Collaborators {
    fn status(&self) -> ServicesRuntimeStatus {
        ServicesRuntimeStatus {
            control_present: true,
            image_worker_running: false,
        }
    }
}
impl ServicesProxyInvalidation for Collaborators {
    fn invalidate(&self, _: &str) -> Result<(), ServicesError> {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl ServicesExceptionText for Collaborators {
    fn registry(&self, _: &RegistryError) -> String {
        "safe_registry_failure".into()
    }
    fn validation(&self, _: &LangGraphValidationError) -> String {
        "safe_validation_failure".into()
    }
    fn internal(&self) -> String {
        "safe_internal_failure".into()
    }
}
fn actor() -> ServicesIdentity {
    ServicesIdentity {
        user_id: "user".into(),
        tenant_id: "tenant".into(),
        roles: vec!["platform_admin".into()],
        permissions: vec!["admin:*".into()],
        ..Default::default()
    }
}
fn api(actor: ServicesIdentity) -> (ServicesApi, Arc<MemoryRegistryStorage>, Arc<Collaborators>) {
    let storage = Arc::new(MemoryRegistryStorage::new());
    let c = Arc::new(Collaborators {
        actor,
        calls: Mutex::new(Vec::new()),
        invalidations: AtomicUsize::new(0),
        denied: AtomicUsize::new(0),
    });
    (
        ServicesApi::new(ServicesDependencies {
            identity: c.clone(),
            rbac: Rbac::default(),
            registry: Arc::new(ServiceRegistry::new(storage.clone())),
            access_resolver: ServiceAccessConstraintResolver::new(),
            access_database: None,
            providers: None,
            models: None,
            runtime: c.clone(),
            proxy_invalidation: Some(c.clone()),
            transparent_proxy: None,
            exception_text: c.clone(),
        }),
        storage,
        c,
    )
}
fn definition() -> Value {
    json!({"service_id":"service-fixture","name":"Original","service_type":"custom","connector_type":"http",
        "supported_modes":["sync","async","stream","webhook"],"accepted_content_types":["text","json"],
        "output_content_types":["text","tool_result"],"tags":["kept"],"metadata":{"fixture":true},
        "connector_config":{"api_key":"synthetic-only-key"},"unknown_ignored":true})
}
async fn call(
    api: &ServicesApi,
    operation: ServicesOperation,
    body: &Value,
) -> Result<Value, ServicesError> {
    api.handle(operation, &HeaderMap::new(), body, &Default::default())
        .await
}
fn assert_success(id: &str, value: &Value) {
    let schema = contract::success_schema(id);
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(
        validator.is_valid(value),
        "{id}: {value} does not match {schema}"
    );
}
fn assert_error(id: &str, error: ServicesError, expected: u16) -> Value {
    let (status, value) = error.response(&Default::default()).unwrap();
    assert_eq!(status, expected);
    let (method, route, _) = SERVICES_ROUTES
        .iter()
        .find(|(_, _, name)| *name == id)
        .unwrap();
    let doc = crate::native_contract::document();
    let schema = &doc["paths"][format!("/api/v1{route}")][method.to_ascii_lowercase()]["responses"]
        [status.to_string()]["content"]["application/json"]["schema"];
    assert!(
        jsonschema::is_valid(schema, &value),
        "{id}/{status}: {value}"
    );
    value
}
#[tokio::test]
async fn six_production_operations_and_legacy_upsert_keep_real_wire_shapes() {
    let (a, storage, c) = api(actor());
    let registered = call(&a, ServicesOperation::Register, &definition())
        .await
        .unwrap();
    assert_eq!(
        registered,
        json!({"service_id":"service-fixture","status":"registered"})
    );
    assert_success("register_service", &registered);
    let list = call(
        &a,
        ServicesOperation::List {
            service_type: None,
            tags: vec![],
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("list_services", &list);
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["service_id"], "assistant");
    assert_eq!(list[0]["status"], "unavailable");
    assert!(list[0].get("availability_status").is_none());
    assert!(list[1]["availability_status"].is_null());
    assert_eq!(list[1]["supported_modes"], definition()["supported_modes"]);
    let detail = call(
        &a,
        ServicesOperation::Get {
            service_id: "service-fixture".into(),
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("get_service", &detail);
    assert_eq!(detail["name"], "Original");
    assert!(detail.get("service_config").is_none());
    let schema = call(
        &a,
        ServicesOperation::Schema {
            service_id: "service-fixture".into(),
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("get_service_schema", &schema);
    assert_eq!(
        schema,
        json!({"input_schema":{},"output_schema":{},"accepted_content_types":["text","json"],"output_content_types":["text","tool_result"]})
    );
    let changed = call(
        &a,
        ServicesOperation::Update {
            service_id: "service-fixture".into(),
        },
        &json!({"name":"Updated","ignored":123}),
    )
    .await
    .unwrap();
    assert_eq!(
        changed,
        json!({"status":"success","service_id":"service-fixture"})
    );
    assert_success("update_service", &changed);
    assert_eq!(
        storage
            .get("service-fixture")
            .await
            .unwrap()
            .unwrap()
            .snapshot()
            .name,
        "Updated"
    );
    let mut replacement = definition();
    replacement["name"] = json!("Upserted");
    call(&a, ServicesOperation::Register, &replacement)
        .await
        .unwrap();
    assert_eq!(
        storage
            .get("service-fixture")
            .await
            .unwrap()
            .unwrap()
            .snapshot()
            .name,
        "Upserted"
    );
    let deleted = call(
        &a,
        ServicesOperation::Delete {
            service_id: "service-fixture".into(),
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("delete_service", &deleted);
    assert_eq!(deleted, changed);
    assert!(storage.get("service-fixture").await.unwrap().is_none());
    assert_eq!(c.invalidations.load(Ordering::SeqCst), 4);
    let calls = c.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|(_, user)| *user).count(), 2);
    assert!(
        calls
            .iter()
            .all(|(path, _)| path.starts_with("/api/v1/services"))
    );
}
#[tokio::test]
async fn dynamic_domain_fields_and_optional_config_are_not_narrowed() {
    let (a, storage, _) = api(actor());
    let mut service = ServiceDefinition::new("dynamic", "Dynamic");
    service.connector_config = json!({"nested":[1,true,null]});
    service.input_schema = json!(false);
    service.output_schema = json!([{"unbounded":null}]);
    service.metadata = json!(["arbitrary", false]);
    service.rate_limit = Some(json!(7));
    service.async_config = Some(json!([1, 2]));
    storage
        .save(ServiceHandle::new(service.clone()))
        .await
        .unwrap();
    let detail = call(
        &a,
        ServicesOperation::Get {
            service_id: "dynamic".into(),
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("get_service", &detail);
    assert_eq!(detail["input_schema"], json!({}));
    assert_eq!(detail["metadata"], service.metadata);
    assert_eq!(detail["rate_limit"], json!(7));
    let schema = call(
        &a,
        ServicesOperation::Schema {
            service_id: "dynamic".into(),
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("get_service_schema", &schema);
    assert_eq!(schema["input_schema"], json!(false));
    let list = call(
        &a,
        ServicesOperation::List {
            service_type: None,
            tags: vec![],
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("list_services", &list);
    assert_eq!(list[1]["metadata"], service.metadata);
    service.service_config = Some(Default::default());
    let detail = service_to_dict(&service, false).unwrap();
    assert_success("get_service", &detail);
    assert!(detail["service_config"].is_object());
}
#[tokio::test]
async fn visibility_and_schema_lookup_semantics_stay_distinct() {
    let mut restricted = actor();
    restricted.roles.push("admin".into());
    restricted.access.load.api_key_info = json!({"allowed_services":["another-service"]});
    let (a, storage, _) = api(restricted);
    storage
        .save(ServiceHandle::new(ServiceDefinition::new(
            "service-fixture",
            "Fixture",
        )))
        .await
        .unwrap();
    let list = call(
        &a,
        ServicesOperation::List {
            service_type: None,
            tags: vec![],
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("list_services", &list);
    assert_eq!(list, json!([]));
    assert_error(
        "get_service",
        call(
            &a,
            ServicesOperation::Get {
                service_id: "service-fixture".into(),
            },
            &Value::Null,
        )
        .await
        .unwrap_err(),
        403,
    );
    let schema = call(
        &a,
        ServicesOperation::Schema {
            service_id: "service-fixture".into(),
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("get_service_schema", &schema);
}
#[tokio::test]
async fn assistant_filter_and_tags_keep_existing_mixed_list_behavior() {
    let (a, _, _) = api(actor());
    call(&a, ServicesOperation::Register, &definition())
        .await
        .unwrap();
    let list = call(
        &a,
        ServicesOperation::List {
            service_type: Some("assistant".into()),
            tags: vec!["kept".into()],
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("list_services", &list);
    assert_eq!(list.as_array().unwrap().len(), 2);
    let list = call(
        &a,
        ServicesOperation::List {
            service_type: None,
            tags: vec!["absent".into()],
        },
        &Value::Null,
    )
    .await
    .unwrap();
    assert_success("list_services", &list);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["service_id"], "assistant");
}
#[test]
fn native_detail_serializer_preserves_masking_and_sdk_write_only_fields() {
    let mut service = ServiceDefinition::new("fixture", "Fixture");
    service.connector_config = json!({"api_key":"synthetic-ordinary","nested":{"token":"synthetic-nested"},
        "sdk_provider_api_key":"synthetic-sdk","sdk_provider_api_key_encrypted":"synthetic-sealed"});
    let admin = service_to_dict(&service, false).unwrap();
    let masked = service_to_dict(&service, true).unwrap();
    assert_success("get_service", &admin);
    assert_success("get_service", &masked);
    assert_eq!(admin["connector_config"]["api_key"], "synthetic-ordinary");
    assert_ne!(masked["connector_config"]["api_key"], "synthetic-ordinary");
    for output in [admin, masked] {
        assert!(
            output["connector_config"]
                .get("sdk_provider_api_key")
                .is_none()
        );
        assert!(
            output["connector_config"]
                .get("sdk_provider_api_key_encrypted")
                .is_none()
        );
        assert_eq!(output["metadata"]["sdk_provider_key_configured"], true);
    }
}
#[tokio::test]
async fn dictionary_validation_unknown_patch_keys_and_error_envelopes_are_real() {
    let (a, storage, c) = api(actor());
    for body in [Value::Null, json!([]), json!("body")] {
        assert_error(
            "register_service",
            call(&a, ServicesOperation::Register, &body)
                .await
                .unwrap_err(),
            422,
        );
    }
    assert!(storage.list().await.unwrap().is_empty());
    call(&a, ServicesOperation::Register, &definition())
        .await
        .unwrap();
    for body in [
        json!({"unknown":true}),
        json!({"service_id":"different","name":"No"}),
    ] {
        assert_error(
            "update_service",
            call(
                &a,
                ServicesOperation::Update {
                    service_id: "service-fixture".into(),
                },
                &body,
            )
            .await
            .unwrap_err(),
            400,
        );
    }
    assert_error(
        "get_service",
        call(
            &a,
            ServicesOperation::Get {
                service_id: "absent".into(),
            },
            &Value::Null,
        )
        .await
        .unwrap_err(),
        404,
    );
    assert_error(
        "list_services",
        call(
            &a,
            ServicesOperation::List {
                service_type: Some("not-an-enum".into()),
                tags: vec![],
            },
            &Value::Null,
        )
        .await
        .unwrap_err(),
        500,
    );
    let mut denied = actor();
    denied.roles.clear();
    assert_error(
        "register_service",
        a.execute(
            ServicesOperation::Register,
            &denied,
            &definition(),
            &Default::default(),
        )
        .await
        .unwrap_err(),
        403,
    );
    assert_eq!(c.denied.load(Ordering::SeqCst), 1);
    assert_eq!(c.invalidations.load(Ordering::SeqCst), 1);
}
struct Unavailable;
impl ServicesIdentityResolver for Unavailable {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
        _: bool,
    ) -> ServicesFuture<'a, ServicesIdentity> {
        Box::pin(async {
            let error = ai_platform_auth::admission::AdmissionError::Unavailable;
            Err(ServicesError::detail(
                error.status(),
                "Authentication required",
            ))
        })
    }
    fn context(&self, _: &HeaderMap) -> ConfigAuditRequestContext {
        Default::default()
    }
    fn permission_denied(
        &self,
        _: &ServicesIdentity,
        _: &ConfigAuditRequestContext,
        _: Capability,
    ) {
    }
}
#[tokio::test]
async fn trusted_resolver_fixture_outage_shape_matches_source_declaration() {
    let (mut a, _, _) = api(actor());
    a.dependencies.identity = Arc::new(Unavailable);
    let error = call(
        &a,
        ServicesOperation::List {
            service_type: None,
            tags: vec![],
        },
        &Value::Null,
    )
    .await
    .unwrap_err();
    let output = assert_error("list_services", error, 503);
    assert_eq!(output, json!({"detail":"Authentication required"}));
}
struct BrokenInvalidation;
impl ServicesProxyInvalidation for BrokenInvalidation {
    fn invalidate(&self, _: &str) -> Result<(), ServicesError> {
        Err(ServicesError::Internal)
    }
}
#[tokio::test]
async fn failed_post_write_invalidation_does_not_undo_or_retry_mutations() {
    let (mut a, storage, _) = api(actor());
    a.dependencies.proxy_invalidation = Some(Arc::new(BrokenInvalidation));
    assert_error(
        "register_service",
        call(&a, ServicesOperation::Register, &definition())
            .await
            .unwrap_err(),
        400,
    );
    assert!(storage.get("service-fixture").await.unwrap().is_some());
    assert_error(
        "update_service",
        call(
            &a,
            ServicesOperation::Update {
                service_id: "service-fixture".into(),
            },
            &json!({"name":"Persisted"}),
        )
        .await
        .unwrap_err(),
        400,
    );
    assert_eq!(
        storage
            .get("service-fixture")
            .await
            .unwrap()
            .unwrap()
            .snapshot()
            .name,
        "Persisted"
    );
    assert_error(
        "delete_service",
        call(
            &a,
            ServicesOperation::Delete {
                service_id: "service-fixture".into(),
            },
            &Value::Null,
        )
        .await
        .unwrap_err(),
        500,
    );
    assert!(storage.get("service-fixture").await.unwrap().is_none());
}
struct Cancelled;
impl ServicesCancellation for Cancelled {
    fn cancelled(&self) -> crate::service_config_api::ServiceConfigCancellationFuture<'_> {
        Box::pin(async { StatusCode::from_u16(499).unwrap().into_response() })
    }
}
#[tokio::test]
async fn real_router_factory_selection_query_and_host_cancellation_stay_bounded() {
    let (a, _, _) = api(actor());
    let api = Arc::new(a);
    let _router = router(api.clone(), Arc::new(Cancelled));
    // Actual router factory consumes the same route inventory; selector is the
    // production dispatch helper. No socket or fabricated matched-path extractor.
    let uri: axum::http::Uri =
        "/api/v1/services?service_type=custom&tags=a&tags=&service_type=assistant&ignored=x"
            .parse()
            .unwrap();
    let selected = routed_operation("/api/v1/services", "GET", String::new(), &uri).unwrap();
    assert!(
        matches!(selected,ServicesOperation::List{service_type:Some(value),tags} if value=="assistant" && tags==vec!["a",""])
    );
    for (method, route, _) in SERVICES_ROUTES {
        let path = format!("/api/v1{route}");
        assert!(allowed_methods(&path).contains(method));
        let selected = routed_operation(&path, method, "fixture".into(), &uri).unwrap();
        match *method {
            "POST" => assert!(matches!(
                selected,
                ServicesOperation::Register | ServicesOperation::CreateRegistration
            )),
            "PUT" => assert!(
                matches!(selected,ServicesOperation::Update{service_id} if service_id=="fixture")
            ),
            "DELETE" => assert!(
                matches!(selected,ServicesOperation::Delete{service_id} if service_id=="fixture")
            ),
            _ => assert!(matches!(
                selected,
                ServicesOperation::List { .. }
                    | ServicesOperation::Get { .. }
                    | ServicesOperation::Schema { .. }
            )),
        }
    }
    let state = HttpState {
        api,
        cancellation: Arc::new(Cancelled),
    };
    let cancelled = response(&state, Err(ServicesError::Cancelled), &Default::default()).await;
    assert_eq!(cancelled.status().as_u16(), 499);
    let doc = crate::native_contract::document();
    assert_eq!(
        doc["paths"]["/api/v1/services"]["get"]["responses"]["default"]["x-native-undeclared"],
        true
    );
}

#[test]
fn production_list_builder_preserves_all_enum_wire_values_and_dynamic_availability() {
    use crate::protocol::{ContentType, InvocationMode};
    let modes = vec![
        InvocationMode::Sync,
        InvocationMode::Async,
        InvocationMode::Stream,
        InvocationMode::Webhook,
    ];
    let content = vec![
        ContentType::Text,
        ContentType::Image,
        ContentType::Audio,
        ContentType::Video,
        ContentType::File,
        ContentType::Json,
        ContentType::ToolCall,
        ContentType::ToolResult,
    ];
    for kind in [
        ServiceType::Conversational,
        ServiceType::Generative,
        ServiceType::Processing,
        ServiceType::Embedding,
        ServiceType::Classification,
        ServiceType::Langgraph,
        ServiceType::Custom,
    ] {
        let mut s = ServiceDefinition::new("wire", "Wire");
        s.service_type = kind;
        s.supported_modes = modes.clone();
        s.accepted_content_types = content.clone();
        s.output_content_types = content.clone();
        s.metadata = json!([null, true, 3]);
        let availability = json!({"availability_status":["dynamic"],"last_health_check_at":42,"last_health_error":{"nested":false}});
        let previous = json!({"service_id":s.service_id,"name":s.name,"description":s.description,"version":s.version,
            "service_type":s.service_type,"supported_modes":s.supported_modes,
            "accepted_content_types":s.accepted_content_types,"output_content_types":s.output_content_types,
            "status":s.status,"tags":s.tags,"metadata":s.metadata,
            "availability_status":availability["availability_status"],"last_health_check_at":availability["last_health_check_at"],
            "last_health_error":availability["last_health_error"]});
        let actual = contract::listed_service(
            &s.service_id,
            &s.name,
            &s.description,
            &s.version,
            s.service_type.as_str(),
            s.supported_modes.iter().map(|v| v.as_str()).collect(),
            s.accepted_content_types
                .iter()
                .map(|v| v.as_str())
                .collect(),
            s.output_content_types.iter().map(|v| v.as_str()).collect(),
            &s.status,
            &s.tags,
            &s.metadata,
            &availability["availability_status"],
            &availability["last_health_check_at"],
            &availability["last_health_error"],
        );
        assert_eq!(actual, previous);
        assert_success("list_services", &json!([actual]));
    }
}
