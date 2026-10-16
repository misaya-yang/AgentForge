//! Actual native Bridge requests against a bounded loopback Gateway fixture.
use super::*;
use std::{
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
pub(super) fn body(stream: bool) -> Value {
    json!({"protocol":"ai-platform/sdk-agent/v1","framework":"openai_agents","tenant_id":"tenant","user_id":"user","service_id":"service","request_id":"request","session_id":"session","agent":{"model":"model","instructions":"Help","max_turns":2},"input":"Hello","stream":stream,"capability_bridge":{"grant":"a".repeat(64),"run_id":"00000000-0000-4000-8000-000000000001","tools":[],"timeout_seconds":300}})
}
pub(super) fn launch(v: Value) -> Launch {
    let mut l = crate::contract::parse_request(v).unwrap();
    l.validate("synthetic-provider-key").unwrap();
    l
}
pub(super) fn fixture(
    count: usize,
    reply: impl Fn(&str, Value) -> (u16, Value) + Send + 'static,
) -> (Bridge, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let join = std::thread::spawn(move || {
        for _ in 0..count {
            let end = Instant::now() + Duration::from_secs(4);
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(s) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < end, "missing bridge request");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut data = vec![];
            let mut one = [0u8];
            while !data.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut one).unwrap();
                data.push(one[0]);
                assert!(data.len() < 16384);
            }
            let head = String::from_utf8(data).unwrap();
            assert!(
                head.to_lowercase()
                    .contains(&format!("authorization: bearer {}", "a".repeat(64)))
            );
            let length = head
                .lines()
                .find_map(|s| {
                    s.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut data = vec![0; length];
            socket.read_exact(&mut data).unwrap();
            let path = head
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let (code, value) = reply(path, serde_json::from_slice(&data).unwrap());
            let body = value.to_string();
            write!(socket,"HTTP/1.1 {code} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let bridge = Bridge {
        client: reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(),
        origin: Some(origin),
    };
    (bridge, join)
}
#[test]
fn pure_sync_and_stream_have_identity_without_tools() {
    for stream in [false, true] {
        let l = launch(body(stream));
        assert_eq!(l.timeout(), 300);
        assert!(l.capability_bridge.as_ref().unwrap().tools.is_empty());
        assert!(
            !descriptors(&l)
                .iter()
                .any(|t| t["name"] == "knowledge_retrieve")
        );
        assert!(!descriptors(&l).iter().any(|t| {
            t["name"]
                .as_str()
                .unwrap()
                .starts_with("platform_capability_")
        }));
    }
}
#[tokio::test]
async fn empty_tools_cannot_dispatch() {
    let l = launch(body(true));
    let b = Bridge {
        client: reqwest::Client::new(),
        origin: None,
    };
    assert!(
        b.invoke(&l, "platform_capability_0", json!({}), "real-call")
            .await
            .unwrap_err()
            .to_string()
            .contains("sdk_tool_not_allowed")
    );
}
#[tokio::test]
async fn empty_bridge_still_checks_durable_admission() {
    let (b, j) = fixture(1, |path, value| {
        assert_eq!(path, "/internal/v1/sdk-tools/capabilities/admit");
        assert_eq!(
            value,
            json!({"run_id":"00000000-0000-4000-8000-000000000001"})
        );
        (200, json!({"authorized":false}))
    });
    assert!(b.admit(&launch(body(true))).await.is_err());
    j.join().unwrap();
}
#[tokio::test]
async fn terminal_authority_check_does_not_spend_exhausted_kb_quota() {
    let used = Arc::new(AtomicUsize::new(1));
    let observed = used.clone();
    let (b, j) = fixture(20, move |path, _| {
        if path.ends_with("/capabilities/admit") {
            return (200, json!({"authorized":true}));
        }
        let cost = if path.ends_with("/knowledge/admit") {
            1
        } else {
            assert!(path.ends_with("/knowledge/retrieve"));
            2
        };
        let old = observed.load(Ordering::SeqCst);
        if old + cost > 32 {
            return (403, json!({"error":"quota"}));
        }
        observed.fetch_add(cost, Ordering::SeqCst);
        if cost == 1 {
            (200, json!({"authorized":true}))
        } else {
            (200, json!({"passages":[]}))
        }
    });
    let mut value = body(true);
    let dataset = "00000000-0000-4000-8000-000000000002";
    value["read_bridge"] = json!({"grant":"a".repeat(64),"dataset_ids":[dataset]});
    let l = launch(value);
    b.admit(&l).await.unwrap();
    for _ in 0..15 {
        b.invoke(
            &l,
            "knowledge_retrieve",
            json!({"dataset_id":dataset,"query":"query","top_k":1}),
            "",
        )
        .await
        .unwrap();
    }
    assert_eq!(used.load(Ordering::SeqCst), 32);
    b.admit_execution(&l).await.unwrap();
    assert_eq!(used.load(Ordering::SeqCst), 32);
    assert!(b.admit(&l).await.is_err());
    j.join().unwrap();
}
#[test]
fn malformed_empty_bridge_rejected() {
    for change in [
        json!({"tools":null}),
        json!({"tools":{}}),
        json!({"timeout_seconds":1800}),
        json!({"timeout_seconds":true}),
    ] {
        let mut value = body(true);
        for (k, v) in change.as_object().unwrap() {
            value["capability_bridge"][k] = v.clone();
        }
        let result = crate::contract::parse_request(value)
            .and_then(|mut l| l.validate("synthetic-provider-key"));
        assert!(result.is_err());
    }
    let mut value = body(true);
    value["capability_bridge"]
        .as_object_mut()
        .unwrap()
        .remove("tools");
    assert!(crate::contract::parse_request(value).is_err());
}
#[test]
fn pure_and_kb_profiles_remain_independently_scoped() {
    let pure = launch(body(true));
    assert_eq!(
        pure.profile(),
        json!({"schema":"sdk-session-tools/v1","dataset_ids":[]})
    );
    let mut value = body(true);
    value["read_bridge"] =
        json!({"grant":"a".repeat(64),"dataset_ids":["00000000-0000-4000-8000-000000000002"]});
    let kb = launch(value);
    assert!(
        descriptors(&kb)
            .iter()
            .any(|t| t["name"] == "knowledge_retrieve")
    );
    assert!(kb.capability_bridge.as_ref().unwrap().tools.is_empty());
    assert_ne!(pure.profile(), kb.profile());
}
#[test]
fn pure_profile_remains_compatible_on_disk() {
    let state = tempfile::tempdir().unwrap();
    let path = state.path().join("sdk-profile.json");
    let original = br#"{"schema":"sdk-session-tools/v1","dataset_ids":[]}"#;
    std::fs::write(&path, original).unwrap();
    crate::engine::ensure_profile(&launch(body(true)), state.path()).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), original);
}
#[test]
fn kb_profile_remains_independently_scoped_on_disk() {
    let state = tempfile::tempdir().unwrap();
    let mut request = body(true);
    request["read_bridge"] =
        json!({"grant":"a".repeat(64),"dataset_ids":["00000000-0000-4000-8000-000000000002"]});
    let l = launch(request);
    assert!(
        descriptors(&l)
            .iter()
            .any(|t| t["name"] == "knowledge_retrieve")
    );
    assert!(l.capability_bridge.as_ref().unwrap().tools.is_empty());
    crate::engine::ensure_profile(&l, state.path()).unwrap();
    let before = std::fs::read(state.path().join("sdk-profile.json")).unwrap();
    assert_eq!(
        crate::engine::ensure_profile(&launch(body(true)), state.path())
            .unwrap_err()
            .to_string(),
        "sdk_session_profile_changed"
    );
    assert_eq!(
        std::fs::read(state.path().join("sdk-profile.json")).unwrap(),
        before
    );
}
