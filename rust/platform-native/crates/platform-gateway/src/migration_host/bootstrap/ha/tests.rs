use super::*;
fn environment(config: serde_json::Value) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("AI_PLATFORM_GATEWAY_HA".into(), config.to_string()),
        ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
    ])
}
fn valid() -> serde_json::Value {
    serde_json::json!({"enabled":true,"instance_id":"gateway-api-0","origin":"https://10.42.0.7:9444"})
}
#[test]
fn opt_in_is_explicit_and_workload_required() {
    assert!(
        Configuration::from_environment(&BTreeMap::new())
            .unwrap()
            .is_none()
    );
    assert!(
        Configuration::from_environment(&environment(serde_json::json!({"enabled":false})))
            .unwrap()
            .is_none()
    );
    let c = Configuration::from_environment(&environment(valid()))
        .unwrap()
        .unwrap();
    assert_eq!(c.lease_seconds, 60);
    for mode in ["disabled", "dual", ""] {
        let mut e = environment(valid());
        e.insert("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), mode.into());
        assert!(Configuration::from_environment(&e).is_err());
    }
}
#[test]
fn no_ambiguous_owner_or_unbounded_socket_configuration() {
    for (key, value) in [
        ("instance_id", serde_json::json!("bad/id")),
        ("origin", serde_json::json!("http://10.42.0.7:9444")),
        (
            "origin",
            serde_json::json!("https://user:pw@10.42.0.7:9444"),
        ),
        (
            "origin",
            serde_json::json!("https://10.42.0.7:9444/callback"),
        ),
        ("origin", serde_json::json!("https://10.42.0.7:9444/")),
        ("lease_seconds", serde_json::json!(14)),
        ("lease_seconds", serde_json::json!(301)),
        ("dashboard_global", serde_json::json!(1000001)),
        ("dashboard_tenant", serde_json::json!(0)),
        ("dashboard_tenant", serde_json::json!(513)),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut c = valid();
        c[key] = value;
        assert!(
            Configuration::from_environment(&environment(c)).is_err(),
            "accepted {key}"
        );
    }
}
#[test]
fn directory_origin_must_resolve_to_exact_private_listener() {
    let bound = "10.42.0.7:9444".parse().unwrap();
    assert!(validate_binding(&std::collections::BTreeSet::from([bound]), bound).is_ok());
    for peers in [
        vec![],
        vec!["10.42.0.8:9444".parse().unwrap()],
        vec![bound, "10.42.0.8:9444".parse().unwrap()],
        vec!["10.42.0.7:443".parse().unwrap()],
    ] {
        assert!(validate_binding(&peers.into_iter().collect(), bound).is_err());
    }
}
#[test]
fn callback_inventory_covers_current_owner_handlers_only() {
    let paths = callback_paths().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(paths.len(), 7);
    for path in paths {
        assert!(include_str!("../../sdk_callback_tls.rs").contains(&format!("\"{path}\"")));
    }
    assert!(worker_routes().is_ok());
}
#[test]
fn actual_process_constructs_validated_ha_before_consuming_graph() {
    let process = include_str!("../process.rs");
    let graph = include_str!("../graph.rs");
    let workers = include_str!("../workers.rs");
    assert!(process.contains("GatewayGraph::from_model_services_with_ha("));
    assert!(process.contains("resources.coordination.as_ref()"));
    assert!(process.contains("ha.routing().protect(router)"));
    assert!(process.contains("ha_lease: ha.as_ref().map(|h| h.health())"));
    assert!(graph.contains("ha.verify(&graph)?"));
    assert!(graph.contains("ha.configure_grants(&core.registry)?"));
    assert!(graph.contains(".verify_ready()"));
    assert!(workers.contains("ha.supervise_workers(&stops)"));
    assert!(workers.contains("starts.insert(0, registration)"));
    let stream = include_str!("../../../migration_agent/model_guarded_stream.rs");
    let producer = stream
        .find("metrics.observe_prepared(&call, &prepared.body)")
        .unwrap();
    assert!(stream.find("inspect_input(").unwrap() < producer);
    assert!(producer < stream.find("status='dispatched'").unwrap());
    assert!(producer < stream.find(".post(prepared.url)").unwrap());
    let sdk = include_str!("../../knowledge_proxy/sdk_models.rs");
    let producer = sdk.find("metrics\n            .observe(").unwrap();
    assert!(sdk.find(".inspect_input(").unwrap() < producer);
    assert!(producer < sdk.find("SET state='dispatched'").unwrap());
    assert!(producer < sdk.find(".post(p.url)").unwrap());
}
