//! Local publisher for the gateway's A2A 1.0 agent card.
//! The route is the same `card_router` mounted on the invocation gateway.
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "8765".to_owned());
    let port: u16 = port.parse().expect("port");
    let app = ai_platform_gateway_foundation::invocation_http::a2a::card_router();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("address");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind");
    println!("a2a-card listening on http://{addr}/.well-known/agent-card.json");
    axum::serve(listener, app).await.expect("serve");
}
