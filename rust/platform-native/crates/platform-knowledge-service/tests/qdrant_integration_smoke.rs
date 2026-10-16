//! Explicit live-only parity for the two historical Qdrant integration smoke cases.
//! Run with --ignored and an explicit QDRANT_URL for a disposable test service.
//! Each case owns a random disposable collection; no SQL rows or default endpoint.
use ai_platform_knowledge_service::migration_knowledge_query::{
    config::RetrieveRequest,
    qdrant::QdrantStore,
    types::{Dataset, QueryError},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};
type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}
async fn fixture(
    prefix: &str,
) -> Result<
    (QdrantStore, Dataset, reqwest::Client, reqwest::Url),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let endpoint = std::env::var("QDRANT_URL")
        .map_err(|_| "QDRANT_URL must explicitly select a disposable test service")?;
    require(!endpoint.trim().is_empty(), "QDRANT_URL must not be empty")?;
    let origin = reqwest::Url::parse(&endpoint)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut store = QdrantStore::new(client.clone(), origin.clone(), None)?;
    store.timeout = Duration::from_secs(10);
    let mut dataset = Dataset {
        dataset_id: format!("{prefix}-{}", uuid::Uuid::new_v4().simple()),
        tenant_id: "kb-eval-smoke-tenant".into(),
        content_revision: Some(1),
        ..Default::default()
    };
    dataset.collection_name = Some(store.ensure_collection(&dataset, 8).await?);
    Ok((store, dataset, client, origin))
}
async fn hits(
    store: &QdrantStore,
    dataset: &Dataset,
) -> Result<
    Vec<ai_platform_knowledge_service::migration_knowledge_query::qdrant::VectorHit>,
    QueryError,
> {
    store
        .hybrid_search_native(
            dataset,
            "quarterly invoice download billing history",
            &[1., 0., 0., 0., 0., 0., 0., 0.],
            3,
            3,
            3,
            60,
            None,
            &RetrieveRequest::default(),
        )
        .await
}
#[tokio::test]
#[ignore = "requires an explicitly supplied live Qdrant; creates and deletes a disposable collection"]
async fn qdrant_integration_smoke() {
    let (store, dataset, _, _) = fixture("kb-eval-smoke").await.expect("live Qdrant fixture");
    let result:TestResult=async {
        let info=store.collection_info(dataset.collection()?).await?;
        require(info["config"]["metadata"]["knowledge_scope"]["dataset_id"]==dataset.dataset_id,"collection dataset scope")?;
        require(info["config"]["metadata"]["knowledge_scope"]["tenant_id"]==dataset.tenant_id,"collection tenant scope")?;
        let ids=(0..3).map(|_|uuid::Uuid::new_v4().to_string()).collect::<Vec<_>>();
        let texts=["quarterly invoice download billing history","password reset link verified email channel","garden gnomes ceramic patio decoration"];
        let docs=["doc-a","doc-b","doc-c"];
        let points=(0..3).map(|i| {let mut vector=vec![0.;8];vector[i]=1.;json!({"id":ids[i],"vector":vector,"payload":{"document_id":docs[i],"segment_id":ids[i],"text":texts[i],"level":0,"content_type":"text","content_revision":1}})}).collect::<Vec<_>>();
        store.upsert_points(&dataset,&points).await?;
        let found=hits(&store,&dataset).await?;
        require(found.first().is_some_and(|h|h.payload["segment_id"]==ids[0]),"hybrid RRF first result")?;
        require(found.iter().map(|h|h.point_id.clone()).collect::<BTreeSet<_>>()==ids.iter().cloned().collect(),"hybrid hit membership")?;
        require(found.iter().all(|h|h.score>0.),"positive fusion scores")?;
        let mut foreign=dataset.clone();foreign.tenant_id="some-other-tenant".into();
        require(matches!(hits(&store,&foreign).await,Err(QueryError::CollectionAuthority(_))),"foreign tenant must be refused")?;
        store.delete_segment_points(&dataset,"doc-a",&ids[0]).await?;
        let remaining=hits(&store,&dataset).await?;
        require(remaining.iter().map(|h|h.point_id.clone()).collect::<BTreeSet<_>>()==ids[1..].iter().cloned().collect(),"scoped segment deletion")?;
        Ok(())
    }.await;
    let cleanup = store.delete_dataset_collections(&dataset).await;
    result.expect("live hybrid smoke assertions");
    cleanup.expect("disposable collection cleanup");
}
#[tokio::test]
#[ignore = "requires an explicitly supplied live Qdrant; creates and deletes a disposable collection"]
async fn special_cleanup_deletes_legacy_tenantless_point_and_skips_missing_collection() {
    let (store, dataset, client, origin) = fixture("kb-special-cleanup")
        .await
        .expect("live Qdrant fixture");
    let result:TestResult=async {
        let collection=dataset.collection()?;
        let ids=[uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string()];
        let mut url=origin.clone();url.path_segments_mut().unwrap().pop_if_empty().extend(["collections",collection,"points"]);url.set_query(Some("wait=true"));
        client.put(url).json(&json!({"points":[
            {"id":ids[0],"vector":[1.,0.,0.,0.,0.,0.,0.,0.],"payload":{"dataset_id":dataset.dataset_id,"document_id":"doc-old"}},
            {"id":ids[1],"vector":[0.,1.,0.,0.,0.,0.,0.,0.],"payload":{"dataset_id":dataset.dataset_id,"document_id":"doc-other"}}
        ]})).send().await?.error_for_status()?;
        let frozen=store.document_point_ids_by_collection(&dataset,"doc-old").await?;
        require(frozen.get(collection)==Some(&vec![ids[0].clone()]),"legacy tenantless source manifest")?;
        store.delete_document_points_by_ids(collection,&ids,&dataset,"doc-old",true).await?;
        let remaining=store.snapshot_points(&dataset,&ids).await?;
        require(remaining.iter().map(|p|p["id"].clone()).collect::<Vec<Value>>()==vec![json!(ids[1])],"cleanup must not delete a different document")?;
        store.delete_document_points_by_ids(&format!("{collection}_not_created"),&ids[..1],&dataset,"doc-old",true).await?;
        Ok(())
    }.await;
    let cleanup = store.delete_dataset_collections(&dataset).await;
    result.expect("live special cleanup assertions");
    cleanup.expect("disposable collection cleanup");
}
