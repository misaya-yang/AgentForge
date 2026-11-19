//! Original gateway_performance_baseline observable contracts against the native library.
use ai_platform_tools::performance::*;
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const RUN: &str = "abcdef01-2345-6789-abcd-ef0123456789";
fn profile() -> ReadProfile {
    ReadProfile::new("public_liveness".into(), None, None, None).unwrap()
}
fn owned(token: &str) -> Result<ReadProfile> {
    ReadProfile::new(
        "owner_terminal_sdk_read".into(),
        Some(RUN.into()),
        Some("openai_agents".into()),
        Some(token.into()),
    )
}
#[test]
fn empirical_quantiles_and_empty_are_honest() {
    let v = distribution((1..=100).map(f64::from).collect()).unwrap();
    assert_eq!(
        (v["p50"].as_f64(), v["p95"].as_f64(), v["p99"].as_f64()),
        (Some(50.), Some(95.), Some(99.))
    );
    assert_eq!(v["tail_sample_warning"], false);
    assert!(distribution(vec![]).unwrap()["p99"].is_null());
    assert_eq!(
        distribution(vec![1., 2.]).unwrap()["tail_sample_warning"],
        true
    );
    assert!(distribution(vec![f64::NAN]).is_err());
}
#[test]
fn denominators_include_errors_cancelled_and_not_started() {
    let rows = vec![
        Observation {
            status: Some(200),
            outcome: "success".into(),
            elapsed_ms: Some(2.),
            ..Default::default()
        },
        Observation {
            status: Some(429),
            outcome: "rate_limited".into(),
            elapsed_ms: Some(1.),
            ..Default::default()
        },
        Observation {
            outcome: "timeout".into(),
            elapsed_ms: Some(100.),
            ..Default::default()
        },
        Observation {
            outcome: "not_started".into(),
            ..Default::default()
        },
    ];
    let v = summarize(&rows, 2.).unwrap();
    for (key, expected) in [
        ("scheduled_requests", 4.),
        ("successful_requests", 1.),
        ("non_success_fraction", 0.75),
        ("http_responses_per_second", 1.),
        ("success_per_second", 0.5),
    ] {
        assert_eq!(v[key].as_f64(), Some(expected));
    }
    assert_eq!(v["success_elapsed_ms"]["p99"], 2.);
    assert_eq!(v["all_attempt_elapsed_ms"]["p99"], 100.);
    assert_eq!(v["rejection_elapsed_ms"]["p99"], 1.);
    assert_eq!(v["outcomes"]["not_started"], 1);
}
#[test]
fn no_invalid_wall_window() {
    for n in [0., -1., f64::NAN] {
        assert!(summarize(&[], n).is_err());
    }
}
#[test]
fn process_cpu_is_one_gateway_pid_not_machine_percent() {
    let v=process_summary(&[json!({"monotonic_seconds":1,"cpu_seconds":3,"rss_bytes":100,"open_fd_count":10,"open_socket_count":3}),json!({"monotonic_seconds":3,"cpu_seconds":4,"rss_bytes":200,"open_fd_count":12,"open_socket_count":4})]).unwrap();
    assert_eq!(v["gateway_cpu_percent_of_one_core"], 50.);
    assert_eq!(v["sampled_peak_rss_bytes"], 200);
    assert!(v["scope"].as_str().unwrap().contains("not a DB pool"));
}
#[cfg(target_os = "linux")]
fn process() -> ProcessIdentity {
    let pid = std::process::id();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    ProcessIdentity {
        pid,
        start_ticks: fields[19].parse().unwrap(),
    }
}
#[cfg(target_os = "linux")]
#[test]
fn live_self_proc_parser_and_stale_start_rejection_are_read_only() {
    let identity = process();
    let v = identity.sample().unwrap();
    assert!(v["rss_bytes"].as_u64().unwrap() > 0);
    assert!(v["open_fd_count"].as_u64().unwrap() > 0);
    assert!(
        ProcessIdentity {
            start_ticks: identity.start_ticks + 1,
            ..identity
        }
        .sample()
        .is_err()
    );
}
#[test]
fn scope_cannot_claim_unmeasured_enterprise_acceptance() {
    let v = plan();
    assert_eq!(v["status"], "PREPARED_NOT_EXECUTED");
    for key in [
        "provider_latency",
        "gateway_exclusive_overhead",
        "server_queue_delay",
        "streaming_first_token_inter_chunk",
        "knowledge_retrieval",
        "database_pool_pressure",
        "execution_cancellation",
    ] {
        assert!(v["scope"]["unavailable"].get(key).is_some());
    }
}
#[test]
fn only_exact_fixed_get_profiles() {
    assert!(ReadProfile::new("http://external.example/".into(), None, None, None).is_err());
    assert!(
        ReadProfile::new(
            "public_liveness".into(),
            None,
            None,
            Some("synthetic".into())
        )
        .is_err()
    );
    for id in [
        "../../health",
        "?redirect=external",
        "not-a-uuid",
        &RUN.to_uppercase(),
    ] {
        assert!(
            ReadProfile::new(
                "owner_terminal_sdk_read".into(),
                Some(id.into()),
                Some("openai_agents".into()),
                Some("synthetic".into())
            )
            .is_err()
        );
    }
}
#[test]
fn no_header_injection_or_invalid_token() {
    for token in [
        "x\r\nHost: external",
        "x\nx",
        "x x",
        "é",
        "",
        &"x".repeat(16385),
    ] {
        assert!(owned(token).is_err());
    }
    assert!(owned("synthetic.unit.test").is_ok());
}
#[test]
fn safety_budgets_reject_unbounded_work() {
    for change in [
        json!({"concurrency":9}),
        json!({"measured_requests":201}),
        json!({"concurrency":true}),
        json!({"case_timeout_seconds":31}),
        json!({"sample_interval_seconds":0.001}),
    ] {
        let mut v = serde_json::to_value(Budget::default()).unwrap();
        for (k, value) in change.as_object().unwrap() {
            v[k] = value.clone();
        }
        assert!(serde_json::from_value::<Budget>(v).map_or(true, |b| b.validate().is_err()));
    }
    let b = Budget {
        request_timeout_seconds: f64::NAN,
        ..Default::default()
    };
    assert!(b.validate().is_err());
}
fn response(status: u16, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).into_bytes()
}
async fn request_head(s: &mut tokio::net::TcpStream) -> String {
    let mut b = vec![];
    while !b.ends_with(b"\r\n\r\n") {
        b.push(s.read_u8().await.unwrap());
        assert!(b.len() < 20000);
    }
    String::from_utf8(b).unwrap()
}
// The production profile fixes loopback:18080. Bind first and fail closed if occupied;
// never send a request to an ambient service. One test owns all transport scenarios.
#[tokio::test]
async fn bounded_real_transport_source_matrix() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18080")
        .await
        .expect("isolated fixed profile port must be free");
    async fn exchange(
        listener: &tokio::net::TcpListener,
        wire: Vec<u8>,
        p: ReadProfile,
    ) -> (Observation, String) {
        let server = async {
            let (mut s, _) = listener.accept().await.unwrap();
            let head = request_head(&mut s).await;
            s.write_all(&wire).await.unwrap();
            s.shutdown().await.unwrap();
            head
        };
        let client = async {
            let mut c = HttpConnection::default();
            let mut row = Observation::default();
            c.request(&p, &mut row, 0.2).await;
            c.close().await;
            row
        };
        let (head, row) = tokio::join!(server, client);
        (row, head)
    }
    let (r, head) = exchange(&listener, response(200, r#"{"status":"alive"}"#), profile()).await;
    assert_eq!(r.outcome, "success");
    assert!(head.starts_with("GET /health/live HTTP/1.1\r\nHost: 127.0.0.1:18080\r\n"));
    assert!(r.first_response_byte_ms.unwrap() <= r.response_headers_ms.unwrap());
    assert!(r.response_headers_ms.unwrap() <= r.first_body_byte_ms.unwrap());
    assert!(r.first_body_byte_ms.unwrap() <= r.elapsed_ms.unwrap());
    let chunked=b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n12\r\n{\"status\":\"alive\"}\r\n0\r\n\r\n".to_vec();
    assert_eq!(
        exchange(&listener, chunked, profile()).await.0.outcome,
        "success"
    );
    for (status, outcome) in [
        (302, "unexpected_http_status"),
        (429, "rate_limited"),
        (503, "unavailable"),
    ] {
        let (r, _) = exchange(&listener, response(status, "{}"), profile()).await;
        assert_eq!(r.outcome, outcome);
        assert_eq!(r.status, Some(status));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
    }
    for wire in [
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 99\r\n\r\n{}"
            .to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 65537\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n{}".to_vec(),
    ] {
        assert_eq!(
            exchange(&listener, wire, profile()).await.0.outcome,
            "transport_or_protocol_error"
        );
    }
    for body in ["not-json", "{}", r#"{"status":"dead"}"#] {
        assert_eq!(
            exchange(&listener, response(200, body), profile())
                .await
                .0
                .outcome,
            "invalid_success_body"
        );
    }
    let valid = json!({"run":{"run_id":RUN,"engine":"openai_agents","status":"cancelled","harness_thread_id":null,"harness_turn_id":null}});
    assert_eq!(
        exchange(
            &listener,
            response(200, &valid.to_string()),
            owned("synthetic").unwrap()
        )
        .await
        .0
        .outcome,
        "success"
    );
    for (k, value) in [
        ("run_id", json!("foreign")),
        ("status", json!("running")),
        ("engine", json!("claude_agent_sdk")),
        ("harness_thread_id", json!("fabricated")),
        ("request_id", json!("private")),
    ] {
        let mut v = valid.clone();
        v["run"][k] = value;
        assert_eq!(
            exchange(
                &listener,
                response(200, &v.to_string()),
                owned("synthetic").unwrap()
            )
            .await
            .0
            .outcome,
            "invalid_success_body"
        );
    }
    let server = async {
        let (mut s, _) = listener.accept().await.unwrap();
        request_head(&mut s).await;
        let mut b = [0u8];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), s.read(&mut b))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    };
    let client = async {
        let mut c = HttpConnection::default();
        let mut r = Observation::default();
        c.request(&profile(), &mut r, 0.05).await;
        assert_eq!(r.outcome, "timeout");
    };
    tokio::join!(server, client);
    let server = async {
        let (mut s, _) = listener.accept().await.unwrap();
        for _ in 0..2 {
            request_head(&mut s).await;
            let wire = response(200, r#"{"status":"alive"}"#);
            let wire = String::from_utf8(wire)
                .unwrap()
                .replace("Connection: close", "Connection: keep-alive");
            s.write_all(wire.as_bytes()).await.unwrap();
        }
    };
    let client = async {
        let mut c = HttpConnection::default();
        for reused in [false, true] {
            let mut r = Observation::default();
            c.request(&profile(), &mut r, 0.2).await;
            assert_eq!(r.outcome, "success");
            assert_eq!(r.reused_connection, reused);
        }
        c.close().await;
    };
    tokio::join!(server, client);
    #[cfg(target_os = "linux")]
    orchestration_source_cases(&listener).await;
}
#[tokio::test]
async fn ownership_failure_precedes_process_sampling_and_transport_construction() {
    let checks = std::cell::Cell::new(0);
    let result = measure_case(
        profile(),
        Budget::default(),
        ProcessIdentity {
            pid: u32::MAX,
            start_ticks: 0,
        },
        || {
            checks.set(checks.get() + 1);
            Err("stale_owner".into())
        },
    )
    .await;
    assert_eq!(result.unwrap_err().to_string(), "stale_owner");
    assert_eq!(checks.get(), 1);
}
#[cfg(target_os = "linux")]
async fn measure_over_owned_fixture<F: Fn() -> Result<()>>(
    listener: &tokio::net::TcpListener,
    p: ReadProfile,
    budget: Budget,
    verify: F,
    status: u16,
    body: String,
) -> (serde_json::Value, usize, usize) {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let server = async {
        let mut workers = tokio::task::JoinSet::new();
        let mut calls = 0;
        loop {
            tokio::select! {_=&mut stopped=>break,accepted=listener.accept()=>{
                let(mut socket,_)=accepted.unwrap();calls+=1;let active=active.clone();let peak=peak.clone();let wire=response(status,&body);
                workers.spawn(async move {let n=active.fetch_add(1,Ordering::SeqCst)+1;peak.fetch_max(n,Ordering::SeqCst);request_head(&mut socket).await;tokio::time::sleep(Duration::from_millis(10)).await;socket.write_all(&wire).await.unwrap();socket.shutdown().await.unwrap();active.fetch_sub(1,Ordering::SeqCst);});
            }}
        }
        while let Some(done) = workers.join_next().await {
            done.unwrap();
        }
        calls
    };
    let client = async {
        let result = measure_case(p, budget, process(), verify).await;
        let _ = stop.send(());
        result.unwrap()
    };
    let (calls, value) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, client)
    })
    .await
    .unwrap();
    (value, calls, peak.load(Ordering::SeqCst))
}
#[cfg(target_os = "linux")]
async fn orchestration_source_cases(listener: &tokio::net::TcpListener) {
    let checks = std::cell::Cell::new(0);
    let body=json!({"run":{"run_id":RUN,"engine":"openai_agents","status":"cancelled","harness_thread_id":null,"harness_turn_id":null}}).to_string();
    let (result, calls, peak) = measure_over_owned_fixture(
        listener,
        owned("synthetic-private-token").unwrap(),
        Budget {
            concurrency: 2,
            measured_requests: 6,
            ..Default::default()
        },
        || {
            checks.set(checks.get() + 1);
            Ok(())
        },
        200,
        body,
    )
    .await;
    assert_eq!(checks.get(), 2);
    assert_eq!(calls, 8);
    assert_eq!(peak, 2);
    assert_eq!(result["warmup"]["requests"], 2);
    assert_eq!(result["summary"]["successful_requests"], 6);
    assert_eq!(result["summary"]["client_batch_queue_ms"]["count"], 6);
    assert!(!result.to_string().contains("synthetic-private-token"));
    assert!(!result.to_string().contains(RUN));
    assert_eq!(
        result["load_generator_cpu_window"],
        "measured_batch_excluding_warmup"
    );
    let checks = std::cell::Cell::new(0);
    let (result, calls, _) = measure_over_owned_fixture(
        listener,
        profile(),
        Budget {
            measured_requests: 2,
            ..Default::default()
        },
        || {
            checks.set(checks.get() + 1);
            if checks.get() == 2 {
                Err("identity_changed".into())
            } else {
                Ok(())
            }
        },
        200,
        r#"{"status":"alive"}"#.into(),
    )
    .await;
    assert_eq!(calls, 3);
    assert_eq!(result["status"], "IDENTITY_VERIFICATION_FAILED");
    assert_eq!(result["observations"].as_array().unwrap().len(), 2);
    assert_eq!(result["summary"]["successful_requests"], 2);
    let (result, calls, _) = measure_over_owned_fixture(
        listener,
        profile(),
        Budget::default(),
        || Ok(()),
        429,
        "{}".into(),
    )
    .await;
    assert_eq!(calls, 1);
    assert_eq!(result["status"], "WARMUP_FAILED");
    assert_eq!(result["summary"]["outcomes"], json!({"not_started":12}));
}
