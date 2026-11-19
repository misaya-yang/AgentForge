use ai_platform_gateway_foundation::{
    generic_rest_adapter::{ReqwestRestTransport, RestRequest, RestTransport},
    service_registry::service_from_value,
};
use axum::{Router, extract::State, routing::head};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[tokio::test]
async fn production_http_pool_warms_once_and_shutdown_stops_new_calls() {
    let count = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/prefix/",
            head(|State(count): State<Arc<AtomicUsize>>| async move {
                count.fetch_add(1, Ordering::SeqCst);
                ""
            }),
        )
        .with_state(count.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/prefix", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let service = service_from_value(
        &json!({"service_id":"http-lifecycle", "connector_config":{"base_url":endpoint}}),
    )
    .unwrap();
    let transport = ReqwestRestTransport::new(&service).unwrap();
    transport.warm_up_connections(2).await;
    transport.warm_up_connections(10).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    transport.shutdown().await.unwrap();
    transport.shutdown().await.unwrap();
    assert!(!transport.health_check().await.unwrap());
    assert!(
        transport
            .request(RestRequest {
                method: "GET".into(),
                endpoint: "/".into(),
                body: json!({})
            })
            .await
            .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    server.abort();
}
