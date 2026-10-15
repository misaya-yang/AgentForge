//! Actual UsersApi/validator/projector/dispatcher tests. Persistence and identity
//! are explicit controlled doubles; no PostgreSQL, HTTP listener or live auth.
use super::*;
use crate::service_config_api::ServiceConfigCancellationFuture;
use crate::users_store::UsersFuture;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Dependencies {
    actor: ServiceConfigIdentity,
    calls: Mutex<Vec<(bool, String)>>,
    rows: Mutex<HashMap<String, Value>>,
    grants: Mutex<HashMap<String, Vec<Value>>>,
    mutations: Mutex<Vec<(String, String, UserMutation)>>,
    filters: Mutex<Vec<UserListFilter>>,
    resets: Mutex<Vec<(String, String, String)>>,
    deletes: Mutex<Vec<(String, String, String)>>,
    hashes: AtomicUsize,
    invalidations: AtomicUsize,
    fail_invalidation: AtomicBool,
}
impl ServiceConfigIdentityResolver for Dependencies {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        self.calls.lock().unwrap().push((false, path.into()));
        Box::pin(async { Ok(self.actor.clone()) })
    }
}
impl UsersProfileIdentityResolver for Dependencies {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        self.calls.lock().unwrap().push((true, path.into()));
        Box::pin(async { Ok(self.actor.clone()) })
    }
}
impl UsersPasswordHasher for Dependencies {
    fn hash<'a>(&'a self, password: &'a str) -> ServiceConfigFuture<'a, String> {
        assert_eq!(password, "fixture-only-password");
        self.hashes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok("fixture-hash".into()) })
    }
}
impl UsersRuntimeValues for Dependencies {
    fn collision_suffix(&self) -> String {
        "suffix".into()
    }
    fn created_at(&self) -> String {
        "2030-01-01T00:00:00".into()
    }
}
impl UsersServiceAccessInvalidation for Dependencies {
    fn clear(&self) -> Result<(), ServiceConfigError> {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        if self.fail_invalidation.load(Ordering::SeqCst) {
            Err(ServiceConfigError::Internal)
        } else {
            Ok(())
        }
    }
}
impl UsersDatabase for Dependencies {
    fn enabled(&self) -> bool {
        true
    }
    fn get_user<'a>(&'a self, id: &'a str) -> UsersFuture<'a, Option<Value>> {
        Box::pin(async move { Ok(self.rows.lock().unwrap().get(id).cloned()) })
    }
    fn get_user_for_tenant<'a>(
        &'a self,
        id: &'a str,
        tenant: &'a str,
    ) -> UsersFuture<'a, Option<Value>> {
        Box::pin(async move {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .get(id)
                .filter(|v| v["tenant_id"] == tenant)
                .cloned())
        })
    }
    fn get_user_by_email<'a>(&'a self, email: &'a str) -> UsersFuture<'a, Option<Value>> {
        Box::pin(async move {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .find(|v| v["email"] == email)
                .cloned())
        })
    }
    fn list_users_paginated<'a>(
        &'a self,
        filter: &'a UserListFilter,
    ) -> UsersFuture<'a, (Vec<Value>, i64)> {
        self.filters.lock().unwrap().push(filter.clone());
        Box::pin(async move {
            let rows: Vec<_> = self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|v| v["tenant_id"].as_str() == filter.tenant_id.as_deref())
                .cloned()
                .collect();
            Ok((rows.clone(), rows.len() as i64))
        })
    }
    fn save_user_with_password<'a>(&'a self, input: &'a NewUser) -> UsersFuture<'a, ()> {
        Box::pin(async move {
            let id = input.user["user_id"].as_str().unwrap().to_owned();
            let mut rows = self.rows.lock().unwrap();
            assert!(!rows.contains_key(&id), "fixture must never upsert");
            rows.insert(id, input.user.clone());
            Ok(())
        })
    }
    fn reset_user_password_for_tenant<'a>(
        &'a self,
        id: &'a str,
        tenant: &'a str,
        hash: &'a str,
        actor: &'a str,
    ) -> UsersFuture<'a, bool> {
        assert_eq!(hash, "fixture-hash");
        self.resets
            .lock()
            .unwrap()
            .push((id.into(), tenant.into(), actor.into()));
        Box::pin(async { Ok(true) })
    }
    fn update_user_for_tenant<'a>(
        &'a self,
        id: &'a str,
        tenant: &'a str,
        mutation: &'a UserMutation,
    ) -> UsersFuture<'a, bool> {
        self.mutations
            .lock()
            .unwrap()
            .push((id.into(), tenant.into(), mutation.clone()));
        Box::pin(async move {
            let mut rows = self.rows.lock().unwrap();
            let row = rows.get_mut(id).unwrap();
            assert_eq!(row["tenant_id"], tenant);
            for (name, value) in &mutation.fields {
                row[name] = value.clone();
            }
            if let Some(roles) = &mutation.roles {
                row["roles"] = json!(roles);
            }
            if let Some(grants) = &mutation.extra_permissions {
                self.grants.lock().unwrap().insert(
                    id.into(),
                    grants
                        .iter()
                        .map(|v| json!({"permission_code":v}))
                        .collect(),
                );
            }
            Ok(true)
        })
    }
    fn delete_user_for_tenant<'a>(
        &'a self,
        id: &'a str,
        tenant: &'a str,
        actor: &'a str,
    ) -> UsersFuture<'a, bool> {
        self.deletes
            .lock()
            .unwrap()
            .push((id.into(), tenant.into(), actor.into()));
        Box::pin(async move { Ok(self.rows.lock().unwrap().remove(id).is_some()) })
    }
    fn get_user_extra_permissions<'a>(&'a self, id: &'a str) -> UsersFuture<'a, Vec<Value>> {
        Box::pin(async move {
            Ok(self
                .grants
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_default())
        })
    }
}
fn actor(roles: &[&str]) -> ServiceConfigIdentity {
    ServiceConfigIdentity {
        user_id: "actor".into(),
        tenant_id: "tenant".into(),
        roles: roles.iter().map(|v| (*v).into()).collect(),
        permissions: vec![],
    }
}
fn row(id: &str, tenant: &str) -> Value {
    json!({"user_id":id,"tenant_id":tenant,"email":format!("{id}@example.test"),"display_name":"Original","department":"tech","roles":["user"],"metadata":{"service_access":{"mode":"allowlist","allowed_services":["service-a"]}},"private_dynamic_column":{"must_not":"leak"}})
}
fn api(identity: ServiceConfigIdentity) -> (UsersApi, Arc<Dependencies>) {
    let d = Arc::new(Dependencies {
        actor: identity,
        calls: Mutex::new(vec![]),
        rows: Mutex::new(HashMap::from([
            ("actor".into(), row("actor", "tenant")),
            ("target".into(), row("target", "tenant")),
            ("foreign".into(), row("foreign", "other")),
        ])),
        grants: Mutex::new(HashMap::from([(
            "target".into(),
            vec![json!({"permission_code":"custom:read"})],
        )])),
        mutations: Mutex::new(vec![]),
        filters: Mutex::new(vec![]),
        resets: Mutex::new(vec![]),
        deletes: Mutex::new(vec![]),
        hashes: AtomicUsize::new(0),
        invalidations: AtomicUsize::new(0),
        fail_invalidation: AtomicBool::new(false),
    });
    (
        UsersApi::new(UsersDependencies {
            identity: d.clone(),
            profile_identity: d.clone(),
            rbac: Rbac::new(HashMap::from([("admin".into(), vec!["admin:*".into()])])),
            database: Some(d.clone()),
            passwords: d.clone(),
            configuration: UsersConfiguration::new(
                "fixture-only-password".into(),
                "example.test".into(),
            ),
            runtime: d.clone(),
            service_access: d.clone(),
        }),
        d,
    )
}
async fn call(
    api: &UsersApi,
    operation: UsersOperation,
    body: Value,
) -> Result<Value, ServiceConfigError> {
    api.handle(operation, &HeaderMap::new(), &body).await
}
fn success(id: &str, value: &Value) {
    let schema = contract::success_schema(id);
    assert!(
        jsonschema::is_valid(&schema, value),
        "{id}: {value} != {schema}"
    );
}
fn error(id: &str, value: ServiceConfigError, status: u16) -> Value {
    let (actual, body) = value.response(&Default::default()).unwrap();
    assert_eq!(actual, status);
    let (method, route, _) = USERS_ROUTES
        .iter()
        .find(|(_, _, name)| *name == id)
        .unwrap();
    let document = crate::native_contract::document();
    let schema = &document["paths"][format!("/api/v1{route}")][method.to_ascii_lowercase()]["responses"]
        [status.to_string()]["content"]["application/json"]["schema"];
    assert!(
        jsonschema::is_valid(schema, &body),
        "{id}/{status}: {body} != {schema}"
    );
    body
}
#[tokio::test]
async fn complete_nine_operation_lifecycle_uses_actual_handlers_and_projectors() {
    let (a, d) = api(actor(&["admin"]));
    let created = call(
        &a,
        UsersOperation::Create,
        json!({"email":"New.User","display_name":"New","ignored":true}),
    )
    .await
    .unwrap();
    assert_eq!(created["user_id"], "new_user");
    assert_eq!(created["email"], "new.user@example.test");
    assert_eq!(created["roles"], json!(["user"]));
    assert_eq!(created["created_at"], "2030-01-01T00:00:00");
    assert_eq!(created["department"], Value::Null);
    assert!(created.get("tenant_id").is_none());
    success("create_user", &created);
    let listed = call(
        &a,
        UsersOperation::List {
            page: Value::Null,
            page_size: Value::Null,
            status: None,
            search: None,
        },
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(listed["total"], 3);
    assert_eq!(listed["page"], 1);
    assert_eq!(listed["page_size"], 20);
    assert!(
        listed["users"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["user_id"] != "foreign")
    );
    let filter = d.filters.lock().unwrap()[0].clone();
    assert_eq!(filter.tenant_id.as_deref(), Some("tenant"));
    assert_eq!(filter.offset, 0);
    success("list_users", &listed);
    let got = call(
        &a,
        UsersOperation::Get {
            user_id: "target".into(),
        },
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(got["extra_permissions"], json!(["custom:read"]));
    assert_eq!(got["allowed_services"], json!(["service-a"]));
    assert!(got.get("private_dynamic_column").is_none());
    success("get_user", &got);
    let updated=call(&a,UsersOperation::Update {user_id:"target".into()},json!({"display_name":"Edited","roles":["user"],"extra_permissions":[],"service_access_mode":" ALL ","allowed_services":["service-b","service-b"," "]})).await.unwrap();
    assert_eq!(updated["display_name"], "Edited");
    assert_eq!(updated["allowed_services"], json!(["service-b"]));
    assert_eq!(updated["extra_permissions"], json!([]));
    success("update_user", &updated);
    let mutation = d.mutations.lock().unwrap()[0].clone();
    assert_eq!(mutation.1, "tenant");
    assert_eq!(mutation.2.granted_by.as_deref(), Some("actor"));
    assert!(!mutation.2.profile_only);
    success(
        "reset_user_password",
        &call(
            &a,
            UsersOperation::ResetPassword {
                user_id: "target".into(),
            },
            Value::Null,
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        d.resets.lock().unwrap()[0],
        ("target".into(), "tenant".into(), "actor".into())
    );
    let disabled = call(
        &a,
        UsersOperation::Disable {
            user_id: "target".into(),
        },
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(
        disabled,
        json!({"status":"success","message":"User target disabled"})
    );
    success("disable_user", &disabled);
    assert_eq!(d.rows.lock().unwrap()["target"]["status"], "disabled");
    let enabled = call(
        &a,
        UsersOperation::Enable {
            user_id: "target".into(),
        },
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(
        enabled,
        json!({"status":"success","message":"User target enabled"})
    );
    success("enable_user", &enabled);
    let profile = call(
        &a,
        UsersOperation::Profile {
            user_id: "actor".into(),
        },
        json!({"display_name":"Self"}),
    )
    .await
    .unwrap();
    assert_eq!(profile["display_name"], "Self");
    success("update_user_profile", &profile);
    let deleted = call(
        &a,
        UsersOperation::Delete {
            user_id: "target".into(),
        },
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(
        deleted,
        json!({"status":"success","message":"User target deleted"})
    );
    success("delete_user", &deleted);
    assert_eq!(d.hashes.load(Ordering::SeqCst), 2);
    assert_eq!(d.invalidations.load(Ordering::SeqCst), 2);
    assert_eq!(
        d.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(profile, _)| *profile)
            .count(),
        1
    );
}
#[tokio::test]
async fn profile_uses_own_resolver_without_admin_rbac_and_keeps_legacy_projection() {
    let (a, d) = api(actor(&[]));
    let got = call(
        &a,
        UsersOperation::Profile {
            user_id: "actor".into(),
        },
        json!({"display_name":null,"roles":["admin"],"department":"sales"}),
    )
    .await
    .unwrap();
    assert_eq!(got["display_name"], Value::Null);
    assert_eq!(got["department"], "tech");
    assert_eq!(got["roles"], json!(["user"]));
    assert_eq!(got["service_access_mode"], "all");
    assert_eq!(got["allowed_services"], json!([]));
    success("update_user_profile", &got);
    let mutations = d.mutations.lock().unwrap();
    assert_eq!(
        mutations[0].2.fields,
        json!({"display_name":null}).as_object().unwrap().clone()
    );
    assert!(mutations[0].2.profile_only);
    drop(mutations);
    assert_eq!(
        d.calls.lock().unwrap()[0],
        (true, "/api/v1/users/actor/profile".into())
    );
    error(
        "update_user_profile",
        call(
            &a,
            UsersOperation::Profile {
                user_id: "target".into(),
            },
            json!({}),
        )
        .await
        .unwrap_err(),
        403,
    );
    error(
        "get_user",
        call(
            &a,
            UsersOperation::Get {
                user_id: "actor".into(),
            },
            Value::Null,
        )
        .await
        .unwrap_err(),
        403,
    );
    assert_eq!(
        d.calls.lock().unwrap()[2],
        (false, "/api/v1/users/actor".into())
    );
}
#[tokio::test]
async fn omitted_null_empty_assignments_and_profile_noop_remain_distinct() {
    let (a, d) = api(actor(&["admin"]));
    for input in [
        json!({}),
        json!({"roles":null,"extra_permissions":null,"service_access_mode":null,"allowed_services":null,"denied_services":null}),
    ] {
        let got = call(
            &a,
            UsersOperation::Update {
                user_id: "target".into(),
            },
            input,
        )
        .await
        .unwrap();
        assert_eq!(got["display_name"], "Original");
        assert_eq!(got["extra_permissions"], json!(["custom:read"]));
        assert_eq!(got["allowed_services"], json!(["service-a"]));
    }
    call(
        &a,
        UsersOperation::Profile {
            user_id: "actor".into(),
        },
        json!({"ignored":true}),
    )
    .await
    .unwrap();
    assert!(d.mutations.lock().unwrap().is_empty());
    let got=call(&a,UsersOperation::Update {user_id:"target".into()},json!({"display_name":null,"department":null,"roles":[],"extra_permissions":[],"allowed_services":[]})).await.unwrap();
    assert_eq!(got["display_name"], Value::Null);
    assert_eq!(got["department"], Value::Null);
    assert_eq!(got["roles"], json!([]));
    assert_eq!(got["extra_permissions"], json!([]));
    assert_eq!(got["allowed_services"], json!([]));
    let m = d.mutations.lock().unwrap()[0].2.clone();
    assert_eq!(m.roles, Some(vec![]));
    assert_eq!(m.extra_permissions, Some(vec![]));
    assert_eq!(m.fields["display_name"], Value::Null);
    assert_eq!(m.fields["department"], Value::Null);
    // The HTTP model accepts these nulls; the unchanged real PostgreSQL guard
    // rejects them. This double only proves they reach the mutation unchanged.
    assert!(
        call(
            &a,
            UsersOperation::Update {
                user_id: "target".into()
            },
            json!({"status":null,"tier":null})
        )
        .await
        .is_err()
    );
    let m = d.mutations.lock().unwrap()[1].2.clone();
    assert_eq!(m.fields["status"], Value::Null);
    assert_eq!(m.fields["tier"], Value::Null);
}
#[tokio::test]
async fn admission_rbac_tenant_and_exact_admin_checks_keep_order_and_scope() {
    let (a, d) = api(actor(&[
        "user:create",
        "user:edit",
        "user:delete",
        "user:list",
    ]));
    let got = call(
        &a,
        UsersOperation::Get {
            user_id: "target".into(),
        },
        Value::Null,
    )
    .await
    .unwrap();
    success("get_user", &got);
    for operation in [
        UsersOperation::Create,
        UsersOperation::Update {
            user_id: "target".into(),
        },
        UsersOperation::Delete {
            user_id: "target".into(),
        },
        UsersOperation::ResetPassword {
            user_id: "target".into(),
        },
        UsersOperation::Enable {
            user_id: "target".into(),
        },
        UsersOperation::Disable {
            user_id: "target".into(),
        },
    ] {
        let err = call(&a, operation, json!({"email":"new","display_name":"New"}))
            .await
            .unwrap_err();
        assert_eq!(
            err.response(&Default::default()).unwrap(),
            (403, json!({"detail":"Tenant administrator role required"}))
        );
    }
    assert!(d.mutations.lock().unwrap().is_empty());
    assert_eq!(d.hashes.load(Ordering::SeqCst), 0);
    // Model validation still precedes endpoint RBAC inside execute.
    error(
        "create_user",
        call(&a, UsersOperation::Create, json!({}))
            .await
            .unwrap_err(),
        422,
    );
    let mut empty = actor(&["admin"]);
    empty.tenant_id = " ".into();
    let (a, _) = api(empty);
    error(
        "get_user",
        call(
            &a,
            UsersOperation::Get {
                user_id: "target".into(),
            },
            Value::Null,
        )
        .await
        .unwrap_err(),
        403,
    );
    let (a, d) = api(actor(&["admin"]));
    for (id, op) in [
        (
            "get_user",
            UsersOperation::Get {
                user_id: "foreign".into(),
            },
        ),
        (
            "update_user",
            UsersOperation::Update {
                user_id: "foreign".into(),
            },
        ),
        (
            "delete_user",
            UsersOperation::Delete {
                user_id: "foreign".into(),
            },
        ),
    ] {
        error(id, call(&a, op, json!({})).await.unwrap_err(), 404);
    }
    assert!(d.mutations.lock().unwrap().is_empty());
    assert!(d.deletes.lock().unwrap().is_empty());
}
#[test]
fn shared_request_declarations_match_actual_accepted_and_rejected_models() {
    for (model, input) in [
        (
            "UserCreate",
            json!({"email":" PERSON ","display_name":"名".repeat(255),"unknown":"ignored"}),
        ),
        (
            "UserUpdate",
            json!({"department":null,"roles":null,"service_access_mode":" ALLOWLIST ","status":"unknown","tier":null}),
        ),
        ("ProfileUpdate", json!({"display_name":null,"ignored":true})),
    ] {
        assert!(jsonschema::is_valid(
            &contract::request_schema(model),
            &input
        ));
        assert!(validate_model(model, &input, "example.test").is_ok());
    }
    assert_eq!(
        validate_model(
            "UserCreate",
            &json!({"email":" NAME ","display_name":" "}),
            "example.test"
        )
        .unwrap(),
        json!({"email":"name@example.test","display_name":" ","department":null,"roles":["user"]})
    );
    for input in [
        json!({}),
        json!({"email":null,"display_name":"X"}),
        json!({"email":"x","display_name":""}),
        json!({"email":"x","display_name":"名".repeat(256)}),
        json!({"email":"x","display_name":"X","roles":null}),
        json!({"email":"x","display_name":"X","roles":[1]}),
        json!({"email":"x","display_name":"X","department":"TECH"}),
    ] {
        assert!(!jsonschema::is_valid(
            &contract::request_schema("UserCreate"),
            &input
        ));
        error(
            "create_user",
            validate_model("UserCreate", &input, "example.test").unwrap_err(),
            422,
        );
    }
    // Dynamic normalizing validation is documented, not falsely replaced by a
    // static email format or raw-value enum that rejects accepted input.
    error(
        "create_user",
        validate_model(
            "UserCreate",
            &json!({"email":"x@other.test","display_name":"X"}),
            "example.test",
        )
        .unwrap_err(),
        422,
    );
    error(
        "update_user",
        validate_model(
            "UserUpdate",
            &json!({"service_access_mode":"bogus"}),
            "example.test",
        )
        .unwrap_err(),
        422,
    );
}
#[test]
fn real_projection_defaults_coercion_and_dynamic_source_fields_remain_exact() {
    let got = project_user(
        &json!({"user_id":"minimal","force_password_change":"off","unpublished":{"nested":true}}),
        &[],
        true,
    )
    .unwrap();
    assert_eq!(
        got,
        json!({"user_id":"minimal","email":null,"display_name":null,"username":null,"department":null,"roles":[],"extra_permissions":[],"service_access_mode":"all","allowed_services":[],"denied_services":[],"status":"active","tier":"normal","force_password_change":false,"last_login_at":null,"created_at":null})
    );
    success("get_user", &got);
    for bad in [
        json!({}),
        json!({"user_id":1}),
        json!({"user_id":"x","roles":[null]}),
        json!({"user_id":"x","display_name":{}}),
        json!({"user_id":"x","metadata":{"service_access":{"allowed_services":1}}}),
    ] {
        error("get_user", project_user(&bad, &[], true).unwrap_err(), 500);
    }
    let mut row = row("x", "tenant");
    row["created_at"] = json!("not a date");
    success("get_user", &project_user(&row, &[], true).unwrap());
    assert_eq!(
        project_user(&row, &[], false).unwrap()["allowed_services"],
        json!([])
    );
}
#[test]
fn mounted_route_selector_and_query_rules_cover_all_nine_operations() {
    let uri: axum::http::Uri =
        "/api/v1/users?page=2&page=3&page_size=7&status=old&status=active&search=x&unknown=z"
            .parse()
            .unwrap();
    for (method, route, id) in USERS_ROUTES {
        let path = format!("/api/v1{route}");
        let operation = routed_operation(&path, method, "target".into(), &uri).unwrap();
        let actual = match operation {
            UsersOperation::List {
                page,
                page_size,
                status,
                search,
            } => {
                assert_eq!(page, "3");
                assert_eq!(page_size, "7");
                assert_eq!(status.as_deref(), Some("active"));
                assert_eq!(search.as_deref(), Some("x"));
                "list_users"
            }
            UsersOperation::Create => "create_user",
            UsersOperation::Get { .. } => "get_user",
            UsersOperation::Update { .. } => "update_user",
            UsersOperation::Delete { .. } => "delete_user",
            UsersOperation::ResetPassword { .. } => "reset_user_password",
            UsersOperation::Enable { .. } => "enable_user",
            UsersOperation::Disable { .. } => "disable_user",
            UsersOperation::Profile { .. } => "update_user_profile",
        };
        assert_eq!(actual, *id);
        assert!(contract::allowed_methods(&path).contains(method));
        assert!(!contract::allowed_methods(&path).contains(&"PATCH"));
    }
    assert_eq!(
        validate_pagination(&Value::Null, &Value::Null).unwrap(),
        (1, 20)
    );
    assert_eq!(
        validate_pagination(&json!(" 2 "), &json!(100)).unwrap(),
        (2, 100)
    );
    for (page, size) in [
        (json!(0), json!(20)),
        (json!(1), json!(101)),
        (json!("bad"), json!(20)),
        (json!(1.5), json!(20)),
    ] {
        error(
            "list_users",
            validate_pagination(&page, &size).unwrap_err(),
            422,
        );
    }
    let (a, _) = api(actor(&["admin"]));
    let _router = router(Arc::new(a), Arc::new(Cancel));
}
#[tokio::test]
async fn observed_preflight_errors_and_uncertain_post_write_failure_are_truthful() {
    let (a, d) = api(actor(&["admin"]));
    error(
        "create_user",
        call(
            &a,
            UsersOperation::Create,
            json!({"email":"target","display_name":"X"}),
        )
        .await
        .unwrap_err(),
        400,
    );
    error(
        "delete_user",
        call(
            &a,
            UsersOperation::Delete {
                user_id: "actor".into(),
            },
            Value::Null,
        )
        .await
        .unwrap_err(),
        400,
    );
    error(
        "disable_user",
        call(
            &a,
            UsersOperation::Disable {
                user_id: "actor".into(),
            },
            Value::Null,
        )
        .await
        .unwrap_err(),
        400,
    );
    d.fail_invalidation.store(true, Ordering::SeqCst);
    error(
        "update_user",
        call(
            &a,
            UsersOperation::Update {
                user_id: "target".into(),
            },
            json!({"allowed_services":["committed"]}),
        )
        .await
        .unwrap_err(),
        500,
    );
    assert_eq!(
        d.rows.lock().unwrap()["target"]["metadata"]["service_access"]["allowed_services"],
        json!(["committed"])
    );
    assert_eq!(d.mutations.lock().unwrap().len(), 1);
    let read = call(
        &a,
        UsersOperation::Get {
            user_id: "target".into(),
        },
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(read["allowed_services"], json!(["committed"]));
    let (mut a, _) = api(actor(&["admin"]));
    a.dependencies.configuration = UsersConfiguration::new(String::new(), "example.test".into());
    error(
        "reset_user_password",
        call(
            &a,
            UsersOperation::ResetPassword {
                user_id: "target".into(),
            },
            Value::Null,
        )
        .await
        .unwrap_err(),
        503,
    );
    a.dependencies.database = None;
    error(
        "get_user",
        call(
            &a,
            UsersOperation::Get {
                user_id: "target".into(),
            },
            Value::Null,
        )
        .await
        .unwrap_err(),
        503,
    );
}
struct Cancel;
impl ServiceConfigCancellation for Cancel {
    fn cancelled(&self) -> ServiceConfigCancellationFuture<'_> {
        Box::pin(async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"host":"cancelled"})),
            )
                .into_response()
        })
    }
}
#[tokio::test]
async fn real_response_renderer_keeps_success_200_redaction_and_host_cancellation() {
    let (a, _) = api(actor(&["admin"]));
    let state = HttpState {
        api: Arc::new(a),
        cancellation: Arc::new(Cancel),
    };
    let response = response(&state, Ok(contract::enabled("target")), &Default::default()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        contract::enabled("target")
    );
    let redacted = error(
        "get_user",
        ServiceConfigError::Exception("private database data".into()),
        500,
    );
    assert!(!redacted.to_string().contains("private"));
    let cancelled = super::response(
        &state,
        Err(ServiceConfigError::Cancelled),
        &Default::default(),
    )
    .await;
    assert_eq!(cancelled.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(cancelled.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"host":"cancelled"})
    );
    let document = crate::native_contract::document();
    assert_eq!(
        document["paths"]["/api/v1/users"]["get"]["responses"]["default"]["x-native-undeclared"],
        true
    );
}

