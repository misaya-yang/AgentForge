//! The production service builds and executes its actual parameterized queries.
//! This in-memory SQL-boundary fixture is not PostgreSQL/concurrency acceptance.
use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct PolicyDatabase(Arc<Mutex<PolicyState>>);
#[derive(Default)]
struct PolicyState {
    rows: Vec<ModelRow>,
    queries: Vec<String>,
    admin_edit_before_update: bool,
}
impl PolicyDatabase {
    fn new(rows: Vec<ModelRow>) -> Self {
        Self(Arc::new(Mutex::new(PolicyState {
            rows,
            ..Default::default()
        })))
    }
}
fn value(param: &ModelParam) -> Value {
    match param {
        ModelParam::Text(v) => json!(v),
        ModelParam::Integer(v) => json!(v),
        ModelParam::Bool(v) => json!(v),
        ModelParam::Decimal(v) => json!(v.to_string()),
        ModelParam::Jsonb(v) => serde_json::from_str(v).unwrap(),
        ModelParam::Timestamp(v) => json!(v.to_rfc3339()),
    }
}
fn bound(token: &str, params: &[ModelParam]) -> Value {
    let token = token.trim();
    if let Some(index) = token.strip_prefix('$') {
        value(&params[index.parse::<usize>().unwrap() - 1])
    } else {
        json!(token.parse::<i64>().unwrap())
    }
}
impl ModelDatabase for PolicyDatabase {
    fn fetch(&self, _: ModelQuery) -> ModelFuture<'_, Vec<ModelRow>> {
        Box::pin(async { panic!("unexpected fetch") })
    }
    fn execute(&self, _: ModelQuery) -> ModelFuture<'_, String> {
        Box::pin(async { panic!("unexpected execute") })
    }
    fn fetchrow(&self, query: ModelQuery) -> ModelFuture<'_, Option<ModelRow>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let sql = query.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            state.queries.push(sql.clone());
            if sql.starts_with("INSERT INTO llm_models") {
                let (_, rest) = sql.split_once('(').unwrap();
                let (columns, rest) = rest.split_once(") VALUES (").unwrap();
                let (values, _) = rest.split_once(')').unwrap();
                let row: ModelRow = columns
                    .split(',')
                    .zip(values.split(','))
                    .map(|(column, token)| (column.trim().into(), bound(token, &query.params)))
                    .collect();
                state.rows.push(row.clone());
                return Ok(Some(row));
            }
            let (_, rest) = sql.split_once(" WHERE ").unwrap();
            let predicates = rest.split(" RETURNING ").next().unwrap();
            let predicates: Vec<_> = predicates.split(" AND ").collect();
            assert_eq!(predicates.len(), 3);
            let position = state.rows.iter().position(|row| {
                predicates.iter().all(|predicate| {
                    let (key, token) = predicate.split_once(" = ").unwrap();
                    row.get(key) == Some(&bound(token, &query.params))
                })
            });
            let Some(position) = position else {
                return Ok(None);
            };
            if sql.starts_with("UPDATE ") {
                if state.admin_edit_before_update {
                    let row = &mut state.rows[position];
                    row.insert("access_level".into(), json!("premium"));
                    row.insert("input_price_per_1k".into(), json!("0.5"));
                    row.insert("output_price_per_1k".into(), json!("1.25"));
                    state.admin_edit_before_update = false;
                }
                let row = &mut state.rows[position];
                let assignments = sql
                    .split(" SET ")
                    .nth(1)
                    .unwrap()
                    .split(" WHERE ")
                    .next()
                    .unwrap();
                for assignment in assignments.split(", ") {
                    let (key, token) = assignment.split_once(" = ").unwrap();
                    let next = if token == "capability_revision + 1" {
                        json!(row[key].as_i64().unwrap() + 1)
                    } else {
                        bound(token, &query.params)
                    };
                    row.insert(key.into(), next);
                }
            } else {
                assert!(sql.starts_with("SELECT "));
            }
            Ok(Some(state.rows[position].clone()))
        })
    }
}
struct Pricing;
impl ModelPricingCache for Pricing {
    fn invalidate_cache(&self) -> Result<(), ModelServiceError> {
        Ok(())
    }
}
fn policy_row() -> ModelRow {
    json!({
        "tenant_id":"tenant-a", "provider_id":"test-provider", "model_id":"custom-model",
        "display_name":"Old catalog label", "context_window":1000, "max_output_tokens":100,
        "supports_tools":true, "supports_vision":false,
        "catalog_capabilities":{}, "capability_overrides":{"reasoning":{"default_option":"off"}},
        "capability_revision":3, "input_price_per_1k":"0.125", "output_price_per_1k":"0.375",
        "access_level":"admin", "is_enabled":true, "sort_order":1
    })
    .as_object()
    .unwrap()
    .clone()
}
fn catalog_fields() -> ModelFields {
    ModelFields {
        display_name: "New catalog label".into(),
        context_window: 32000,
        max_output_tokens: 8192,
        supports_vision: true,
        sort_order: 9,
        input_price_per_1k: Decimal::new(1, 3),
        output_price_per_1k: Decimal::new(2, 3),
        access_level: "public".into(),
        catalog_capabilities: Some(
            json!({"reasoning":{"default_option":"off"}})
                .as_object()
                .unwrap()
                .clone(),
        ),
        ..Default::default()
    }
}

