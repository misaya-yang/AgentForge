//! Explicit later acceptance only. These tests fill the selected coordination
//! instance; run during a planned outage after checking project/resource ownership.
use super::*;
use std::collections::BTreeMap;
fn selected() -> SplitConfig {
    assert_eq!(
        std::env::var("TEST_VALKEY_PRESSURE_ACK").as_deref(),
        Ok("planned-outage"),
        "explicit planned-outage acknowledgement required"
    );
    let values: BTreeMap<_, _> = std::env::vars()
        .filter(|(key, _)| key.starts_with("AI_PLATFORM_VALKEY_"))
        .collect();
    SplitConfig::from_values(&values, ServiceIdentity::Gateway)
        .expect("invalid acceptance configuration")
        .expect("explicit split transport required")
}
#[tokio::test]
#[ignore = "real memory pressure; explicit planned-outage config and private test inputs required"]
async fn real_pressure_never_evicts_coordination_and_cache_cannot_admit() {
    let config = selected();
    let connections = SplitConnections::connect(&config)
        .await
        .expect("real TLS/ACL startup required");
    let CacheAvailability::Ready(cache) = connections.cache else {
        panic!("cache pressure acceptance requires the actual cache instance")
    };
    let id = uuid::Uuid::new_v4().to_string();
    let session = format!("auth:token:acceptance-{id}");
    let lease = format!("gateway:capacity:{{acceptance-{id}}}:lease");
    for key in [&session, &lease] {
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg("must-survive").arg("EX").arg(600);
        connections
            .coord
            .execute::<()>(&cmd)
            .await
            .expect("marker creation required");
    }
    let mut pressure = false;
    let mut written = Vec::new();
    let payload = vec![b'x'; 16_384];
    for i in 0..65536 {
        let key = format!("gateway:acceptance:{{{id}}}:{i}");
        let mut cmd = redis::cmd("SET");
        cmd.arg(&key).arg(&payload).arg("EX").arg(120);
        match connections.coord.execute::<()>(&cmd).await {
            Ok(()) => written.push(key),
            Err(ValkeyError::Pressure) => {
                pressure = true;
                break;
            }
            Err(e) => panic!("unexpected coordination failure: {e}"),
        }
    }
    assert!(pressure, "bounded fill must reach real OOM, not eviction");
    assert!(matches!(
        coordination_decision(Err(ValkeyError::Pressure)),
        CoordinationDecision::Unavailable(_)
    ));
    for key in [&session, &lease] {
        let mut cmd = redis::cmd("GET");
        cmd.arg(key);
        assert_eq!(
            connections.coord.execute::<String>(&cmd).await.unwrap(),
            "must-survive"
        );
    }
    let first = CacheKey::new(
        ServiceIdentity::Gateway,
        CacheKind::Principal,
        &id,
        b"first/revision/acl",
    )
    .unwrap();
    cache
        .set(&first, b"principal-test-only", 120)
        .await
        .unwrap();
    for i in 0..16384 {
        let key = CacheKey::new(
            ServiceIdentity::Gateway,
            CacheKind::Policy,
            &id,
            format!("policy/{i}").as_bytes(),
        )
        .unwrap();
        cache
            .set(&key, &payload, 120)
            .await
            .expect("cache must evict instead of coordination pressure");
    }
    assert!(
        cache.get(&first).await.unwrap().is_none(),
        "disposable cache entry must demonstrate actual eviction"
    );
    // Deleting only this test's exact recorded keys restores headroom; no FLUSH.
    written.extend([session, lease]);
    for group in written.chunks(128) {
        let mut cmd = redis::cmd("DEL");
        cmd.arg(group);
        connections.coord.execute::<u64>(&cmd).await.unwrap();
    }
}
#[tokio::test]
#[ignore = "real TLS negative/ACL tests; explicit selected purpose credentials required"]
async fn real_wrong_ca_credentials_and_cross_service_acl_are_refused() {
    let config = selected();
    let mut wrong_password = config.coord().clone();
    wrong_password.password_file = config.cache().password_file.clone();
    assert!(matches!(
        super::transport::connect(&wrong_password).await,
        Err(ValkeyError::Authentication)
    ));
    let mut wrong_ca = config.coord().clone();
    wrong_ca.ca_file = std::env::var("TEST_VALKEY_WRONG_CA_FILE")
        .expect("independent untrusted CA file required")
        .into();
    assert!(super::transport::connect(&wrong_ca).await.is_err());
    let wrong_name = EndpointConfig::new(
        &std::env::var("TEST_VALKEY_WRONG_DNS_ENDPOINT")
            .expect("same server reachable through a non-SAN DNS alias required"),
        "gateway-coord",
        config.coord().password_file.clone(),
        config.coord().ca_file.clone(),
        ServiceIdentity::Gateway,
        Instance::Coord,
    )
    .unwrap();
    assert!(super::transport::connect(&wrong_name).await.is_err());
    let host = if config.coord().host.contains(':') {
        format!("[{}]", config.coord().host)
    } else {
        config.coord().host.clone()
    };
    let knowledge = EndpointConfig::new(
        &format!("rediss://{}:{}/0", host, config.coord().port),
        "knowledge-api-coord",
        std::env::var("TEST_VALKEY_KNOWLEDGE_COORD_PASSWORD_FILE")
            .expect("Knowledge purpose password file required")
            .into(),
        config.coord().ca_file.clone(),
        ServiceIdentity::KnowledgeApi,
        Instance::Coord,
    )
    .unwrap();
    let mut connection = super::transport::connect(&knowledge).await.unwrap();
    let err = redis::cmd("GET")
        .arg("auth:token:acceptance-inaccessible")
        .query_async::<Option<String>>(&mut connection)
        .await
        .expect_err("Knowledge must not read Gateway session authority");
    assert_eq!(err.code(), Some("NOPERM"));
}
