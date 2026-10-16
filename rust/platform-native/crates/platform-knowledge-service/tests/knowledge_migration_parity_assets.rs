use ai_platform_knowledge_service::{
    migration_content::{
        ContentFuture,
        storage::{ObjectInfo, ObjectStorage},
    },
    migration_knowledge_ingest::assets::AssetStorage,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Recording {
    downloads: Mutex<Vec<String>>,
    deleted: Mutex<Vec<String>>,
}
impl ObjectStorage for Recording {
    fn upload<'a>(
        &'a self,
        _: &'a str,
        _: Vec<u8>,
        _: &'a str,
        _: BTreeMap<String, String>,
    ) -> ContentFuture<'a, String> {
        Box::pin(async { panic!("unexpected upload") })
    }
    fn download<'a>(&'a self, key: &'a str) -> ContentFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.downloads.lock().unwrap().push(key.into());
            Ok(b"owned-image".to_vec())
        })
    }
    fn delete<'a>(&'a self, _: &'a str) -> ContentFuture<'a, bool> {
        Box::pin(async { panic!("unexpected object deletion") })
    }
    fn delete_prefix<'a>(&'a self, prefix: &'a str) -> ContentFuture<'a, u64> {
        Box::pin(async move {
            self.deleted.lock().unwrap().push(prefix.into());
            Ok(2)
        })
    }
    fn head<'a>(&'a self, _: &'a str) -> ContentFuture<'a, Option<ObjectInfo>> {
        Box::pin(async { panic!("unexpected head") })
    }
    fn download_url<'a>(
        &'a self,
        _: &'a str,
        _: u32,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, Option<String>> {
        Box::pin(async { panic!("unexpected URL generation") })
    }
    fn upload_url<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: u32,
        _: BTreeMap<String, String>,
    ) -> ContentFuture<'a, Option<BTreeMap<String, String>>> {
        Box::pin(async { panic!("unexpected upload URL") })
    }
}
#[tokio::test]
async fn cleanup_uses_only_exact_owned_prefixes_and_counts() {
    let backend = Arc::new(Recording::default());
    let storage = AssetStorage::from_backend(backend.clone());
    assert_eq!(
        storage
            .delete_document_assets(" tenant-a ", " document-a ")
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        *backend.deleted.lock().unwrap(),
        vec![
            "knowledge/confluence/tenant-a/document-a/images/",
            "knowledge/documents/tenant-a/document-a/original/"
        ]
    );
}
#[tokio::test]
async fn ambiguous_cleanup_scope_never_calls_backend() {
    let backend = Arc::new(Recording::default());
    let storage = AssetStorage::from_backend(backend.clone());
    for (tenant, document) in [
        ("", "document-a"),
        ("tenant-a", "   "),
        ("tenant/a", "document-a"),
        ("tenant-a", "../document-a"),
        ("tenant-a", "document\\a"),
    ] {
        assert!(
            storage
                .delete_document_assets(tenant, document)
                .await
                .is_err()
        );
    }
    assert!(backend.deleted.lock().unwrap().is_empty());
}
#[tokio::test]
async fn scoped_image_download_calls_backend_for_exact_owned_key_only() {
    let backend = Arc::new(Recording::default());
    let storage = AssetStorage::from_backend(backend.clone());
    let key = "knowledge/confluence/tenant-a/document-a/images/attachment_image.png";
    assert_eq!(
        storage
            .download_document_image("tenant-a", "document-a", key)
            .await
            .unwrap(),
        b"owned-image"
    );
    assert_eq!(*backend.downloads.lock().unwrap(), vec![key]);
    backend.downloads.lock().unwrap().clear();
    for key in [
        "http://127.0.0.1:8092/internal/metadata",
        "knowledge/confluence/tenant-b/document-a/images/image.png",
        "knowledge/confluence/tenant-a/document-b/images/image.png",
        "knowledge/confluence/tenant-a/document-a/images/",
        "knowledge/confluence/tenant-a/document-a/images/../secret",
        "knowledge/confluence/tenant-a/document-a/images/folder\\secret",
        " knowledge/confluence/tenant-a/document-a/images/image.png ",
    ] {
        assert!(
            storage
                .download_document_image("tenant-a", "document-a", key)
                .await
                .is_err()
        );
    }
    assert!(backend.downloads.lock().unwrap().is_empty());
}
#[test]
fn local_image_signing_fails_closed_without_key() {
    let storage = AssetStorage::from_backend(Arc::new(Recording::default()));
    assert!(
        storage
            .sign_url_if_local("file:///tmp/owned-image.png")
            .unwrap_err()
            .to_string()
            .contains("signing key")
    );
}

#[test]
fn local_image_signing_matches_exported_knowledge_verifier() {
    use ai_platform_knowledge_service::migration_knowledge_ingest::assets::verify_signed_url;
    let storage = AssetStorage::from_backend(Arc::new(Recording::default()))
        .with_signing_key(b"test-signing-key".to_vec());
    let signed = storage
        .sign_url_if_local("file:///tmp/owned-image.png")
        .unwrap();
    assert!(signed.contains("expires=") && signed.contains("sig="));
    assert_eq!(
        verify_signed_url(&signed, "test-signing-key"),
        (true, String::new())
    );
    assert_eq!(
        verify_signed_url(&signed, "wrong-key"),
        (false, "Invalid signature".into())
    );
    assert_eq!(
        verify_signed_url(
            &signed.replace("owned-image", "other-image"),
            "test-signing-key"
        ),
        (false, "Invalid signature".into())
    );
    assert_eq!(
        verify_signed_url("", "test-signing-key"),
        (false, "Empty URL".into())
    );
    assert_eq!(
        verify_signed_url("file:///tmp/unsigned", ""),
        (true, String::new())
    );
    assert_eq!(
        verify_signed_url("file:///tmp/unsigned", "test-signing-key"),
        (false, "Missing signature or expiry".into())
    );
    assert_eq!(
        verify_signed_url(
            "file:///tmp/unsigned?expires=never&sig=abc",
            "test-signing-key"
        ),
        (false, "Invalid expiry format".into())
    );
    assert_eq!(
        verify_signed_url("file:///tmp/unsigned?expires=1&sig=abc", "test-signing-key"),
        (false, "URL has expired".into())
    );
    // The first nonempty query value wins, as in archived parse_qs.
    let with_duplicate = format!("{signed}&sig=wrong&expires=1&other=ignored#fragment");
    assert_eq!(
        verify_signed_url(&with_duplicate, "test-signing-key"),
        (true, String::new())
    );
}
