//! Source ModelService assertion matrices at the real service's parameterized SQL boundary.
use ai_platform_gateway_foundation::model_service::*;
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[derive(Clone, Default)]
struct Database {
    rows: Arc<Mutex<VecDeque<Option<ModelRow>>>>,
    queries: Arc<Mutex<Vec<ModelQuery>>>,
}
impl ModelDatabase for Database {
    fn fetchrow(&self, q: ModelQuery) -> ModelFuture<'_, Option<ModelRow>> {
        Box::pin(async move {
            self.queries.lock().unwrap().push(q);
            Ok(self
                .rows
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected SQL call"))
        })
    }
    fn fetch(&self, q: ModelQuery) -> ModelFuture<'_, Vec<ModelRow>> {
        Box::pin(async move {
            self.queries.lock().unwrap().push(q);
            Ok(self.rows.lock().unwrap().drain(..).flatten().collect())
        })
    }
    fn execute(&self, _: ModelQuery) -> ModelFuture<'_, String> {
        Box::pin(async { panic!("unexpected mutation") })
    }
}
#[derive(Clone, Default)]
struct Pricing(Arc<AtomicUsize>);
impl ModelPricingCache for Pricing {
    fn invalidate_cache(&self) -> Result<(), ModelServiceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn row() -> ModelRow {
    json!({"model_id":"custom-model","tenant_id":"tenant-a","provider_id":"test-provider","display_name":"Custom","supports_vision":false,"supports_tools":true,"input_price_per_1k":"0","output_price_per_1k":"0","catalog_capabilities":"{}","capability_overrides":"{}","capability_revision":3}).as_object().unwrap().clone()
}
fn setup(rows: Vec<Option<ModelRow>>) -> (ModelService<Database, Pricing>, Database, Pricing) {
    let db = Database::default();
    *db.rows.lock().unwrap() = rows.into();
    let p = Pricing::default();
    (ModelService::new(db.clone(), p.clone()), db, p)
}
#[test]
fn snapshot_is_scoped_precise_versioned_and_decodes_capability_json() {
    let (s, _, _) = setup(vec![]);
    let mut r = row();
    r.insert("input_price_per_1k".into(), json!("0.000000000123456789"));
    let first = s.row_to_dict(Some(r.clone())).unwrap();
    let mut other = r.clone();
    other.insert("tenant_id".into(), json!("other"));
    let other = s.row_to_dict(Some(other)).unwrap();
    assert_eq!(
        first["pricing_snapshot"]["input_price_per_1k"],
        "0.000000000123456789"
    );
    assert_ne!(
        first["pricing_snapshot"]["version"],
        other["pricing_snapshot"]["version"]
    );
    assert_eq!(
        s.row_to_dict(Some(r)).unwrap()["pricing_snapshot"],
        first["pricing_snapshot"]
    );
    assert!(first["catalog_capabilities"].is_object());
    assert_eq!(first["effective_capabilities"]["schema_version"], 1);
}
#[tokio::test]
async fn ambiguous_model_requires_provider_before_any_mutation() {
    for operation in ["get", "update", "delete"] {
        let mut r = row();
        r.insert("provider_matches".into(), json!(2));
        let (s, db, p) = setup(vec![Some(r)]);
        let error = match operation {
            "get" => s
                .get_model("tenant-a", "custom-model", None)
                .await
                .unwrap_err(),
            "update" => s
                .update_model("tenant-a", "custom-model", None, UpdateModel::default())
                .await
                .unwrap_err(),
            _ => s
                .delete_model("tenant-a", "custom-model", None)
                .await
                .unwrap_err(),
        };
        assert_eq!(error, ModelServiceError::ProviderAmbiguous);
        assert_eq!(error.to_string(), "model_provider_required");
        assert_eq!(db.queries.lock().unwrap().len(), 1);
        assert_eq!(p.0.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn create_validates_builtin_and_safe_profiles_before_sql() {
    for (provider, model, option, valid) in [
        ("dashscope", "qwen3.7-plus", "medium", true),
        ("dashscope", "qwen3.7-plus", "does-not-exist", false),
        ("test-provider", "custom-model", "medium", false),
    ] {
        let mut r = row();
        r.insert("model_id".into(), json!(model));
        r.insert("provider_id".into(), json!(provider));
        let (s, db, _) = setup(vec![Some(r)]);
        let data = CreateModel {
            capability_overrides: json!({"reasoning":{"default_option":option}})
                .as_object()
                .unwrap()
                .clone(),
            ..Default::default()
        };
        let result = s.create_model("tenant-a", model, provider, data).await;
        if valid {
            assert_eq!(result.unwrap()["model_id"], model);
            assert_eq!(db.queries.lock().unwrap().len(), 1);
        } else {
            assert_eq!(result.unwrap_err(), ModelServiceError::Capability);
            assert!(db.queries.lock().unwrap().is_empty());
        }
    }
}
#[tokio::test]
async fn update_revision_guard_and_value_conditional_bump() {
    for (changed, tools, overrides) in [
        (true, true, json!({"reasoning":{"default_option":"off"}})),
        (false, true, json!({})),
        (true, false, json!({})),
    ] {
        let mut updated = row();
        updated.insert(
            "capability_revision".into(),
            json!(if changed { 4 } else { 3 }),
        );
        let (s, db, _) = setup(vec![Some(row()), Some(updated)]);
        let result = s
            .update_model(
                "tenant-a",
                "custom-model",
                Some("test-provider"),
                UpdateModel {
                    supports_tools: Some(tools),
                    supports_vision: Some(false),
                    capability_overrides: Some(overrides.as_object().unwrap().clone()),
                    expected_capability_revision: Some(3),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(result.is_some());
        let q = db.queries.lock().unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(
            q[1].sql
                .contains("capability_revision = capability_revision + 1"),
            changed
        );
        assert!(q[1].sql.contains("AND capability_revision = $"));
        assert_eq!(q[1].params.last(), Some(&ModelParam::Integer(3)));
        assert!(q[1].sql.contains("capability_overrides ="));
    }
}
#[tokio::test]
async fn stale_revision_fails_before_write_and_guarded_miss_distinguishes_deleted_row() {
    for kind in ["stale", "conflict", "deleted"] {
        let mut newer = row();
        newer.insert("capability_revision".into(), json!(4));
        let rows = match kind {
            "stale" => vec![Some(newer)],
            "conflict" => vec![Some(row()), None, Some(newer)],
            _ => vec![Some(row()), None, None],
        };
        let (s, db, p) = setup(rows);
        let result = s
            .update_model(
                "tenant-a",
                "custom-model",
                Some("test-provider"),
                UpdateModel {
                    supports_tools: Some(true),
                    expected_capability_revision: Some(3),
                    ..Default::default()
                },
            )
            .await;
        if kind == "deleted" {
            assert!(result.unwrap().is_none());
        } else {
            assert_eq!(
                result.unwrap_err(),
                ModelServiceError::CapabilityRevisionConflict
            );
        }
        assert_eq!(
            db.queries.lock().unwrap().len(),
            if kind == "stale" { 1 } else { 3 }
        );
        assert_eq!(p.0.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn upsert_reports_only_revision_delta_or_creation_as_capability_change() {
    for kind in ["unchanged", "changed", "created"] {
        let mut updated = row();
        updated.insert(
            "capability_revision".into(),
            json!(if kind == "created" {
                1
            } else if kind == "changed" {
                4
            } else {
                3
            }),
        );
        let rows = if kind == "created" {
            vec![None, Some(updated)]
        } else {
            vec![Some(row()), Some(row()), Some(updated)]
        };
        let (s, _, _) = setup(rows);
        let mut fields = ModelFields::default();
        fields.display_name = "Custom".into();
        if kind == "changed" {
            fields.catalog_capabilities = Some(
                json!({"reasoning":{"default_option":"off"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            );
        }
        let (status, _, changed) = s
            .upsert_model_from_catalog("tenant-a", "test-provider", "custom-model", fields, true)
            .await
            .unwrap();
        assert_eq!(
            status,
            if kind == "created" {
                "created"
            } else {
                "updated"
            }
        );
        assert_eq!(changed, kind != "unchanged");
    }
}
#[tokio::test]
async fn tenant_model_create_update_startup_only_invalidate_global_pricing_cache() {
    for operation in ["create", "update", "startup"] {
        let mut r = row();
        r.insert("tenant_id".into(), json!("test-tenant"));
        r.insert("provider_id".into(), json!("openai"));
        r.insert("model_id".into(), json!("gpt-4o"));
        r.insert("input_price_per_1k".into(), json!("0.0025"));
        r.insert("output_price_per_1k".into(), json!("0.01"));
        let rows = if operation == "update" {
            vec![Some(r.clone()), Some(r.clone())]
        } else {
            vec![Some(r.clone())]
        };
        let (s, db, p) = setup(rows);
        let result = match operation {
            "create" => s
                .create_model("test-tenant", "gpt-4o", "openai", CreateModel::default())
                .await
                .unwrap(),
            "update" => s
                .update_model(
                    "test-tenant",
                    "gpt-4o",
                    None,
                    UpdateModel {
                        input_price_per_1k: Some("0.0025".parse().unwrap()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
                .unwrap(),
            _ => {
                assert_eq!(
                    s.sync_pricing_from_llm_models("test-tenant", false)
                        .await
                        .unwrap(),
                    1
                );
                s.row_to_dict(Some(r)).unwrap()
            }
        };
        assert_eq!(p.0.load(Ordering::SeqCst), 1);
        let receipt = &result["pricing_snapshot"];
        assert_eq!(receipt["tenant_id"], "test-tenant");
        assert_eq!(receipt["provider_id"], "openai");
        assert_eq!(receipt["input_price_per_1k"], "0.0025");
        let queries = db.queries.lock().unwrap();
        assert!(queries.iter().all(|q| !q.sql.contains("model_pricing")));
        if operation == "update" {
            let q = queries.last().unwrap();
            assert!(q.sql.contains("provider_id ="));
            assert!(q.params.contains(&ModelParam::Text("openai".into())));
        }
    }
}
