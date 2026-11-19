use super::*;
use std::{collections::BTreeMap, path::PathBuf};
fn endpoint(host: &str, kind: Instance, service: ServiceIdentity) -> EndpointConfig {
    EndpointConfig::new(
        &format!("rediss://{host}:6380/0"),
        &service.username(kind),
        PathBuf::from(format!("/private/{}.password", kind.name())),
        "/public/ca.pem".into(),
        service,
        kind,
    )
    .unwrap()
}
#[test]
fn tls_username_and_uri_boundary() {
    for url in [
        "redis://coord:6380/0",
        "rediss://shared:secret@coord:6380/0",
        "rediss://coord:6380/1",
        "rediss://coord:6380/0#insecure",
        "rediss://coord:6380/0?insecure=true",
        "rediss://coord/0",
    ] {
        assert!(EndpointConfig::new(
            url,
            "gateway-coord",
            "/private/password".into(),
            "/public/ca.pem".into(),
            ServiceIdentity::Gateway,
            Instance::Coord
        )
        .is_err());
    }
    assert!(EndpointConfig::new(
        "rediss://coord:6380/0",
        "knowledge-api-coord",
        "/private/password".into(),
        "/public/ca.pem".into(),
        ServiceIdentity::Gateway,
        Instance::Coord
    )
    .is_err());
}
#[test]
fn same_instance_and_swapped_domains_rejected() {
    let coord = endpoint("coord", Instance::Coord, ServiceIdentity::Gateway);
    let cache = endpoint("coord", Instance::Cache, ServiceIdentity::Gateway);
    assert!(SplitConfig::new(coord.clone(), cache).is_err());
    assert!(SplitConfig::new(
        endpoint("cache", Instance::Cache, ServiceIdentity::Gateway),
        coord.clone()
    )
    .is_err());
    assert!(SplitConfig::new(
        coord,
        endpoint("cache", Instance::Cache, ServiceIdentity::KnowledgeApi)
    )
    .is_err());
}
#[test]
fn partial_split_never_downgrades_to_legacy() {
    assert!(
        SplitConfig::from_values(&BTreeMap::new(), ServiceIdentity::Gateway)
            .unwrap()
            .is_none()
    );
    for values in [
        BTreeMap::from([("AI_PLATFORM_VALKEY_MODE".into(), "split".into())]),
        BTreeMap::from([(
            "AI_PLATFORM_VALKEY_COORD_ENDPOINT".into(),
            "rediss://coord:6380/0".into(),
        )]),
        BTreeMap::from([("AI_PLATFORM_VALKEY_MODE".into(), "legacy".into())]),
    ] {
        assert!(SplitConfig::from_values(&values, ServiceIdentity::Gateway).is_err());
    }
}
#[test]
fn every_coordination_failure_denies_admission() {
    for err in [
        ValkeyError::Pressure,
        ValkeyError::Authentication,
        ValkeyError::Transport,
        ValkeyError::Timeout,
        ValkeyError::ServerPolicy,
    ] {
        assert_ne!(
            coordination_decision(Err(err)),
            CoordinationDecision::Confirmed
        );
    }
    assert_eq!(
        coordination_decision(Ok(false)),
        CoordinationDecision::Denied
    );
    assert_eq!(
        coordination_decision(Ok(true)),
        CoordinationDecision::Confirmed
    );
}
fn verify(kind: Instance, policy: &str, aof: &str, user: &str) -> Result<(), ValkeyError> {
    verify_server(
        kind,
        "gateway-coord",
        "valkey_version:9.1.2\r\n",
        &format!("maxmemory:268435456\r\nmaxmemory_policy:{policy}\r\n"),
        &format!("aof_enabled:{aof}\r\naof_last_write_status:ok\r\n"),
        &format!("id=1 user={user} age=0"),
    )
}
#[test]
fn server_policy_and_auth_identity_cannot_be_swapped() {
    assert!(verify(Instance::Coord, "noeviction", "1", "gateway-coord").is_ok());
    assert!(verify(Instance::Coord, "allkeys-lru", "1", "gateway-coord").is_err());
    assert!(verify(Instance::Coord, "noeviction", "0", "gateway-coord").is_err());
    assert!(verify(Instance::Coord, "noeviction", "1", "default").is_err());
    assert!(verify(Instance::Cache, "noeviction", "1", "gateway-coord").is_err());
}
#[test]
fn persistence_failure_and_unbounded_memory_refused() {
    assert!(verify_server(
        Instance::Coord,
        "gateway-coord",
        "valkey_version:9.1.2",
        "maxmemory:1\nmaxmemory_policy:noeviction",
        "aof_enabled:1\naof_last_write_status:err",
        "user=gateway-coord"
    )
    .is_err());
    assert!(verify_server(
        Instance::Coord,
        "gateway-coord",
        "valkey_version:9.1.2",
        "maxmemory:0\nmaxmemory_policy:noeviction",
        "aof_enabled:1\naof_last_write_status:ok",
        "user=gateway-coord"
    )
    .is_err());
}
#[test]
fn memory_warning_at_seventy_percent() {
    assert!(!memory_pressure_warning("maxmemory:100\nused_memory:69").unwrap());
    assert!(memory_pressure_warning("maxmemory:100\nused_memory:70").unwrap());
    assert!(memory_pressure_warning("maxmemory:0\nused_memory:1").is_err());
}
#[test]
fn cache_keys_bind_service_tenant_and_kind() {
    let a = CacheKey::new(
        ServiceIdentity::Gateway,
        CacheKind::Principal,
        "tenant-a",
        b"actor/revision",
    )
    .unwrap();
    let b = CacheKey::new(
        ServiceIdentity::Gateway,
        CacheKind::Principal,
        "tenant-b",
        b"actor/revision",
    )
    .unwrap();
    assert_ne!(a.as_str(), b.as_str());
    assert!(a.as_str().starts_with("af-cache:gateway:Principal:{"));
    assert!(CacheKey::new(
        ServiceIdentity::Gateway,
        CacheKind::Retrieval,
        "tenant",
        b"x"
    )
    .is_err());
    assert!(CacheKey::new(
        ServiceIdentity::KnowledgeApi,
        CacheKind::Principal,
        "tenant",
        b"x"
    )
    .is_err());
    let injected = CacheKey::new(
        ServiceIdentity::KnowledgeApi,
        CacheKind::Retrieval,
        "}{other}*",
        b"query",
    )
    .unwrap();
    assert_eq!(injected.as_str().matches('{').count(), 1);
    assert_eq!(injected.as_str().matches('}').count(), 1);
}
fn hashes() -> BTreeMap<String, String> {
    acl::USERS
        .iter()
        .enumerate()
        .map(|(i, u)| (u.to_string(), format!("{i:064x}")))
        .collect()
}
#[test]
fn acl_users_are_distinct_and_domain_scoped() {
    let h = hashes();
    let coord = acl::render(Instance::Coord, &h).unwrap();
    let cache = acl::render(Instance::Cache, &h).unwrap();
    for text in [&coord, &cache] {
        assert!(text.starts_with("user default reset off"));
        assert!(!text.contains("+@all"));
        assert!(!text.contains(" ~*"));
        assert!(!text.contains(" >"));
    }
    assert!(!coord.contains("gateway-cache"));
    assert!(!cache.contains("gateway-coord"));
    let knowledge = coord
        .lines()
        .find(|s| s.starts_with("user knowledge-api-coord "))
        .unwrap();
    assert!(!knowledge.contains("~auth:"));
    assert!(!knowledge.contains("+config"));
    let user = cache
        .lines()
        .find(|s| s.starts_with("user knowledge-api-cache "))
        .unwrap();
    assert!(user.contains("~af-cache:knowledge-api:*"));
    assert!(!user.contains("+eval"));
    assert!(!user.contains("~auth:"));
}
#[test]
fn shared_password_or_unknown_acl_user_refused() {
    let mut h = hashes();
    h.insert("gateway-cache".into(), h["gateway-coord"].clone());
    assert!(acl::render(Instance::Coord, &h).is_err());
    let mut h = hashes();
    h.insert("admin".into(), "a".repeat(64));
    assert!(acl::render(Instance::Coord, &h).is_err());
}
#[test]
fn debug_never_contains_password_or_files() {
    let c = endpoint("coord.private", Instance::Coord, ServiceIdentity::Gateway);
    let text = format!("{c:?}");
    assert!(!text.contains("coord.private"));
    assert!(!text.contains("password"));
    assert!(!text.contains("/private"));
}

