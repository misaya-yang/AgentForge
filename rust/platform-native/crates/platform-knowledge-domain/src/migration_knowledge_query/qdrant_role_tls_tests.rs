//! Qdrant protocol/security regressions.
use super::*;
#[tokio::test]
async fn read_role_rejects_all_mutation_methods_before_network_io() {
    let store = QdrantStore::for_role(
        Url::parse("http://127.0.0.1:1").unwrap(),
        None,
        QdrantRole::ApiReadOnly,
        None,
    )
    .unwrap();
    // `wait` used to be named `write`: metadata/collection mutations often
    // pass false. Role enforcement must inspect the actual operation.
    for (method, path) in [
        (Method::DELETE, vec![]),
        (Method::PUT, vec![]),
        (Method::PATCH, vec![]),
        (Method::POST, vec!["points", "payload"]),
        (Method::POST, vec!["points", "delete"]),
        (Method::PUT, vec!["points"]),
    ] {
        let error = store
            .call(method, "collection", &path, None, false)
            .await
            .unwrap_err();
        assert!(matches!(error, QueryError::Validation(_)));
    }
}
#[test]
fn ca_configuration_never_silently_ignores_an_invalid_path_or_plain_http() {
    for (origin, path) in [
        ("https://example.invalid", "relative.pem"),
        ("http://example.invalid", "/nonexistent/controlled-ca.pem"),
        ("https://example.invalid", "/nonexistent/controlled-ca.pem"),
    ] {
        assert!(
            QdrantStore::for_role(
                Url::parse(origin).unwrap(),
                None,
                QdrantRole::ApiReadOnly,
                Some(std::path::Path::new(path))
            )
            .is_err()
        );
    }
    let mut store = QdrantStore::new(
        Client::new(),
        Url::parse("https://example.invalid").unwrap(),
        None,
    )
    .unwrap();
    store.custom_ca = true;
    assert!(store.set_prefer_grpc(true).is_err());
    assert!(store.set_prefer_grpc(false).is_ok());
}
