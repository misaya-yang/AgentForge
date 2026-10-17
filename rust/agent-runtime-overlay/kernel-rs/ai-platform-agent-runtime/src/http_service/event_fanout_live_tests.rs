//! Explicit integration test on an operator-owned synthetic thread. No provider
//! or tools run; durable probe envelopes are fixture data, not product acceptance.
use std::{sync::Arc, time::Duration};
use axum::http::HeaderMap;
use codex_protocol::ThreadId;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;
use crate::{AssistantTurnEventV1, PlatformThreadIdentity, PostgresThreadStore};

fn required(name: &str) -> String { std::env::var(name).expect("explicit private fanout test input required") }
fn headers(token: &str, scope: &PlatformThreadIdentity) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (key, value) in [("x-ai-platform-internal-token", token), ("x-ai-tenant-id", scope.tenant_id.as_str()),
        ("x-ai-user-id", scope.user_id.as_str()), ("x-ai-session-id", scope.session_id.as_str())] {
        headers.insert(key, value.parse().unwrap());
    }
    headers
}
async fn next_probe(response: &mut reqwest::Response, pending: &mut Vec<u8>, probe: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            while let Some(end) = pending.windows(2).position(|w| w == b"\n\n") {
                let frame = String::from_utf8(pending[..end].to_vec()).unwrap();
                pending.drain(..end + 2);
                for data in frame.lines().filter_map(|l| l.strip_prefix("data: ")) {
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        if value["event"]["data"].get("fanout_probe").is_some() {
                            assert_eq!(value["event"]["data"]["fanout_probe"], probe,
                                "query/header precedence and replay order must not resend an earlier probe");
                            return value;
                        }
                    }
                }
            }
            let next = response.chunk().await.expect("SSE must remain readable").expect("SSE closed before probe");
            pending.extend_from_slice(&next);
            assert!(pending.len() <= 1_048_576, "bounded SSE frame required");
        }
    }).await.expect("committed probe must arrive before the 7s polling backstop")
}
async fn append_probe(store: &PostgresThreadStore, scope: &PlatformThreadIdentity, probe: &str) -> i64 {
    let event_id = Uuid::new_v4();
    store.append_v1_event(scope.runtime_thread_id, event_id, &format!("compat/fanout-probe/{event_id}"),
        &AssistantTurnEventV1::new("text_delta", json!({"thread_id":scope.runtime_thread_id,
            "session_id":scope.session_id,"content":"","fanout_probe":probe})))
        .await.expect("dedicated fixture probe must commit under actual Runtime role")
}

#[tokio::test]
#[ignore = "explicit two running Runtime endpoints, scoped DSN and dedicated synthetic thread required"]
async fn real_runtime_fanout_replays_header_drains_pages_and_wakes_both_instances() {
    assert_eq!(required("AI_PLATFORM_FANOUT_ALLOW_FIXTURE_APPEND"), "1", "explicit fixture mutation opt-in required");
    let first_url = required("AI_PLATFORM_FANOUT_RUNTIME_A_URL");
    let second_url = required("AI_PLATFORM_FANOUT_RUNTIME_B_URL");
    assert_ne!(first_url, second_url, "independent endpoints required; operator must pin distinct processes/images");
    let scope = PlatformThreadIdentity::new(ThreadId::from_string(&required("AI_PLATFORM_FANOUT_THREAD_ID")).unwrap(),
        required("AI_PLATFORM_FANOUT_TENANT_ID"), required("AI_PLATFORM_FANOUT_USER_ID"), required("AI_PLATFORM_FANOUT_SESSION_ID"));
    let token = required("AI_PLATFORM_FANOUT_INTERNAL_TOKEN");
    let connected = sqlx::postgres::PgPoolOptions::new().max_connections(2)
        .connect(&required("AI_PLATFORM_RUNTIME_NOTIFY_TEST_DATABASE_URL")).await;
    assert!(connected.is_ok(), "actual scoped Runtime connection required");
    let pool = connected.unwrap();
    let principal = sqlx::query("SELECT current_user=session_user AS login, rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls FROM pg_catalog.pg_roles WHERE rolname=current_user AND current_user ~ '_runtime$'")
        .fetch_one(&pool).await.expect("actual Runtime login required");
    assert!(principal.get::<bool,_>("login") && principal.get::<bool,_>("rolcanlogin"));
    for key in ["rolsuper","rolcreatedb","rolcreaterole","rolreplication","rolbypassrls"] { assert!(!principal.get::<bool,_>(key)); }
    let root = Uuid::parse_str(&scope.runtime_thread_id.to_string()).unwrap();
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assistant_runs WHERE harness_thread_id=$1 AND status IN ('running','awaiting_approval')")
        .bind(root).fetch_one(&pool).await.unwrap();
    assert_eq!(active, 0, "fixture thread must have no active business turn");
    let store = Arc::new(PostgresThreadStore::from_pool(pool.clone()));
    store.verify_root_identity(&scope).await.unwrap();
    let baseline: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(sequence),0)::bigint FROM assistant_runtime_items WHERE runtime_thread_id=$1")
        .bind(root).fetch_one(&pool).await.unwrap();
    let probes = [Uuid::new_v4().to_string(), Uuid::new_v4().to_string(), Uuid::new_v4().to_string()];
    let mut sequences = Vec::new();
    for probe in &probes { sequences.push(append_probe(&store, &scope, probe).await); }
    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(30)).build().unwrap();
    let mut resume_headers = headers(&token, &scope);
    resume_headers.insert("last-event-id", baseline.to_string().parse().unwrap());
    let mut first = client.get(format!("{first_url}/internal/v1/threads/{}/events?limit=1",scope.runtime_thread_id))
        .headers(resume_headers).send().await.unwrap();
    assert!(first.status().is_success());
    let mut first_pending = Vec::new();
    for (probe, sequence) in probes.iter().zip(&sequences) {
        let value = next_probe(&mut first, &mut first_pending, probe).await;
        assert_eq!(value["sequence"], *sequence);
    }
    // Query cursor overrides the stale header; no earlier probe may be replayed.
    let mut second_headers = headers(&token, &scope);
    second_headers.insert("last-event-id", baseline.to_string().parse().unwrap());
    let mut second = client.get(format!("{second_url}/internal/v1/threads/{}/events?after_sequence={}&limit=1",scope.runtime_thread_id,sequences[2]))
        .headers(second_headers).send().await.unwrap();
    assert!(second.status().is_success());
    let live = Uuid::new_v4().to_string();
    let live_sequence = append_probe(&store, &scope, &live).await;
    // This test calls the production store directly, without either HTTP
    // instance's local hub.send. Delivery before backstop requires PG wakeup.
    let mut second_pending = Vec::new();
    let (a,b) = tokio::join!(next_probe(&mut first,&mut first_pending,&live), next_probe(&mut second,&mut second_pending,&live));
    assert_eq!(a["sequence"], live_sequence); assert_eq!(a, b);
    let mut wrong_scope = headers(&token, &scope);
    wrong_scope.insert("x-ai-tenant-id", format!("wrong-{}",Uuid::new_v4()).parse().unwrap());
    let denied = client.get(format!("{second_url}/internal/v1/threads/{}/events",scope.runtime_thread_id))
        .headers(wrong_scope).send().await.unwrap();
    assert_eq!(denied.status().as_u16(),404,"fanout cannot grant another tenant access");
    drop(first); drop(second); drop(store); pool.close().await;
}
