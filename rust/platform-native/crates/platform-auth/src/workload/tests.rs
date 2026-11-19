use super::*;
fn rules() -> PeerRules {
    PeerRules::new([
        ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime").unwrap(),
    ])
    .unwrap()
}
#[test]
fn required_never_falls_back_to_legacy_and_identity_headers_never_authenticate() {
    let required = WorkloadAuth::new(Mode::Required, None, rules()).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", "a".repeat(48)).parse().unwrap(),
    );
    assert_eq!(
        required.authenticate(&headers, None).err(),
        Some(Error::Credentials)
    );
    for name in [
        "x-spiffe-id",
        "x-workload-identity",
        "x-forwarded-client-cert",
    ] {
        headers.insert(
            name,
            "spiffe://agentforge.local/ns/platform/sa/runtime"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            required.authenticate(&headers, None).err(),
            Some(Error::IdentityHints)
        );
        headers.remove(name);
    }
}
#[test]
fn required_rejects_retained_secret_configuration_and_partial_modes() {
    let mut env = BTreeMap::from([
        ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
        ("AI_PLATFORM_INTERNAL_TOKEN_FILE".into(), "/not/read".into()),
    ]);
    assert!(WorkloadAuth::from_environment(&env, rules()).is_err());
    env.remove("AI_PLATFORM_INTERNAL_TOKEN_FILE");
    assert!(WorkloadAuth::from_environment(&env, rules()).is_ok());
    for mode in ["true", "require", "dual-accept", ""] {
        env.insert("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), mode.into());
        assert!(WorkloadAuth::from_environment(&env, rules()).is_err());
    }
}
#[cfg(unix)]
#[test]
fn dual_window_bearer_is_explicit_and_duplicate_headers_fail() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("workload-auth-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("token");
    std::fs::write(
        &file,
        "synthetic-unit-credential-abcdefghijklmnopqrstuvwxyz\n",
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let env = BTreeMap::from([
        ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "dual".into()),
        (
            "AI_PLATFORM_INTERNAL_TOKEN_FILE".into(),
            file.to_str().unwrap().into(),
        ),
    ]);
    let gate = WorkloadAuth::from_environment(&env, rules()).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        "Bearer synthetic-unit-credential-abcdefghijklmnopqrstuvwxyz"
            .parse()
            .unwrap(),
    );
    assert_eq!(
        gate.authenticate(&headers, None).unwrap().kind(),
        CredentialKind::LegacyBearer
    );
    headers.append("authorization", "Bearer duplicate".parse().unwrap());
    assert!(gate.authenticate(&headers, None).is_err());
    headers.remove("authorization");
    headers.insert("authorization", "Bearer incorrect".parse().unwrap());
    assert!(gate.authenticate(&headers, None).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn exact_route_peer_rules_only_narrow_the_existing_tls_allowlist() {
    let runtime =
        ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime").unwrap();
    let knowledge =
        ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/knowledge").unwrap();
    let path = "/internal/v1/authorization/dataset";
    let gate = WorkloadAuth::new(
        Mode::Required,
        None,
        PeerRules::new([runtime.clone(), knowledge.clone()]).unwrap(),
    )
    .unwrap()
    .with_route_peers(
        [
            ("POST".into(), path.into()),
            ("POST".into(), format!("{path}/batch")),
        ],
        [knowledge.clone()],
    )
    .unwrap();
    for target in [path.to_string(), format!("{path}/batch")] {
        assert!(gate.route_admits("POST", &target, Some(&knowledge)));
        assert!(!gate.route_admits("POST", &target, Some(&runtime)));
    }
    assert!(gate.route_admits("POST", "/internal/v1/existing", Some(&runtime)));
    assert!(
        gate.authenticate_route("POST", path, &HeaderMap::new(), None)
            .is_err()
    );
    let foreign =
        ServiceIdentity::parse("spiffe://another.local/ns/platform/sa/knowledge").unwrap();
    assert!(!gate.route_admits("POST", path, Some(&foreign)));
    assert!(
        WorkloadAuth::new(Mode::Required, None, rules())
            .unwrap()
            .with_route_peers([("POST".into(), path.into())], [knowledge])
            .is_err()
    );
}
#[test]
fn absent_route_peer_mapping_denies_tls_while_dual_keeps_explicit_signed_legacy_boundary() {
    let runtime =
        ServiceIdentity::parse("spiffe://agentforge.local/ns/platform/sa/runtime").unwrap();
    let path = "/internal/v1/authorization/dataset";
    let gate = WorkloadAuth::new(
        Mode::Dual,
        Some(SecretBytes::from_configuration(vec![b'a'; 48])),
        rules(),
    )
    .unwrap()
    .with_route_peers([("POST".into(), path.into())], [])
    .unwrap();
    assert!(!gate.route_admits("POST", path, Some(&runtime)));
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", "a".repeat(48)).parse().unwrap(),
    );
    assert_eq!(
        gate.authenticate_route("POST", path, &headers, None)
            .unwrap()
            .kind(),
        CredentialKind::LegacyBearer
    );
    for target in [
        "/api/v1/public",
        "/internal/%64ataset",
        "/internal/../admin",
        "/internal//dataset",
        "/internal/dataset?tenant=x",
    ] {
        assert!(
            WorkloadAuth::new(Mode::Required, None, rules())
                .unwrap()
                .with_route_peers([("POST".into(), target.into())], [])
                .is_err()
        );
    }
}
