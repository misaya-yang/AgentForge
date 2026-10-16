//! Native equivalent of test_gateway_boot.py. Cargo links every production router;
//! the full create/serve assertion uses the actual gateway executable and explicitly
//! requires a disposable, already provisioned PostgreSQL/Redis configuration.
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn gateway_boot_binary_resolves_and_refuses_malformed_configuration() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gateway"))
        .arg("--check-config-stdin")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("gateway_document"));
}
#[test]
#[ignore = "Requires GATEWAY_BOOT_DISPOSABLE=1 and GATEWAY_BOOT_CONFIG_PATH for isolated provisioned PostgreSQL/Redis; never run by ordinary CI"]
fn gateway_boot_constructs_all_production_routes_in_native_subprocess() {
    assert_eq!(
        std::env::var("GATEWAY_BOOT_DISPOSABLE").as_deref(),
        Ok("1"),
        "only an explicitly disposable fixture may start this process"
    );
    let path = std::env::var_os("GATEWAY_BOOT_CONFIG_PATH")
        .expect("explicit disposable config path required");
    let mut document: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    // No inherited credential discovery or bootstrap-account creation in this fixture.
    document["environment"]
        .as_object_mut()
        .expect("explicit environment object")
        .remove("DEFAULT_USER_PASSWORD");
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    document["settings"]["host"] = json!("127.0.0.1");
    document["settings"]["port"] = json!(port);
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_gateway"))
            .arg("--config-stdin")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let origin = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "native gateway exited before constructing router"
            );
            if let Ok(response) = client.get(format!("{origin}/health/live")).send().await {
                if response.status().is_success() {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "native gateway never published liveness"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // A real router must publish readiness and mount the protected management API.
        let response = client
            .get(format!("{origin}/health/ready"))
            .send()
            .await
            .unwrap();
        assert_ne!(response.status(), reqwest::StatusCode::NOT_FOUND);
        let response = client
            .get(format!("{origin}/api/v1/providers"))
            .send()
            .await
            .unwrap();
        assert_ne!(response.status(), reqwest::StatusCode::NOT_FOUND);
    });
}
#[tokio::test]
async fn readiness_probes_runtime_and_knowledge_ready_endpoints_with_service_request_ids() {
    use ai_platform_gateway::migration_host::readiness::probe_http_service;
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use std::sync::{Arc, Mutex};
    let observed = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let mut app = Router::new();
    for name in ["agent_runtime", "knowledge_service"] {
        let observed = observed.clone();
        app = app.route(
            &format!("/{name}/health/ready"),
            get(move |headers: HeaderMap| {
                let observed = observed.clone();
                async move {
                    observed.lock().unwrap().push((
                        name.to_owned(),
                        headers
                            .get("x-request-id")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    ));
                    Json(json!({"status":"ready"}))
                }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut checks = std::collections::BTreeMap::new();
    for name in ["agent_runtime", "knowledge_service"] {
        checks.insert(
            name,
            probe_http_service(
                &client,
                Some(&format!("http://{address}/{name}")),
                "/health/ready",
                Some("ready"),
                false,
            )
            .await,
        );
    }
    server.abort();
    assert_eq!(
        checks,
        std::collections::BTreeMap::from([
            ("agent_runtime", "healthy".into()),
            ("knowledge_service", "healthy".into())
        ])
    );
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0, "agent_runtime");
    assert_eq!(observed[1].0, "knowledge_service");
    assert!(observed.iter().all(|(_, id)| id.starts_with("svc-")));
}
