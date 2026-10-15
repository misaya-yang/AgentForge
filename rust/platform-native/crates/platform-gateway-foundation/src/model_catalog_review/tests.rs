use super::*;
use crate::{
    model_catalog_sync::{CatalogApiKey, CatalogHttpFuture, CatalogRequest, CatalogResponse},
    provider_service::ServiceFuture,
};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Providers;
impl CatalogProviders for Providers {
    fn get_provider<'a>(&'a self, _: &'a str, id: &'a str) -> ServiceFuture<'a, Option<Value>> {
        Box::pin(async move {
            Ok(Some(
                json!({"provider_id":id,"api_type":"dashscope","base_url":"https://dashscope.aliyuncs.com/compatible-mode/v1"}),
            ))
        })
    }
    fn get_runtime_api_key<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> ServiceFuture<'a, Option<CatalogApiKey>> {
        Box::pin(async { Ok(None) })
    }
}
struct Http;
impl CatalogHttp for Http {
    fn get(&self, _: CatalogRequest) -> CatalogHttpFuture<'_> {
        Box::pin(async {
            Ok(CatalogResponse {
                status_code: 200,
                json: Ok(json!({"data":[]})),
            })
        })
    }
}
pub(crate) struct Database {
    value: Mutex<Snapshot>,
    calls: AtomicUsize,
    mode: AtomicUsize,
}
impl ReviewDatabase for Database {
    fn snapshot<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str) -> ReviewFuture<'a, Snapshot> {
        Box::pin(async { Ok(self.value.lock().unwrap().clone()) })
    }
    fn apply<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        before: &'a Snapshot,
        proposals: Vec<CatalogModelKwargs>,
    ) -> ReviewFuture<'a, Vec<Value>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.mode.load(Ordering::SeqCst) == 2 {
                std::future::pending::<()>().await;
            }
            let mut v = self.value.lock().unwrap();
            if &*v != before {
                return Err(ReviewError::Conflict);
            }
            if self.mode.load(Ordering::SeqCst) == 1 {
                return Err(ReviewError::Unknown);
            }
            let result: Vec<_> = proposals.iter().map(metadata).collect();
            v.models = result.clone();
            Ok(result)
        })
    }
}
pub(crate) fn setup() -> (CatalogReview, Arc<Database>) {
    let db = Arc::new(Database {
        value: Mutex::new(Snapshot {
            provider_revision: json!({"updated_at":"1"}),
            models: vec![],
            references: vec![],
            reference_coverage: "fixture".into(),
        }),
        calls: AtomicUsize::new(0),
        mode: AtomicUsize::new(0),
    });
    (
        CatalogReview::new(Arc::new(Providers), Arc::new(Http), db.clone()),
        db,
    )
}
fn selection(p: &Value) -> ApplySelection {
    ApplySelection {
        review_id: p["review_id"].as_str().unwrap().into(),
        model_ids: vec![p["changes"][0]["model_id"].as_str().unwrap().into()],
        replace_existing_metadata: false,
    }
}
#[tokio::test]
async fn preview_is_inert_and_apply_is_single_use() {
    let (s, db) = setup();
    let p = s.preview("t", "a", "dashscope").await.unwrap();
    assert!(!p["changes"].as_array().unwrap().is_empty());
    assert_eq!(db.calls.load(Ordering::SeqCst), 0);
    let q = selection(&p);
    let applied = s.apply("t", "a", "dashscope", q.clone()).await.unwrap();
    assert_eq!(applied["state"], "applied");
    assert_eq!(
        s.apply("t", "a", "dashscope", q).await.unwrap_err(),
        ReviewError::Unknown
    );
    assert_eq!(db.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.read("t", "a", "dashscope", p["review_id"].as_str().unwrap())
            .await
            .unwrap()["state"],
        "applied"
    );
}
#[tokio::test]
async fn actor_tenant_provider_scope_blocks_read_and_write() {
    let (s, db) = setup();
    let p = s.preview("t", "a", "dashscope").await.unwrap();
    for (t, a, provider) in [
        ("other", "a", "dashscope"),
        ("t", "other", "dashscope"),
        ("t", "a", "other"),
    ] {
        assert_eq!(
            s.apply(t, a, provider, selection(&p)).await.unwrap_err(),
            ReviewError::NotFound
        );
        assert_eq!(
            s.read(t, a, provider, p["review_id"].as_str().unwrap())
                .await
                .unwrap_err(),
            ReviewError::NotFound
        );
    }
    assert_eq!(db.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn full_snapshot_conflicts_on_provider_model_or_reference_change() {
    for field in 0..3 {
        let (s, db) = setup();
        let p = s.preview("t", "a", "dashscope").await.unwrap();
        {
            let mut v = db.value.lock().unwrap();
            match field {
                0 => v.provider_revision = json!("changed"),
                1 => v
                    .models
                    .push(json!({"model_id":"manual","display_name":"edited"})),
                _ => v.references.push(json!({"revision":2})),
            }
        }
        assert_eq!(
            s.apply("t", "a", "dashscope", selection(&p))
                .await
                .unwrap_err(),
            ReviewError::Conflict
        );
        assert_eq!(
            s.read("t", "a", "dashscope", p["review_id"].as_str().unwrap())
                .await
                .unwrap()["state"],
            "stale"
        );
    }
}
#[tokio::test]
async fn invalid_selection_does_not_consume_receipt() {
    let (s, db) = setup();
    let p = s.preview("t", "a", "dashscope").await.unwrap();
    for ids in [
        vec![],
        vec!["arbitrary".into()],
        vec![selection(&p).model_ids[0].clone(); 2],
    ] {
        let mut q = selection(&p);
        q.model_ids = ids;
        assert_eq!(
            s.apply("t", "a", "dashscope", q).await.unwrap_err(),
            ReviewError::Invalid
        );
    }
    assert_eq!(db.calls.load(Ordering::SeqCst), 0);
    s.apply("t", "a", "dashscope", selection(&p)).await.unwrap();
}
#[tokio::test]
async fn unknown_commit_never_replays() {
    let (s, db) = setup();
    let p = s.preview("t", "a", "dashscope").await.unwrap();
    db.mode.store(1, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(
            s.apply("t", "a", "dashscope", selection(&p))
                .await
                .unwrap_err(),
            ReviewError::Unknown
        );
    }
    assert_eq!(db.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.read("t", "a", "dashscope", p["review_id"].as_str().unwrap())
            .await
            .unwrap()["state"],
        "outcome_unknown"
    );
}
#[tokio::test]
async fn existing_metadata_requires_explicit_acknowledgement() {
    let (s, db) = setup();
    let initial = s.preview("t", "a", "dashscope").await.unwrap();
    let id = initial["changes"][0]["model_id"].clone();
    db.value.lock().unwrap().models.push(json!({"model_id":id,"display_name":"Manual title","input_price_per_1k":"9.50","capability_overrides":{"tools":false}}));
    let p = s.preview("t", "a", "dashscope").await.unwrap();
    let q = selection(&p);
    assert_eq!(
        s.apply("t", "a", "dashscope", q.clone()).await.unwrap_err(),
        ReviewError::Invalid
    );
    let mut q = q;
    q.replace_existing_metadata = true;
    s.apply("t", "a", "dashscope", q).await.unwrap();
    assert!(
        p["changes"][0]["preserved_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("capability_overrides"))
    );
}
#[tokio::test]
async fn expired_and_capacity_receipts_fail_closed() {
    let (s, _) = setup();
    let p = s.preview("t", "a", "dashscope").await.unwrap();
    let id = p["review_id"].as_str().unwrap();
    s.receipts.lock().unwrap().get_mut(id).unwrap().created =
        Instant::now() - Duration::from_secs(901);
    assert_eq!(
        s.read("t", "a", "dashscope", id).await.unwrap_err(),
        ReviewError::NotFound
    );
}

#[tokio::test]
async fn cancellation_consumes_attempt_and_never_replays() {
    let (service, database) = setup();
    let service = Arc::new(service);
    let preview = service.preview("t", "a", "dashscope").await.unwrap();
    database.mode.store(2, Ordering::SeqCst);
    let cloned = service.clone();
    let request = selection(&preview);
    let task = tokio::spawn(async move { cloned.apply("t", "a", "dashscope", request).await });
    while database.calls.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        service
            .apply("t", "a", "dashscope", selection(&preview))
            .await
            .unwrap_err(),
        ReviewError::Unknown
    );
    assert_eq!(database.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn production_envelopes_match_source_owned_bounded_schemas() {
    let (service, _) = setup();
    let preview = service.preview("t", "a", "dashscope").await.unwrap();
    assert!(jsonschema::is_valid(
        &crate::native_contract::responses::catalog_review_schema(true, false),
        &preview
    ));
    let applied = service
        .apply("t", "a", "dashscope", selection(&preview))
        .await
        .unwrap();
    assert!(jsonschema::is_valid(
        &crate::native_contract::responses::catalog_review_schema(false, false),
        &applied
    ));
    let read = service
        .read(
            "t",
            "a",
            "dashscope",
            preview["review_id"].as_str().unwrap(),
        )
        .await
        .unwrap();
    assert!(jsonschema::is_valid(
        &crate::native_contract::responses::catalog_review_schema(false, true),
        &read
    ));
    assert!(read.get("preview").is_none());
}