struct FailedIdentity(u16);
impl ServiceConfigIdentityResolver for FailedIdentity {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        Box::pin(async move {
            Err(ServiceConfigError::Http {
                status: self.0,
                body: json!({"error":"fixture admission failure"}),
            })
        })
    }
}
impl UsersProfileIdentityResolver for FailedIdentity {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        ServiceConfigIdentityResolver::resolve(self, headers, path)
    }
}
#[tokio::test]
async fn resolver_failures_preserve_admission_envelopes_and_precede_model_validation() {
    let (mut a, d) = api(actor(&["admin"]));
    a.dependencies.identity = Arc::new(FailedIdentity(401));
    // Invalid body does not replace an authentication failure with model 422.
    error(
        "create_user",
        call(&a, UsersOperation::Create, json!({}))
            .await
            .unwrap_err(),
        401,
    );
    call(
        &a,
        UsersOperation::Profile {
            user_id: "actor".into(),
        },
        json!({}),
    )
    .await
    .unwrap();
    a.dependencies.profile_identity = Arc::new(FailedIdentity(503));
    error(
        "update_user_profile",
        call(
            &a,
            UsersOperation::Profile {
                user_id: "actor".into(),
            },
            json!({"display_name": 1}),
        )
        .await
        .unwrap_err(),
        503,
    );
    assert!(d.mutations.lock().unwrap().is_empty());
    assert_eq!(d.hashes.load(Ordering::SeqCst), 0);
}
