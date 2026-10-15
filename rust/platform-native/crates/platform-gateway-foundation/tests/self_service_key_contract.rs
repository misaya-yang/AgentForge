//! Real production parser/projection/router with in-memory collaborators only.
//! No listeners, entropy generation, secrets, database or network.
use ai_platform_gateway_foundation::{
    native_contract::api_keys as contract,
    self_service_api_keys::{
        CreateKey, KeyFilter, KeyFuture, SelfServiceApiKeyError as Error, SelfServiceApiKeyService,
    },
    self_service_api_keys_api::{
        self as api, SelfServiceKeyDependencies, SelfServiceKeyIdentity,
        SelfServiceKeyIdentityResolver, SelfServiceKeyInvalidation, SelfServiceKeysApi,
    },
    service_config_api::{ServiceConfigCancellation, ServiceConfigCancellationFuture},
};
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
fn row() -> Value {
    json!({"key_id":"fixture-id","name":"fixture","is_active":true,"created_at":"2026-10-14T00:00:00Z",
        "user_id":"owner","tenant_id":"tenant","key_hash":"synthetic-hash-canary","id":17,
        "api_key":"synthetic-not-a-valid-key","warning":"fixture-warning","extra":"drop"})
}
fn valid(schema: Value, value: &Value) -> bool {
    jsonschema::validator_for(&schema).unwrap().is_valid(value)
}
fn document() -> Value {
    let mut d = json!({"paths":{}});
    contract::extend_document(&mut d);
    d
}
#[test]
fn actual_projection_inserts_nulls_and_strips_storage_and_plaintext() {
    let read = api::project_response(&row(), false).unwrap();
    assert!(valid(contract::key_schema(false), &read));
    assert_eq!(read.as_object().unwrap().len(), 15);
    for field in ["key_hash", "tenant_id", "id", "api_key", "warning", "extra"] {
        assert!(read.get(field).is_none());
    }
    for field in [
        "key_prefix",
        "description",
        "derived_user_id",
        "roles",
        "tier",
        "last_used_at",
        "expires_at",
    ] {
        assert!(read[field].is_null());
    }
    assert_eq!(read["use_count"], 0);
    let created = api::project_response(&row(), true).unwrap();
    assert!(valid(contract::key_schema(true), &created));
    assert!(!valid(contract::key_schema(false), &created));
    assert_eq!(created["api_key"], "synthetic-not-a-valid-key");
    for arbitrary in [
        Value::Null,
        json!(false),
        json!(17),
        json!(["x"]),
        json!({"custom":true}),
    ] {
        let mut r = row();
        r["rate_limit"] = arbitrary.clone();
        let v = api::project_response(&r, false).unwrap();
        assert_eq!(v["rate_limit"], arbitrary);
        assert!(valid(contract::key_schema(false), &v));
    }
    for field in ["key_id", "name", "is_active", "created_at"] {
        let mut r = row();
        r.as_object_mut().unwrap().remove(field);
        assert!(api::project_response(&r, false).is_err());
    }
    let mut r = row();
    r["roles"] = json!([7]);
    assert!(api::project_response(&r, false).is_err());
}
#[test]
fn actual_create_parser_and_broad_timestamp_boundary() {
    for extra in [
        json!({}),
        json!({"scopes":null}),
        json!({"scopes":[]}),
        json!({"scopes":["knowledge:read","knowledge:read"]}),
        json!({"expires_at":0}),
        json!({"expires_at":"0"}),
        json!({"expires_at":"2026-10-14"}),
        json!({"tier":null,"description":null}),
    ] {
        let mut v = json!({"name":" ","unknown":"ignored"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(api::validate_create(&v).is_ok(), "{v}");
        assert!(valid(contract::request_schema(), &v));
    }
    for v in [
        json!({}),
        json!({"name":""}),
        json!({"name":"x".repeat(contract::NAME_MAX+1)}),
        json!({"name":"x","description":"x".repeat(contract::DESCRIPTION_MAX+1)}),
        json!({"name":"x","scopes":[3]}),
        json!({"name":"x","tier":3}),
    ] {
        assert!(api::validate_create(&v).is_err());
        assert!(!valid(contract::request_schema(), &v));
    }
    let broad = json!({"name":"x","expires_at":"not-a-date"});
    assert!(valid(contract::request_schema(), &broad));
    assert!(api::validate_create(&broad).is_err());
    // Authorization owns scope denial; syntactically valid unknown scopes reach it.
    let denied = json!({"name":"x","scopes":["arbitrary:write"]});
    assert!(valid(contract::request_schema(), &denied));
    assert!(api::validate_create(&denied).is_ok());
}
struct Fixture {
    events: Mutex<Vec<String>>,
    paths: Mutex<Vec<String>>,
    admin: bool,
    unauthorized: bool,
    bad_projection: bool,
    invalidate_fail: bool,
}
impl Fixture {
    fn record(&self, s: impl Into<String>) {
        self.events.lock().unwrap().push(s.into());
    }
}
impl SelfServiceKeyIdentityResolver for Fixture {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        path: &'a str,
    ) -> KeyFuture<'a, SelfServiceKeyIdentity> {
        Box::pin(async move {
            self.paths.lock().unwrap().push(path.to_owned());
            self.record("identity");
            if self.unauthorized {
                return Err(Error::detail(401, "denied"));
            }
            Ok(SelfServiceKeyIdentity {
                user_id: "owner".into(),
                tenant_id: "tenant".into(),
                roles: if self.admin {
                    vec!["admin".into()]
                } else {
                    vec![]
                },
                tier: "verified-tier".into(),
                is_authenticated: true,
            })
        })
    }
}
impl SelfServiceApiKeyService for Fixture {
    fn create<'a>(&'a self, key: &'a CreateKey) -> KeyFuture<'a, Value> {
        Box::pin(async move {
            assert_eq!(
                (
                    key.user_id.as_str(),
                    key.tenant_id.as_str(),
                    key.tier.as_str()
                ),
                ("owner", "tenant", "verified-tier")
            );
            assert_eq!(key.scopes, vec!["knowledge:read", "knowledge:write"]);
            self.record("create");
            Ok(row())
        })
    }
    fn list<'a>(&'a self, filter: &'a KeyFilter) -> KeyFuture<'a, Vec<Value>> {
        Box::pin(async move {
            assert_eq!(filter.tenant_id.as_deref(), Some("tenant"));
            assert_eq!(
                filter.user_id.as_deref(),
                if self.admin { None } else { Some("owner") }
            );
            self.record(format!("list:{}", filter.include_inactive));
            Ok(vec![if self.bad_projection {
                json!({})
            } else {
                row()
            }])
        })
    }
    fn get<'a>(&'a self, id: &'a str, tenant: Option<&'a str>) -> KeyFuture<'a, Option<Value>> {
        Box::pin(async move {
            assert_eq!(tenant, Some("tenant"));
            self.record("get");
            if id == "missing" || id == "other-tenant" {
                return Ok(None);
            }
            let mut r = row();
            if id == "foreign" {
                r["user_id"] = json!("other-owner");
            }
            Ok(Some(r))
        })
    }
    fn revoke<'a>(&'a self, id: &'a str, f: &'a KeyFilter) -> KeyFuture<'a, bool> {
        Box::pin(async move {
            assert_eq!(f.tenant_id.as_deref(), Some("tenant"));
            assert_eq!(
                f.user_id.as_deref(),
                if self.admin { None } else { Some("owner") }
            );
            self.record("revoke");
            Ok(id == "fixture-id")
        })
    }
    fn delete<'a>(&'a self, id: &'a str, f: &'a KeyFilter) -> KeyFuture<'a, bool> {
        Box::pin(async move {
            assert_eq!(f.tenant_id.as_deref(), Some("tenant"));
            assert_eq!(
                f.user_id.as_deref(),
                if self.admin { None } else { Some("owner") }
            );
            self.record("delete");
            Ok(id == "fixture-id")
        })
    }
}
impl SelfServiceKeyInvalidation for Fixture {
    fn clear(&self) -> Result<(), Error> {
        self.record("invalidate");
        if self.invalidate_fail {
            Err(Error::Internal)
        } else {
            Ok(())
        }
    }
}
impl ServiceConfigCancellation for Fixture {
    fn cancelled(&self) -> ServiceConfigCancellationFuture<'_> {
        panic!("unexpected cancellation")
    }
}
fn fixture() -> Fixture {
    Fixture {
        events: Mutex::new(vec![]),
        paths: Mutex::new(vec![]),
        admin: false,
        unauthorized: false,
        bad_projection: false,
        invalidate_fail: false,
    }
}
async fn request(
    f: Arc<Fixture>,
    present: bool,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, Value) {
    let service: Option<Arc<dyn SelfServiceApiKeyService>> =
        if present { Some(f.clone()) } else { None };
    let router = api::router(
        Arc::new(SelfServiceKeysApi::new(SelfServiceKeyDependencies {
            identity: f.clone(),
            service,
            invalidation: f.clone(),
        })),
        f,
    );
    let r = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status().as_u16();
    let bytes = to_bytes(r.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn mounted_routes_success_authority_and_secret_projection() {
    let doc = document();
    for (method, path, body, template) in [
        (
            "POST",
            contract::BASE,
            r#"{"name":"fixture","user_id":"spoof","tenant_id":"other","tier":"spoof"}"#,
            contract::BASE,
        ),
        (
            "GET",
            "/api/v1/api-keys?include_inactive=false&include_inactive=true",
            "",
            contract::BASE,
        ),
        ("GET", "/api/v1/api-keys/fixture-id", "", contract::KEY),
        (
            "POST",
            "/api/v1/api-keys/fixture-id/revoke",
            "",
            contract::REVOKE,
        ),
        ("DELETE", "/api/v1/api-keys/fixture-id", "", contract::KEY),
    ] {
        let f = Arc::new(fixture());
        let (status, v) = request(f.clone(), true, method, path, body).await;
        assert_eq!(status, 200, "{method} {path}: {v}");
        assert!(valid(doc["paths"][template][method.to_lowercase()]["responses"]["200"]["content"]["application/json"]["schema"].clone(),&v));
        if !(method == "POST" && template == contract::BASE) {
            assert!(!v.to_string().contains("synthetic-not-a-valid-key"));
        }
        assert_eq!(
            *f.paths.lock().unwrap(),
            vec![path.split('?').next().unwrap()]
        );
        if path.contains('?') {
            assert!(f.events.lock().unwrap().contains(&"list:true".into()));
        }
    }
}
#[tokio::test]
async fn malformed_json_database_identity_and_query_ordering() {
    let f = Arc::new(Fixture {
        unauthorized: true,
        ..fixture()
    });
    assert_eq!(
        request(f.clone(), false, "POST", contract::BASE, "{")
            .await
            .0,
        422
    );
    assert!(f.events.lock().unwrap().is_empty());
    assert_eq!(
        request(f.clone(), false, "POST", contract::BASE, "{}")
            .await
            .0,
        503
    );
    assert!(f.events.lock().unwrap().is_empty());
    assert_eq!(
        request(f.clone(), true, "POST", contract::BASE, "{}")
            .await
            .0,
        401
    );
    assert_eq!(
        request(
            f.clone(),
            true,
            "GET",
            "/api/v1/api-keys?include_inactive=bad",
            ""
        )
        .await
        .0,
        401
    );
    let f = Arc::new(fixture());
    let (status, v) = request(
        f.clone(),
        true,
        "GET",
        "/api/v1/api-keys?include_inactive=bad",
        "",
    )
    .await;
    assert_eq!(status, 422);
    assert!(v["detail"].is_array());
    assert_eq!(*f.events.lock().unwrap(), vec!["identity"]);
}
#[tokio::test]
async fn owner_scope_and_issuance_restrictions_are_not_expanded() {
    for admin in [false, true] {
        let f = Arc::new(Fixture { admin, ..fixture() });
        let (status, v) = request(
            f.clone(),
            true,
            "POST",
            contract::BASE,
            r#"{"name":"x","scopes":["arbitrary:write"]}"#,
        )
        .await;
        assert_eq!(status, 403);
        assert!(v["detail"].is_object());
        assert_eq!(*f.events.lock().unwrap(), vec!["identity"]);
        let (status, _) = request(f.clone(), true, "GET", "/api/v1/api-keys/foreign", "").await;
        assert_eq!(status, if admin { 200 } else { 403 });
        assert_eq!(
            request(f.clone(), true, "GET", "/api/v1/api-keys/other-tenant", "")
                .await
                .0,
            404
        );
        assert_eq!(
            request(
                f.clone(),
                true,
                "POST",
                "/api/v1/api-keys/foreign/revoke",
                ""
            )
            .await
            .0,
            404
        );
        assert_eq!(
            request(f.clone(), true, "DELETE", "/api/v1/api-keys/foreign", "")
                .await
                .0,
            404
        );
        assert_eq!(
            request(f.clone(), true, "GET", contract::BASE, "").await.0,
            200
        );
    }
}
#[tokio::test]
async fn post_write_invalidation_and_projection_failures_remain_errors() {
    let f = Arc::new(Fixture {
        invalidate_fail: true,
        ..fixture()
    });
    let (status, v) = request(f.clone(), true, "POST", contract::BASE, r#"{"name":"x"}"#).await;
    assert_eq!(status, 500);
    assert_eq!(v["error"]["code"], 2000);
    assert_eq!(
        *f.events.lock().unwrap(),
        vec!["identity", "create", "invalidate"]
    );
    assert!(!v.to_string().contains("synthetic-not-a-valid-key"));
    let f = Arc::new(Fixture {
        bad_projection: true,
        ..fixture()
    });
    assert_eq!(request(f, true, "GET", contract::BASE, "").await.0, 500);
}

#[test]
fn exact_five_operations_authority_and_error_boundaries() {
    let d = document();
    assert_eq!(d["paths"].as_object().unwrap().len(), 3);
    let count: usize = d["paths"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_object().unwrap().len())
        .sum();
    assert_eq!(count, 5);
    for (path, method, id, forbidden) in [
        (
            contract::BASE,
            "post",
            "create_api_key_api_v1_api_keys_post",
            true,
        ),
        (
            contract::BASE,
            "get",
            "list_api_keys_api_v1_api_keys_get",
            false,
        ),
        (
            contract::KEY,
            "get",
            "get_api_key_detail_api_v1_api_keys__key_id__get",
            true,
        ),
        (
            contract::KEY,
            "delete",
            "delete_api_key_api_v1_api_keys__key_id__delete",
            false,
        ),
        (
            contract::REVOKE,
            "post",
            "revoke_api_key_api_v1_api_keys__key_id__revoke_post",
            false,
        ),
    ] {
        let op = &d["paths"][path][method];
        assert_eq!(op["operationId"], id);
        assert_eq!(
            op["security"],
            json!([{"NativeBearerAuth":[]},{"NativeBoundApiKey":[]}])
        );
        assert_eq!(op["responses"].get("403").is_some(), forbidden);
        assert_eq!(op["responses"]["default"]["x-native-undeclared"], true);
        assert!(op["responses"].get("201").is_none());
        if path != contract::BASE {
            assert_eq!(op["parameters"][0]["schema"], json!({"type":"string"}));
        }
    }
    assert!(d["paths"].get("/api/v1/config/api-keys").is_none());
}
