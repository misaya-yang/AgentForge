use super::*;
use config::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn example() -> Profile {
    serde_json::from_str(include_str!("profile.example.json")).unwrap()
}
fn service(name: &str, pid: u32) -> identity::Service {
    identity::Service {
        id: name.into(),
        telemetry_service: name.into(),
        instance_id: format!("{name}-instance"),
        pid,
        start_ticks: 10,
        uid: 10001,
        container_id: format!("{:064x}", pid),
        image_id: format!("sha256:{}", "b".repeat(64)),
        binary_sha256: "c".repeat(64),
        environment_sha256: "d".repeat(64),
        materials: vec![],
        network_addresses: vec![],
        runtime_settings: json!({"memory_limit_bytes":1024,"privileged":false}),
        listener_port: Some(8080),
    }
}
fn accepted(p: &Profile) -> identity::Accepted {
    identity::Accepted {
        schema: "agentforge-accepted-performance-build/v1".into(),
        accepted: true,
        scope: "isolated-owned-performance-fixture".into(),
        expires_unix: chrono::Utc::now().timestamp() + 300,
        profile_sha256: "a".repeat(64),
        controller_binary_sha256: "a".repeat(64),
        source_manifest: identity::FileRef {
            path: "/not-created/source.json".into(),
            sha256: "b".repeat(64),
        },
        source_roots: vec![],
        boot_id: "test-only".into(),
        compose_project: "test-only".into(),
        deployment_working_dir: "/test-only".into(),
        services: p
            .services
            .iter()
            .enumerate()
            .map(|(i, s)| service(s, i as u32 + 1))
            .collect(),
        gates: [
            "build_acceptance",
            "privilege_tenant_acl",
            "fixture_ownership",
            "accounting",
        ]
        .into_iter()
        .map(|k| {
            (
                k.into(),
                identity::Gate {
                    document: identity::FileRef {
                        path: "/test-only/evidence.json".into(),
                        sha256: "e".repeat(64),
                    },
                    pointer: "/accepted".into(),
                    expected: json!(true),
                },
            )
        })
        .collect(),
        docker_binary: identity::FileRef {
            path: "/not-created/docker".into(),
            sha256: "f".repeat(64),
        },
        docker_socket: "/not-created/docker.sock".into(),
        real_provider_authorized: false,
    }
}
#[test]
fn no_build_is_blocked_and_never_an_accepted_receipt() {
    let p = plan();
    assert_eq!(p["status"], "BLOCKED");
    assert_eq!(p["acceptance"], false);
    assert_eq!(p["network_executed"], false);
    assert_eq!(command(&["plan".into()]).unwrap(), p);
}
#[test]
fn fixed_complete_profile_and_control_negatives() {
    let p = example();
    validate_profile(&p).unwrap();
    for key in ["authentication", "limiter", "tenant_acl", "billing"] {
        let mut bad = p.clone();
        bad.environment.controls.insert(key.into(), false);
        assert!(validate_profile(&bad).is_err());
    }
    let mut bad = p.clone();
    bad.cases[1].stages.remove("embedding");
    assert!(validate_profile(&bad).is_err());
    let mut bad = p.clone();
    bad.cases[2].path = "/api/v1/knowledge/datasets".into();
    assert!(validate_profile(&bad).is_err());
    let mut bad = p.clone();
    bad.cases[0].body["stream"] = json!(true);
    assert!(validate_profile(&bad).is_err());
}
#[test]
fn remote_redirect_query_and_dot_paths_are_not_allowed() {
    for raw in [
        "http://example.com:8080",
        "http://localhost:8080",
        "http://0.0.0.0:8080",
        "http://127.0.0.1:8080/a",
        "http://127.0.0.1:8080?secret=x",
        "http://user:secret@127.0.0.1:8080",
    ] {
        assert!(origin(raw).is_err());
    }
    for raw in ["//evil/a", "/../a", "/a/%2e%2e", "/a?x=secret"] {
        assert!(!path(raw));
    }
    assert!(mock_origin("http://172.18.0.1:18089").is_ok());
    assert!(mock_origin("http://8.8.8.8:18089").is_err());
}
#[test]
fn signatures_require_external_key_exact_bytes_and_domain() {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(&[7; 32]);
    let bytes = b"unit-test-only-not-a-service-acceptance";
    let mut message = b"agentforge-performance-accepted-build/v1\0".to_vec();
    message.extend_from_slice(bytes);
    let signature = key.sign(&message);
    identity::verify_signature(bytes, &signature.to_bytes(), key.verifying_key().as_bytes())
        .unwrap();
    assert!(
        identity::verify_signature(
            b"changed",
            &signature.to_bytes(),
            key.verifying_key().as_bytes()
        )
        .is_err()
    );
    assert!(
        identity::verify_signature(
            bytes,
            &key.sign(bytes).to_bytes(),
            key.verifying_key().as_bytes()
        )
        .is_err()
    );
    assert!(
        identity::verify_signature(
            bytes,
            &signature.to_bytes(),
            SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes()
        )
        .is_err()
    );
}
#[test]
fn signed_false_stale_or_different_profile_does_not_admit() {
    let p = example();
    let a = accepted(&p);
    identity::validate_acceptance(&a, &p, &"a".repeat(64)).unwrap();
    let mut b = a.clone();
    b.accepted = false;
    assert!(identity::validate_acceptance(&b, &p, &"a".repeat(64)).is_err());
    let mut b = a.clone();
    b.expires_unix = 0;
    assert!(identity::validate_acceptance(&b, &p, &"a".repeat(64)).is_err());
    let mut b = a.clone();
    b.services[1].pid = b.services[0].pid;
    assert!(identity::validate_acceptance(&b, &p, &"a".repeat(64)).is_err());
    assert!(identity::validate_acceptance(&a, &p, &"b".repeat(64)).is_err());
    let mut b = a.clone();
    b.services[1].telemetry_service = b.services[0].telemetry_service.clone();
    assert!(identity::validate_acceptance(&b, &p, &"a".repeat(64)).is_err());
    let mut b = a.clone();
    b.gates.get_mut("accounting").unwrap().expected = json!(false);
    assert!(identity::validate_acceptance(&b, &p, &"a".repeat(64)).is_err());
}
#[test]
fn deterministic_upstream_reports_exact_usage_without_real_provider() {
    let c = example().mock.unwrap();
    let b = json!({"messages":[{"role":"user","content":"fixed"}],"stream":false});
    let a = mock::response("/v1/chat/completions", &b, &c).unwrap();
    assert_eq!(a, mock::response("/v1/chat/completions", &b, &c).unwrap());
    assert_eq!(a["usage"]["total_tokens"], 40);
    let v = mock::response("/v1/embeddings", &json!({"input":["fixed","fixed"]}), &c).unwrap();
    assert_eq!(v["data"][0]["embedding"], v["data"][1]["embedding"]);
    assert_eq!(v["data"][0]["embedding"].as_array().unwrap().len(), 32);
    assert!(mock::response("/admin", &b, &c).is_err());
    assert!(
        mock::response(
            "/v1/chat/completions",
            &json!({"stream":true,"messages":[]}),
            &c
        )
        .is_err()
    );
}
#[test]
fn correlation_substitution_does_not_replace_other_private_fields() {
    let input = json!({"messages":[{"content":"fixed {{correlation}}"}],"private":"do-not-report"});
    let v = controller::substitute(&input, "af-perf:run:case:1");
    assert_eq!(v["private"], "do-not-report");
    assert_eq!(
        mock::marker(&v, "af-perf:run:").as_deref(),
        Some("af-perf:run:case:1")
    );
    assert!(mock::marker(&v, "af-perf:other:").is_none());
}
#[test]
fn gateway_boundaries_use_one_request_and_unknown_is_not_zero() {
    let mut c = example().cases.remove(0);
    for (name, b) in &mut c.boundaries {
        b.span = "http".into();
        b.event = Some(name.clone());
    }
    let names = [
        "receive",
        "admit",
        "upstream_dispatch",
        "upstream_first_byte",
        "upstream_complete",
        "response_complete",
    ];
    let span = trace::Span {
        service: "gateway".into(),
        name: "http".into(),
        start: 1_000_000,
        end: 7_000_000,
        attributes: BTreeMap::new(),
        events: names
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n.into(), 1_000_000 + i as u64 * 1_000_000))
            .collect(),
    };
    let measured = trace::metrics(&c, &json!({}), &[span.clone()]);
    assert_eq!(
        measured["gateway"]["gateway_excluding_upstream_interval_ms"],
        3.0
    );
    let missing = trace::metrics(&c, &json!({}), &[]);
    assert_eq!(missing["gateway"]["status"], "unknown");
    assert!(
        missing["gateway"]
            .get("gateway_excluding_upstream_interval_ms")
            .is_none()
    );
    assert_eq!(
        trace::union_ms(vec![(0, 5_000_000), (2_000_000, 8_000_000)]),
        8.0
    );
}
fn otlp(trace_id: &str, instance: &str) -> Value {
    json!({"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"gateway"}},{"key":"service.instance.id","value":{"stringValue":instance}}]},"scopeSpans":[{"spans":[{"traceId":trace_id,"spanId":"1111111111111111","name":"http","startTimeUnixNano":"100","endTimeUnixNano":"200"}]}]}]})
}
#[test]
fn trace_must_match_live_instance_and_same_request() {
    let service = service("gateway", 1);
    let id = "a".repeat(32);
    trace::spans(&otlp(&id, "gateway-instance"), &id, &[service.clone()]).unwrap();
    assert!(trace::spans(&otlp(&id, "other-instance"), &id, &[service.clone()]).is_err());
    assert!(trace::spans(&otlp(&"b".repeat(32), "gateway-instance"), &id, &[service]).is_err());
}
#[test]
fn stage_absence_parallel_work_and_relevance_do_not_manufacture_pass() {
    let mut c = example().cases.remove(1);
    let metrics = trace::metrics(&c, &json!({}), &[]);
    assert_eq!(metrics["status"], "partial_unknown");
    assert_eq!(
        metrics["retrieval_stages"]["embedding"]["status"],
        "unknown"
    );
    let r = controller::relevance(&c, &json!({"results":[{"document_id":"wrong"}]}));
    assert_eq!(r["status"], "failed_relevance_guard");
    assert_eq!(r["recall_at_k"], 0.0);
    c.stages.get_mut("rerank").unwrap().active_when = Some(Check {
        pointer: "/cache_hit".into(),
        equals: json!(false),
    });
    let metrics = trace::metrics(&c, &json!({"cache_hit":true}), &[]);
    assert_eq!(
        metrics["retrieval_stages"]["rerank"]["status"],
        "not_applicable"
    );
}
#[test]
fn errors_and_unstarted_work_stay_in_denominator_warmups_do_not() {
    let rows = json!([{"case":"a","kind":"deterministic_synthetic_upstream","provider_scope":"synthetic_or_local","phase":"warmup","body_validated":true,"http":{"elapsed_ms":100}}, {"case":"a","kind":"deterministic_synthetic_upstream","provider_scope":"synthetic_or_local","phase":"measured","body_validated":true,"outcome":"validated_http_success","http":{"elapsed_ms":20}}, {"case":"a","phase":"measured","outcome":"rate_limited","body_validated":false,"http":{"elapsed_ms":1}}, {"case":"a","phase":"measured","outcome":"not_started"}]);
    let value = controller::summary(rows.as_array().unwrap());
    assert_eq!(value[0]["scheduled_measured_requests"], 3);
    assert_eq!(value[0]["successful_client_elapsed_ms"]["p95"], 20.0);
    assert_eq!(value[0]["outcomes"]["rate_limited"], 1);
    assert_eq!(value[0]["status"], "PARTIAL_FAILED_OR_UNKNOWN");
    assert!(controller::summary(&[json!({"case":"failed","phase":"measured"})])[0]["successful_client_elapsed_ms"]["p95"].is_null());
}
#[test]
fn ingestion_batch_rate_uses_measured_wall_not_average_inverse_latencies() {
    let rows = json!([{"case":"ingest","phase":"measured","dispatch_offset_ms":0,"terminal_offset_ms":1000,"ingestion":{"status":"measured","confirmed_input_utf8_bytes":100}}, {"case":"ingest","phase":"measured","dispatch_offset_ms":4000,"terminal_offset_ms":5000,"ingestion":{"status":"measured","confirmed_input_utf8_bytes":100}}]);
    let summary = controller::summary(rows.as_array().unwrap());
    assert_eq!(summary[0]["ingestion_completed_documents_per_second"], 0.4);
    assert_eq!(
        summary[0]["ingestion_completed_input_utf8_bytes_per_second"],
        40.0
    );
}
#[test]
fn source_manifest_rejects_changed_and_added_real_files() {
    let root =
        std::env::temp_dir().join(format!("performance-source-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut p = example();
    let mut a = accepted(&p);
    let names = [
        "rust/platform-native/Cargo.toml",
        "rust/platform-native/Cargo.lock",
        "rust/platform-native/crates/platform-gateway/src/lib.rs",
        "rust/platform-native/crates/platform-knowledge-domain/src/lib.rs",
        "rust/platform-native/crates/platform-service-primitives/src/lib.rs",
        "rust/platform-native/crates/unit-fixture/src/credentials.rs",
        "rust/platform-native/vendor/unit-fixture/upstream-rsa-private.pem",
    ];
    let mut files = BTreeMap::new();
    for name in names {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "unit-only-source").unwrap();
        files.insert(name, sha(b"unit-only-source"));
    }
    a.source_roots = vec!["rust/platform-native".into()];
    let manifest = root.join("source.json");
    let bytes = serde_json::to_vec(
        &json!({"schema":"agentforge-performance-source-manifest/v1","files":files}),
    )
    .unwrap();
    std::fs::write(&manifest, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    a.source_manifest = identity::FileRef {
        path: manifest,
        sha256: sha(&bytes),
    };
    identity::verify_sources(&root, &a).unwrap();
    std::fs::write(root.join(names[0]), "changed").unwrap();
    assert!(identity::verify_sources(&root, &a).is_err());
    std::fs::write(root.join(names[0]), "unit-only-source").unwrap();
    // Public credential implementation names and upstream PEM test fixtures are
    // source, not operator secret files. They must still be inventoried exactly.
    std::fs::write(root.join("rust/platform-native/added.rs"), "added").unwrap();
    assert!(identity::verify_sources(&root, &a).is_err());
    std::fs::remove_dir_all(root).unwrap();
    let _ = &mut p;
}
#[tokio::test]
async fn transport_fixture_records_invalid_200_and_does_not_retry_status() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut input = [0; 4096];
        let _ = stream.read(&mut input).await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"wrong\":\"shape\"}").await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    let client = controller::client(1000).unwrap();
    let reply = controller::http(
        &client,
        &origin,
        "/fixture",
        reqwest::Method::POST,
        Some("unit-only-dummy-token"),
        Some(&json!({})),
        None,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 200);
    assert!(!controller::checks(
        &reply.body,
        &[Check {
            pointer: "/expected".into(),
            equals: json!(true)
        }]
    ));
    task.await.unwrap();
}
#[cfg(unix)]
#[test]
fn private_file_modes_and_report_no_clobber_are_enforced() {
    use std::os::unix::fs::PermissionsExt;
    let parent = std::env::temp_dir();
    let root = parent.join(format!("performance-private-test-{}", uuid::Uuid::new_v4()));
    evidence::create_output(&root, None).unwrap();
    assert!(evidence::create_output(&root, None).is_err());
    let input = root.join("token");
    std::fs::write(&input, "unit-only-dummy-token").unwrap();
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(evidence::token(&input).is_err());
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(evidence::token(&input).is_ok());
    let link = root.join("link");
    std::os::unix::fs::symlink(&input, &link).unwrap();
    assert!(evidence::token(&link).is_err());
    evidence::save(&root, "report.json", &plan()).unwrap();
    assert_eq!(
        std::fs::metadata(root.join("report.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_provider_trace_identity_is_not_inferred_from_a_declared_label() {
    let mut c = example().cases.remove(0);
    c.kind = Kind::GatewayReal;
    c.provider_scope = ProviderScope::AuthorizedReal;
    c.upstream_identity = Some(UpstreamIdentity {
        service: "gateway".into(),
        span: "upstream".into(),
        attributes: BTreeMap::from([
            ("server.address".into(), "approved-provider.example".into()),
            ("gen_ai.provider.name".into(), "approved-provider".into()),
            ("gen_ai.request.model".into(), "approved-model".into()),
        ]),
    });
    let mut span = trace::Span {
        service: "gateway".into(),
        name: "upstream".into(),
        start: 1,
        end: 2,
        events: vec![],
        attributes: BTreeMap::new(),
    };
    assert_eq!(
        trace::metrics(&c, &json!({}), &[span.clone()])["real_provider_identity"]["status"],
        "unknown"
    );
    span.attributes = c.upstream_identity.as_ref().unwrap().attributes.clone();
    assert_eq!(
        trace::metrics(&c, &json!({}), &[span])["real_provider_identity"]["status"],
        "verified_from_same_request_service_trace"
    );
}
#[tokio::test]
async fn partial_transport_keeps_received_200_and_body_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut input = [0; 4096];
        let _ = s.read(&mut input).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 200\r\nX-Gateway-Request-Id: partial-test-id\r\nConnection: close\r\n\r\n{\"partial\":").await.unwrap();
    });
    let client = controller::client(1000).unwrap();
    let r = controller::http(
        &client,
        &origin,
        "/fixture",
        reqwest::Method::POST,
        None,
        Some(&json!({})),
        None,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.request_id.as_deref(), Some("partial-test-id"));
    assert!(r.failure.is_some());
    assert!(r.body.is_null());
    assert!(r.bytes > 0);
    server.await.unwrap();
}

#[test]
fn synthetic_upstream_without_same_trace_cannot_complete_a_case() {
    let mut row = json!({"case":"mock","kind":"deterministic_synthetic_upstream","phase":"measured","body_validated":true,"response_trace_correlated":true,"server_metrics":{"status":"measured"},"accounting":{"status":"observed"},"synthetic_upstream":{"status":"observed","same_trace_header":false}});
    assert_eq!(
        controller::summary(&[row.clone()])[0]["status"],
        "PARTIAL_FAILED_OR_UNKNOWN"
    );
    row["synthetic_upstream"]["same_trace_header"] = json!(true);
    assert_eq!(
        controller::summary(&[row])[0]["status"],
        "MEASURED_BASELINE_NO_SLO_EVALUATION"
    );
}
