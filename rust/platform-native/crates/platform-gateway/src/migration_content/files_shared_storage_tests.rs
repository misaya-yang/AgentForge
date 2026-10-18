//! Two independent production S3 adapters against one provider-protocol fixture.
//! This exercises the real PUT/list/HEAD/GET paths, not a live S3 acceptance gate.
use super::*;
use ai_platform_service_primitives::content::storage::{CloudStorage, S3HttpTransport};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use std::sync::Mutex;
#[derive(Clone)]
struct Object {
    bytes: Vec<u8>,
    headers: HeaderMap,
}
type Objects = Arc<Mutex<BTreeMap<String, Object>>>;
async fn provider(
    State(objects): State<Objects>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let key = request
        .uri()
        .path()
        .strip_prefix("/bucket/")
        .unwrap_or("")
        .to_owned();
    if query.contains_key("list-type") {
        let prefix = query.get("prefix").unwrap();
        let after = query.get("continuation-token");
        let objects = objects.lock().unwrap();
        let keys: Vec<_> = objects
            .keys()
            .filter(|key| key.starts_with(prefix) && after.is_none_or(|after| *key > after))
            .collect();
        // Force pagination even for tiny fixtures, preserving the opaque continuation cursor.
        let next = if keys.len() > 1 {
            format!("<NextContinuationToken>{}</NextContinuationToken>", keys[0])
        } else {
            String::new()
        };
        let contents = keys
            .first()
            .map(|key| format!("<Contents><Key>{key}</Key></Contents>"))
            .unwrap_or_default();
        return (
            StatusCode::OK,
            format!("<ListBucketResult>{contents}{next}</ListBucketResult>"),
        )
            .into_response();
    }
    if request.method() == Method::PUT {
        let headers = request.headers().clone();
        let body = to_bytes(request.into_body(), 64 * 1024).await.unwrap();
        let mut objects = objects.lock().unwrap();
        if headers.get("if-none-match").is_some() && objects.contains_key(&key) {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        objects.insert(
            key,
            Object {
                bytes: body.to_vec(),
                headers,
            },
        );
        return StatusCode::OK.into_response();
    }
    if request.method() == Method::DELETE {
        objects.lock().unwrap().remove(&key);
        return StatusCode::NO_CONTENT.into_response();
    }
    let objects = objects.lock().unwrap();
    let Some(object) = objects.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-length", object.bytes.len().to_string());
    for (key, value) in &object.headers {
        if key == "content-type" || key.as_str().starts_with("x-amz-meta-") {
            response = response.header(key, value);
        }
    }
    response
        .body(if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(object.bytes.clone())
        })
        .unwrap()
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("shared-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn replica(root: &FsPath, endpoint: &str) -> FilesApi {
    let mut api =
        (*super::manual_file_scope_regressions::setup(root, "user", "tenant-a", false)).clone();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let transport = S3HttpTransport::new(
        client,
        "local-test".into(),
        "test-key".into(),
        b"test-secret".to_vec(),
        Some(endpoint.into()),
    )
    .unwrap();
    api.storage.backend = Arc::new(CloudStorage {
        transport: Arc::new(transport),
        bucket: "bucket".into(),
        prefix: "dev".into(),
        public_base: String::new(),
        s3: true,
    });
    api
}
#[tokio::test]
async fn independent_gateway_replicas_read_same_shared_files_with_tenant_user_isolation() {
    let objects: Objects = Arc::new(Mutex::new(BTreeMap::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let service = Router::new()
        .fallback(any(provider))
        .with_state(objects.clone());
    let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(task);
    let first = Scratch::new();
    let second = Scratch::new();
    let a = replica(&first.0, &endpoint);
    let b = replica(&second.0, &endpoint);
    let actor = ContentIdentity {
        user_id: "actor".into(),
        tenant_id: "tenant-a".into(),
        is_authenticated: true,
        ..Default::default()
    };
    let upload = a
        .storage
        .upload(&actor, "你好.txt", b"first".to_vec(), 1024)
        .await
        .unwrap();
    let second_upload = a
        .storage
        .upload(&actor, "second.txt", b"second".to_vec(), 1024)
        .await
        .unwrap();
    let foreign = ContentIdentity {
        tenant_id: "tenant-ab".into(),
        ..actor.clone()
    };
    let foreign_upload = a
        .storage
        .upload(&foreign, "foreign.txt", b"foreign".to_vec(), 1024)
        .await
        .unwrap();
    let listed = b.list(&actor).await.unwrap();
    assert_eq!(listed["total"], 2);
    let id = upload["file_id"].as_str().unwrap();
    let found = b.get(&actor, id).await.unwrap();
    assert_eq!(found["file_id"], id);
    assert_eq!(found["size_bytes"], 5);
    let key = upload["file_path"]
        .as_str()
        .unwrap()
        .trim_start_matches('/');
    assert_eq!(b.storage.backend.download(key).await.unwrap(), b"first");
    assert_eq!(
        b.storage.backend.head(key).await.unwrap().unwrap().metadata["original_filename"],
        "你好.txt"
    );
    assert_eq!(
        b.get(&actor, foreign_upload["file_id"].as_str().unwrap())
            .await
            .unwrap_err()
            .status,
        404
    );
    let another_user = ContentIdentity {
        user_id: "another-user".into(),
        ..actor.clone()
    };
    assert_eq!(b.list(&another_user).await.unwrap()["total"], 0);
    assert_eq!(
        b.list(&ContentIdentity {
            tenant_id: "".into(),
            ..actor.clone()
        })
        .await
        .unwrap()["total"],
        0
    );
    assert_eq!(
        b.list(&ContentIdentity {
            user_id: "../actor".into(),
            ..actor.clone()
        })
        .await
        .unwrap_err()
        .status,
        400
    );
    assert!(!first.0.join("uploads").exists());
    assert!(!second.0.join("uploads").exists());
    a.storage
        .delete(&actor, second_upload["file_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(b.list(&actor).await.unwrap()["total"], 1);
}
