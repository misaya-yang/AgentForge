//! Source self-service security assertions through actual key API orchestration.
use ai_platform_gateway_foundation::{self_service_api_keys::*, self_service_api_keys_api::*};
use axum::http::HeaderMap;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Fixture {
    admin: bool,
    calls: Mutex<Vec<String>>,
    created: Mutex<Option<CreateKey>>,
}
fn row() -> Value {
    json!({"key_id":"ak_test","name":"unprivileged-key","user_id":"user-1","tenant_id":"tenant-1","is_active":true,"created_at":"2026-10-15T00:00:00Z","api_key":"synthetic-fixture-not-a-real-key","warning":"fixture"})
}
impl SelfServiceKeyIdentityResolver for Fixture {
    fn resolve<'a>(
        &'a self,
        _: &'a HeaderMap,
        _: &'a str,
    ) -> KeyFuture<'a, SelfServiceKeyIdentity> {
        Box::pin(async move {
            Ok(SelfServiceKeyIdentity {
                user_id: "user-1".into(),
                tenant_id: "tenant-1".into(),
                roles: vec![if self.admin { "admin" } else { "user" }.into()],
                tier: "normal".into(),
                is_authenticated: true,
            })
        })
    }
}
impl Fixture {
    fn filter(&self, f: &KeyFilter) {
        assert_eq!(f.tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(
            f.user_id.as_deref(),
            if self.admin { None } else { Some("user-1") }
        );
    }
}
impl SelfServiceApiKeyService for Fixture {
    fn create<'a>(&'a self, k: &'a CreateKey) -> KeyFuture<'a, Value> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("create".into());
            *self.created.lock().unwrap() = Some(k.clone());
            Ok(row())
        })
    }
    fn list<'a>(&'a self, f: &'a KeyFilter) -> KeyFuture<'a, Vec<Value>> {
        Box::pin(async move {
            self.filter(f);
            assert!(!f.include_inactive);
            self.calls.lock().unwrap().push("list".into());
            Ok(vec![])
        })
    }
    fn get<'a>(&'a self, id: &'a str, tenant: Option<&'a str>) -> KeyFuture<'a, Option<Value>> {
        Box::pin(async move {
            assert_eq!(id, "ak_test");
            assert_eq!(tenant, Some("tenant-1"));
            self.calls.lock().unwrap().push("get".into());
            Ok(Some(row()))
        })
    }
    fn revoke<'a>(&'a self, id: &'a str, f: &'a KeyFilter) -> KeyFuture<'a, bool> {
        Box::pin(async move {
            assert_eq!(id, "ak_test");
            self.filter(f);
            self.calls.lock().unwrap().push("revoke".into());
            Ok(true)
        })
    }
    fn delete<'a>(&'a self, id: &'a str, f: &'a KeyFilter) -> KeyFuture<'a, bool> {
        Box::pin(async move {
            assert_eq!(id, "ak_test");
            self.filter(f);
            self.calls.lock().unwrap().push("delete".into());
            Ok(true)
        })
    }
}
impl SelfServiceKeyInvalidation for Fixture {
    fn clear(&self) -> Result<(), SelfServiceApiKeyError> {
        self.calls.lock().unwrap().push("invalidate".into());
        Ok(())
    }
}
fn api(f: &Arc<Fixture>) -> SelfServiceKeysApi {
    SelfServiceKeysApi::new(SelfServiceKeyDependencies {
        identity: f.clone(),
        service: Some(f.clone()),
        invalidation: f.clone(),
    })
}
#[tokio::test]
async fn self_service_rejects_privileged_management_scopes_before_create() {
    for scope in ["admin", "admin:*", "console:services:edit"] {
        let f = Arc::new(Fixture::default());
        let error = api(&f)
            .handle(
                SelfServiceKeyOperation::Create,
                &HeaderMap::new(),
                &json!({"name":"unprivileged-key","scopes":[scope]}),
            )
            .await
            .unwrap_err();
        match error {
            SelfServiceApiKeyError::Http { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body["detail"]["disallowed_scopes"], json!([scope]));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(f.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn self_service_ignores_forged_tier_and_invalidates_once() {
    let f = Arc::new(Fixture::default());
    let result = api(&f)
        .handle(
            SelfServiceKeyOperation::Create,
            &HeaderMap::new(),
            &json!({"name":"unprivileged-key","scopes":["knowledge:read"],"tier":"admin"}),
        )
        .await
        .unwrap();
    assert_eq!(result["key_id"], "ak_test");
    let created = f.created.lock().unwrap();
    let key = created.as_ref().unwrap();
    assert_eq!(key.scopes, ["knowledge:read"]);
    assert_eq!(key.tier, "normal");
    assert_ne!(key.tier, "admin");
    assert_eq!(*f.calls.lock().unwrap(), ["create", "invalidate"]);
}
#[tokio::test]
async fn management_operations_always_apply_authenticated_tenant_even_for_admin() {
    let f = Arc::new(Fixture {
        admin: true,
        ..Default::default()
    });
    let service = api(&f);
    for operation in [
        SelfServiceKeyOperation::List {
            include_inactive: Value::Null,
        },
        SelfServiceKeyOperation::Get {
            key_id: "ak_test".into(),
        },
        SelfServiceKeyOperation::Revoke {
            key_id: "ak_test".into(),
        },
        SelfServiceKeyOperation::Delete {
            key_id: "ak_test".into(),
        },
    ] {
        service
            .handle(operation, &HeaderMap::new(), &Value::Null)
            .await
            .unwrap();
    }
    assert_eq!(
        *f.calls.lock().unwrap(),
        [
            "list",
            "get",
            "revoke",
            "invalidate",
            "delete",
            "invalidate"
        ]
    );
}