#[test]
fn cache_read_is_bounded_before_deserialization() {
    assert!(super::policy::bounded_cache_value(vec![0; 1_048_577]).is_err());
    assert!(super::policy::bounded_cache_value(Vec::new())
        .unwrap()
        .is_none());
    assert_eq!(
        super::policy::bounded_cache_value(b"{}".to_vec()).unwrap(),
        Some(b"{}".to_vec())
    );
}

#[cfg(unix)]
#[test]
fn password_permissions_symlinks_and_newlines_fail_closed() {
    use std::{
        fs,
        io::Write,
        os::unix::{
            fs::OpenOptionsExt,
            fs::{symlink, PermissionsExt},
        },
    };
    let path = std::env::temp_dir().join(format!(
        "valkey-input-contract-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all("a".repeat(64).as_bytes()).unwrap();
    drop(file);
    assert!(super::config::read_password(&path).is_ok());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(super::config::read_password(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let link = path.with_extension("link");
    symlink(&path, &link).unwrap();
    assert!(super::config::read_password(&link).is_err());
    fs::write(&path, format!("{}\n", "a".repeat(64))).unwrap();
    assert!(super::config::read_password(&path).is_err());
    fs::remove_file(link).unwrap();
    fs::remove_file(path).unwrap();
}

#[test]
fn ipv6_literal_is_passed_unbracketed_to_verified_tls() {
    let config = endpoint("[::1]", Instance::Coord, ServiceIdentity::Gateway);
    assert_eq!(config.address(), ("::1", 6380));
}
#[test]
fn existing_coordination_namespace_inventory() {
    for key in [
        "config:auth",
        "metrics:rt:active_requests",
        "metrics:tokens:tenant:t:user:u",
        "events:usage:recorded:v1:dlq",
        "events:idem:gateway-usage-logger:event",
        "agent:channel:{tenant}:publication:minute",
        "lg:quota:user:threads",
        "auth:token:jti",
        "auth:user_tokens:user",
        "guest_session:id",
        "gateway:capacity:{epoch}:global:model",
        "gateway:tenant-capacity:{tenant}:model",
        "gateway:rate:{tenant}:global",
        "gateway:dispatch-token:{tenant}:request",
        "gateway:quota-rpm:{tenant}:user",
        "ratelimit:tenant:t",
        "oauth:state:nonce",
        "ai-gateway:internal:replay:nonce",
        "ai-gateway:responses:idem:lock:scope",
        "queue:retry:queue:normal",
    ] {
        assert!(
            acl::gateway_key_allowed(key),
            "required coord key denied: {key}"
        );
    }
    for forbidden in [
        "af-cache:gateway:Principal:{tenant}:actor",
        "lg:thread:thread",
        "lg:assistant:assistant",
        "operator:password",
    ] {
        assert!(
            !acl::gateway_key_allowed(forbidden),
            "pure cache/unknown namespace leaked into coord: {forbidden}"
        );
    }
}

#[test]
fn live_coordination_command_inventory_is_granted() {
    let text = acl::render(Instance::Coord, &hashes()).unwrap();
    let gateway = text
        .lines()
        .find(|s| s.starts_with("user gateway-coord "))
        .unwrap();
    for command in [
        "+time",
        "+scan",
        "+zpopmin",
        "+zremrangebyrank",
        "+xclaim",
        "+xgroup|delconsumer",
        "+xinfo|groups",
        "+multi",
        "+exec",
    ] {
        assert!(
            gateway.split_whitespace().any(|c| c == command),
            "required production command denied: {command}"
        );
    }
    for forbidden in ["+keys", "+config", "+acl", "+flushdb", "+flushall"] {
        assert!(!gateway.split_whitespace().any(|c| c == forbidden));
    }
}