#[tokio::test]
async fn catalog_preserves_existing_policy_and_refreshes_metadata() {
    for (access, enabled) in [("admin", true), ("premium", false), ("public", true)] {
        let mut existing = policy_row();
        existing.insert("access_level".into(), json!(access));
        existing.insert("is_enabled".into(), json!(enabled));
        let mut sibling = policy_row();
        sibling.insert("provider_id".into(), json!("other-provider"));
        let mut other_tenant = policy_row();
        other_tenant.insert("tenant_id".into(), json!("tenant-b"));
        let db = PolicyDatabase::new(vec![
            existing.clone(),
            sibling.clone(),
            other_tenant.clone(),
        ]);
        let service = ModelService::new(db.clone(), Pricing);
        let initial_snapshot =
            service.row_to_dict(Some(existing.clone())).unwrap()["pricing_snapshot"].clone();
        for expected_change in [true, false] {
            let (status, row, changed) = service
                .upsert_model_from_catalog(
                    "tenant-a",
                    "test-provider",
                    "custom-model",
                    catalog_fields(),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(status, "updated");
            assert_eq!(row["access_level"], access);
            assert_eq!(row["input_price_per_1k"], 0.125);
            assert_eq!(row["output_price_per_1k"], 0.375);
            assert_eq!(row["is_enabled"], enabled);
            assert_eq!(
                row["capability_overrides"],
                existing["capability_overrides"]
            );
            assert_eq!(row["display_name"], "New catalog label");
            assert_eq!(row["context_window"], 32000);
            assert_eq!(row["max_output_tokens"], 8192);
            assert_eq!(row["supports_vision"], true);
            assert_eq!(row["sort_order"], 9);
            assert_eq!(row["pricing_snapshot"], initial_snapshot);
            assert_eq!(
                row["catalog_capabilities"],
                Value::Object(catalog_fields().catalog_capabilities.unwrap())
            );
            assert_eq!(changed, expected_change);
        }
        let state = db.0.lock().unwrap();
        assert_eq!(&state.rows[1..], &[sibling, other_tenant]);
        for sql in state
            .queries
            .iter()
            .filter(|sql| sql.starts_with("UPDATE "))
        {
            let assignments = sql
                .split(" SET ")
                .nth(1)
                .unwrap()
                .split(" WHERE ")
                .next()
                .unwrap();
            for protected in [
                "access_level",
                "input_price_per_1k",
                "output_price_per_1k",
                "is_enabled",
                "capability_overrides",
            ] {
                assert!(!assignments.contains(&format!("{protected} =")));
            }
        }
    }
}

#[tokio::test]
async fn catalog_initializes_new_model_policy() {
    for enable_new in [true, false] {
        let db = PolicyDatabase::default();
        let (status, row, changed) = ModelService::new(db.clone(), Pricing)
            .upsert_model_from_catalog(
                "tenant-a",
                "test-provider",
                "custom-model",
                catalog_fields(),
                enable_new,
            )
            .await
            .unwrap();
        assert_eq!((status, changed), ("created", true));
        assert_eq!(row["access_level"], "public");
        assert_eq!(row["input_price_per_1k"], 0.001);
        assert_eq!(row["output_price_per_1k"], 0.002);
        assert_eq!(row["is_enabled"], enable_new);
        assert_eq!(row["capability_overrides"], json!({}));
        assert_eq!(db.0.lock().unwrap().rows.len(), 1);
    }
}

#[tokio::test]
async fn explicit_administrator_policy_update_still_applies() {
    let db = PolicyDatabase::new(vec![policy_row()]);
    let row = ModelService::new(db, Pricing)
        .update_model(
            "tenant-a",
            "custom-model",
            Some("test-provider"),
            UpdateModel {
                access_level: Some("premium".into()),
                input_price_per_1k: Some(Decimal::new(5, 1)),
                output_price_per_1k: Some(Decimal::new(125, 2)),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row["access_level"], "premium");
    assert_eq!(row["input_price_per_1k"], 0.5);
    assert_eq!(row["output_price_per_1k"], 1.25);
}

#[tokio::test]
async fn catalog_does_not_write_back_policy_read_before_an_admin_edit() {
    let db = PolicyDatabase::new(vec![policy_row()]);
    db.0.lock().unwrap().admin_edit_before_update = true;
    let (_, row, _) = ModelService::new(db, Pricing)
        .upsert_model_from_catalog(
            "tenant-a",
            "test-provider",
            "custom-model",
            catalog_fields(),
            true,
        )
        .await
        .unwrap();
    assert_eq!(row["access_level"], "premium");
    assert_eq!(row["input_price_per_1k"], 0.5);
    assert_eq!(row["output_price_per_1k"], 1.25);
}

#[tokio::test]
async fn catalog_preserves_persisted_null_and_zero_prices() {
    for price in [Value::Null, json!("0")] {
        let mut existing = policy_row();
        existing.insert("input_price_per_1k".into(), price.clone());
        existing.insert("output_price_per_1k".into(), price.clone());
        let db = PolicyDatabase::new(vec![existing]);
        ModelService::new(db.clone(), Pricing)
            .upsert_model_from_catalog(
                "tenant-a",
                "test-provider",
                "custom-model",
                catalog_fields(),
                true,
            )
            .await
            .unwrap();
        let state = db.0.lock().unwrap();
        assert_eq!(state.rows[0]["input_price_per_1k"], price);
        assert_eq!(state.rows[0]["output_price_per_1k"], price);
    }
}
