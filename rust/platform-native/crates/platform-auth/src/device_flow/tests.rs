use super::*;
#[test]
fn rfc7636_s256_vector_and_verifier_boundaries() {
    assert_eq!(
        pkce("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap(),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert!(challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
    for value in [
        "a".repeat(42),
        "a".repeat(129),
        format!("{}!", "a".repeat(42)),
        "é".repeat(43),
    ] {
        assert_eq!(pkce(&value), Err(DeviceError::InvalidGrant));
    }
    assert!(!challenge(&"a".repeat(43))); // noncanonical trailing bits
    assert!(!challenge(&"a".repeat(44)));
    assert!(pkce(&"~".repeat(128)).is_ok());
}
#[test]
fn client_cannot_supply_identity_or_ambient_browser_credentials() {
    for raw in [
        r#"{"client_id":"agentforge-cli","code_challenge":"x","code_challenge_method":"S256","user_id":"victim"}"#,
        r#"{"client_id":"agentforge-cli","code_challenge":"x","code_challenge_method":"S256","tenant_id":"other"}"#,
    ] {
        assert!(serde_json::from_str::<AuthorizeInput>(raw).is_err());
    }
    assert!(
        serde_json::from_value::<DecisionInput>(
            json!({"user_code":"ABCDE-12345","approve":true,"tenant_id":"other"})
        )
        .is_err()
    );
    assert!(native(&HeaderMap::new()).is_ok());
    for header in ["cookie", "origin", "sec-fetch-site", "authorization"] {
        let mut h = HeaderMap::new();
        h.insert(header, "anything".parse().unwrap());
        assert_eq!(native(&h), Err(DeviceError::Invalid));
    }
    assert!(client(CLIENT_ID).is_ok());
    assert_eq!(client("attacker"), Err(DeviceError::InvalidClient));
    assert!(scope("").is_ok());
    assert!(scope("admin").is_err());
}
#[test]
fn code_and_refresh_validation_bound_untrusted_inputs() {
    assert_eq!(user_code("abcde-12345").unwrap(), "ABCDE12345");
    for raw in ["", "ABCDE/12345", "ABCDE-123456", "ABCDE-1234X"] {
        assert!(user_code(raw).is_err());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let secret = opaque();
    let refresh = format!("adfr.{id}.{secret}");
    let (got, hash) = refresh_parts(&refresh).unwrap();
    assert_eq!(got, id);
    assert_eq!(hash, digest(&refresh));
    assert_ne!(hash, secret);
    for raw in [
        format!("adfr.not-a-uuid.{secret}"),
        format!("adfr.{id}.short"),
        format!("{refresh}.extra"),
        format!("{refresh}\n"),
    ] {
        assert!(refresh_parts(&raw).is_err());
    }
    assert!(device_code(&secret).is_ok());
    assert!(device_code(&secret.to_ascii_uppercase()).is_err());
}
#[test]
fn binding_uses_original_session_identity_and_expiry() {
    let binding = binding();
    let claims = binding.claims();
    assert_eq!(claims.sub, binding.user_id);
    assert_eq!(claims.tenant_id, binding.tenant_id);
    assert_eq!(claims.jti, binding.parent_jti);
    assert_eq!(claims.exp, binding.expires_at);
    assert_eq!(claims.iat, binding.issued_at);
    assert!(claims.permissions.is_empty());
    let refresh = format!("adfr.{}.{}", uuid::Uuid::new_v4(), opaque());
    let grant = RefreshGrant {
        id: uuid::Uuid::new_v4().to_string(),
        jti: uuid::Uuid::new_v4().to_string(),
        client_id: CLIENT_ID.into(),
        scope: "platform".into(),
        refresh_hash: digest(&refresh),
        revocation_hashes: vec![digest(&refresh)],
        binding,
    };
    let stored = serde_json::to_string(&grant).unwrap();
    assert!(!stored.contains(&refresh));
}
#[tokio::test]
async fn browser_shell_needs_review_and_click_and_has_no_embedded_identity() {
    let response = page().await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    let html = include_str!("approval.html");
    assert!(html.contains("addEventListener('click'"));
    assert!(!html.contains("csrf_token="));
    assert!(!html.contains("access_token"));
    assert!(html.contains("textContent="));
    let script = html
        .split_once("<script>")
        .unwrap()
        .1
        .split_once("</script>")
        .unwrap()
        .0;
    let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(script.as_bytes()));
    let standard =
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script.as_bytes()));
    assert!(!hash.is_empty());
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains(&format!("sha256-{standard}"))
    );
}
fn binding() -> Binding {
    Binding {
        parent_jti: uuid::Uuid::new_v4().to_string(),
        user_id: format!("device-test-{}", uuid::Uuid::new_v4()),
        tenant_id: "test-tenant".into(),
        issued_at: 100,
        expires_at: 10900,
        credential_revision: Value::Null,
        auth_epoch: Some(json!(7)),
    }
}
fn flow() -> Flow {
    Flow {
        challenge: pkce(&"a".repeat(43)).unwrap(),
        client_id: CLIENT_ID.into(),
        scope: "platform".into(),
        user_code_hash: digest("ABCDE12345"),
        expires_at: 700,
        last_poll: 0,
        interval: 5,
        status: "pending".into(),
        binding: None,
    }
}
/// Uses only random scoped auth keys in an explicitly supplied disposable Redis.
/// Root owns the serialized Docker run; this test never starts a service.
#[tokio::test]
#[ignore = "requires AGENTFORGE_DEVICE_TEST_REDIS_URL pointing at disposable Redis"]
async fn redis_atomic_device_lifecycle_and_revocation_races() {
    let url = std::env::var("AGENTFORGE_DEVICE_TEST_REDIS_URL")
        .expect("explicit disposable Redis URL required");
    let client = redis::Client::open(url).unwrap();
    let connection = redis::aio::ConnectionManager::new(client).await.unwrap();
    let sessions = crate::sessions::RedisSessions::new(connection);
    let store = sessions.device_store();
    let code = opaque();
    let user = opaque()[..10].to_ascii_uppercase();
    let mut f = flow();
    f.user_code_hash = digest(&user);
    store.create(&code, &user, &f).await.unwrap();
    assert_eq!(
        store.poll(&code, &"b".repeat(43), 100).await.err(),
        Some(DeviceError::InvalidGrant)
    );
    assert_eq!(
        store.poll(&code, &f.challenge, 100).await.err(),
        Some(DeviceError::Pending)
    );
    assert_eq!(
        store.poll(&code, &f.challenge, 101).await.err(),
        Some(DeviceError::SlowDown)
    );
    assert_eq!(
        store.poll(&code, &f.challenge, 109).await.err(),
        Some(DeviceError::SlowDown)
    );
    let (id, old, mut f) = store.by_user(&user).await.unwrap();
    assert_eq!(f.interval, 15);
    f.binding = Some(binding());
    f.status = "approved".into();
    store.decide(&id, &old, &f, 110).await.unwrap();
    assert_eq!(
        store.decide(&id, &old, &f, 110).await.err(),
        Some(DeviceError::InvalidGrant)
    );
    let (old, f) = store.poll(&code, &f.challenge, 124).await.unwrap();
    let binding = f.binding.clone().unwrap();
    let parent =
        json!({"user_id":binding.user_id,"credential_revision":Value::Null,"auth_epoch":7});
    sessions
        .save(&binding.parent_jti, &binding.user_id, &parent, 1000)
        .await
        .unwrap();
    let refresh = format!("adfr.{}.{}", uuid::Uuid::new_v4(), opaque());
    let hash = digest(&refresh);
    let mut grant = RefreshGrant {
        id: uuid::Uuid::new_v4().to_string(),
        jti: uuid::Uuid::new_v4().to_string(),
        client_id: CLIENT_ID.into(),
        scope: "platform".into(),
        refresh_hash: hash.clone(),
        revocation_hashes: vec![hash.clone()],
        binding: binding.clone(),
    };
    let record =
        json!({"user_id":binding.user_id,"credential_revision":Value::Null,"auth_epoch":7});
    store
        .complete(&code, &old, &f, &grant, &record, &parent, 124)
        .await
        .unwrap();
    assert!(sessions.contains(&grant.jti).await.unwrap());
    assert_eq!(
        store
            .complete(&code, &old, &f, &grant, &record, &parent, 124)
            .await
            .err(),
        Some(DeviceError::InvalidGrant)
    );
    assert!(store.by_user(&user).await.is_err());
    let (old, _) = store.grant(&grant.id).await.unwrap();
    grant.refresh_hash = digest("replacement");
    grant.revocation_hashes.push(grant.refresh_hash.clone());
    store.rotate(&old, &grant, &parent, 125).await.unwrap();
    assert_eq!(
        store.rotate(&old, &grant, &parent, 125).await.err(),
        Some(DeviceError::InvalidGrant)
    );
    store.revoke(&grant.id, &digest("unknown")).await.unwrap();
    assert!(sessions.contains(&grant.jti).await.unwrap());
    let (old, _) = store.grant(&grant.id).await.unwrap();
    store.revoke(&grant.id, &hash).await.unwrap();
    assert!(!sessions.contains(&grant.jti).await.unwrap());
    assert!(store.grant(&grant.id).await.is_err());
    assert_eq!(
        store.rotate(&old, &grant, &parent, 126).await.err(),
        Some(DeviceError::InvalidGrant)
    );
    store.revoke(&grant.id, &hash).await.unwrap();
    sessions
        .revoke(&binding.parent_jti, &binding.user_id)
        .await
        .unwrap();
    // Denial and expiration remain terminal and cannot be approved later.
    for (status, time, expected) in [
        ("denied", 100, DeviceError::Denied),
        ("pending", 700, DeviceError::Expired),
    ] {
        let code = opaque();
        let user = opaque()[..10].to_ascii_uppercase();
        let mut f = flow();
        f.user_code_hash = digest(&user);
        store.create(&code, &user, &f).await.unwrap();
        let (id, old, _) = store.by_user(&user).await.unwrap();
        f.status = status.into();
        if status == "denied" {
            store.decide(&id, &old, &f, 99).await.unwrap();
        }
        assert_eq!(
            store.poll(&code, &f.challenge, time).await.err(),
            Some(expected)
        );
        let _: i64 = store
            .eval(
                "return redis.call('DEL',KEYS[1],KEYS[2])",
                &[
                    format!("auth:device:flow:{}", digest(&code)),
                    format!("auth:device:user:{}", digest(&user)),
                ],
                &[],
            )
            .await
            .unwrap();
    }
}
