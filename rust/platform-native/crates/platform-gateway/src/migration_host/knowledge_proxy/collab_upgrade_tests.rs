use super::super::{router, ContentError, ContentIdentity, KnowledgeProxy, Result};
use super::{IntoClientRequest, Message, WebSocketUpgrade};
use crate::migration_knowledge_query::auth::{self, GatewayAuth};
use crate::migration_knowledge_query::types::UserContext;
use axum::{
    extract::{Extension, Path, State},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use axum::http::HeaderMap;
use futures_util::{SinkExt, StreamExt};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::net::TcpListener;

fn identity(headers: &HeaderMap) -> Result<ContentIdentity> {
    let presented = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let (tenant, user, permissions) = match presented {
        "Bearer editor" => ("tenant-a", "editor-a", vec!["knowledge:space:edit".into()]),
        "Bearer stranger" => ("tenant-b", "stranger-b", vec!["knowledge:space:view".into()]),
        "Bearer denied" => {
            return Ok(ContentIdentity {
                tenant_id: "tenant-a".into(),
                user_id: "denied-a".into(),
                is_authenticated: true,
                roles: vec!["guest".into()],
                permissions: vec![],
                tier: "normal".into(),
                allowed_models: None,
            });
        }
        _ => {
            return Ok(ContentIdentity {
                is_authenticated: false,
                user_id: "anon".into(),
                tenant_id: String::new(),
                roles: vec!["guest".into()],
                ..ContentIdentity::default()
            });
        }
    };
    Ok(ContentIdentity {
        tenant_id: tenant.into(),
        user_id: user.into(),
        is_authenticated: true,
        roles: vec!["user".into()],
        permissions,
        tier: "normal".into(),
        allowed_models: None,
    })
}

fn signer() -> Arc<GatewayAuth> {
    let mut keys = std::collections::BTreeMap::new();
    keys.insert("local".into(), "collab-proxy-test-key".into());
    Arc::new(
        GatewayAuth::signed_v2(keys, auth::ReplayStore::memory()).expect("test signer"),
    )
}

#[derive(Clone)]
struct Hits(Arc<AtomicUsize>);

async fn upstream_collab(
    State(hits): State<Hits>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if user.tenant_id != "tenant-a" || user.user_id != "editor-a" || item_id == "hidden" {
        return ContentError::new(404, "space item not found").into_response();
    }
    hits.0.fetch_add(1, Ordering::SeqCst);
    upgrade.on_upgrade(|mut socket| async move {
        if let Some(Ok(Message::Binary(bytes))) = socket.recv().await {
            let _ = socket.send(Message::Binary(bytes)).await;
        }
    })
}

async fn spawn(app: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server");
    });
    address
}

async fn connect(
    address: std::net::SocketAddr,
    authorization: Option<&str>,
    item_id: &str,
) -> std::result::Result<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut request = format!("ws://{address}/api/v1/knowledge/space-items/{item_id}/collab")
        .into_client_request()
        .expect("client request");
    if let Some(authorization) = authorization {
        request.headers_mut().insert(
            "authorization",
            authorization.parse().expect("authorization header"),
        );
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

fn http_status(error: tokio_tungstenite::tungstenite::Error) -> u16 {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
        other => panic!("expected HTTP rejection, got {other}"),
    }
}

#[tokio::test]
async fn gateway_proxies_collab_upgrade_and_enforces_auth_acl() {
    let hits = Arc::new(AtomicUsize::new(0));
    let knowledge_auth = signer();
    let knowledge = Router::new()
        .route(
            "/api/v1/knowledge/space-items/{item_id}/collab",
            get(upstream_collab),
        )
        .with_state(Hits(hits.clone()))
        .layer(axum::middleware::from_fn_with_state(
            knowledge_auth.clone(),
            auth::verify_gateway,
        ));
    let knowledge_address = spawn(knowledge).await;
    let origin = reqwest::Url::parse(&format!("http://{knowledge_address}")).expect("origin");
    let gateway = Arc::new(
        KnowledgeProxy::collab_fixture(origin, signer(), Arc::new(identity)).expect("proxy"),
    );
    let gateway_address = spawn(router(gateway)).await;

    let missing = connect(gateway_address, None, "page-ok")
        .await
        .expect_err("anonymous upgrade");
    assert_eq!(http_status(missing), 401);
    let denied = connect(gateway_address, Some("Bearer denied"), "page-ok")
        .await
        .expect_err("missing space permission");
    assert_eq!(http_status(denied), 403);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let hidden = connect(gateway_address, Some("Bearer editor"), "hidden")
        .await
        .expect_err("object acl");
    assert_eq!(http_status(hidden), 404);
    let stranger = connect(gateway_address, Some("Bearer stranger"), "page-ok")
        .await
        .expect_err("other tenant");
    assert_eq!(http_status(stranger), 404);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let mut socket = connect(gateway_address, Some("Bearer editor"), "page-ok")
        .await
        .expect("editor upgrade");
    socket
        .send(tokio_tungstenite::tungstenite::Message::binary(b"space-frame".as_slice()))
        .await
        .expect("send");
    let echoed = socket.next().await.expect("frame").expect("decoded");
    match echoed {
        tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
            assert_eq!(bytes.as_ref(), b"space-frame");
        }
        other => panic!("expected binary echo, got {other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
