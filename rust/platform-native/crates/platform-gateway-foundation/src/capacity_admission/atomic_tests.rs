use super::*;
#[test]
fn estimate_is_not_usage_and_rejects_unbounded_bad_numbers() {
    let mut request = UnifiedRequest::new("r", "s", vec![]);
    assert_eq!(estimate(&request), Ok(32768));
    request.parameters =
        Some(serde_json::from_value(serde_json::json!({"max_tokens":-1})).unwrap());
    assert!(estimate(&request).is_err());
    request.parameters =
        Some(serde_json::from_value(serde_json::json!({"max_output_tokens":1})).unwrap());
    assert_eq!(estimate(&request), Ok(1));
}
#[test]
fn refusals_keep_retry_and_status_shape() {
    for refusal_kind in [
        Refusal::Rate(7),
        Refusal::Capacity,
        Refusal::Tokens,
        Refusal::Unavailable,
    ] {
        let DispatchError::Admission(AdmissionError::Rejected(error)) = refusal(refusal_kind)
        else {
            panic!("typed admission refusal")
        };
        assert_eq!(
            error.status_code,
            if refusal_kind == Refusal::Unavailable {
                503
            } else {
                429
            }
        );
        assert!(error.headers.contains_key("Retry-After"));
    }
}

/// Explicit isolated-service gate; never starts infrastructure, writes DDL,
/// reads local env files, or flushes a database. The primary runs this separately.
#[tokio::test]
#[ignore = "requires AI_PLATFORM_ADMISSION_TEST_REDIS_URL for isolated loopback Valkey"]
async fn live_atomic_admission_concurrency_isolation_rejection_and_expiry() {
    use redis::IntoConnectionInfo;
    let configured = std::env::var("AI_PLATFORM_ADMISSION_TEST_REDIS_URL")
        .map_err(|_| ())
        .unwrap();
    let info = configured.into_connection_info().map_err(|_| ()).unwrap();
    assert!(
        matches!(info.addr(), redis::ConnectionAddr::Tcp(host,_) if matches!(host.as_str(),"127.0.0.1"|"localhost"|"::1")),
        "isolated loopback endpoint required"
    );
    let client = redis::Client::open(info).map_err(|_| ()).unwrap();
    let connection = client
        .get_connection_manager()
        .await
        .map_err(|_| ())
        .unwrap();
    let unique = uuid::Uuid::new_v4().to_string();
    let tag = tenant_tag(&unique);
    let mut plan = SlotPlan::default();
    plan.capacities
        .push((format!("test:admission:{tag}:capacity"), 4));
    plan.token_key = Some(format!("test:admission:{tag}:tokens"));
    plan.tokens = Some(crate::quota_store::reservations::TokenEnvelope {
        estimate: 1,
        daily_limit: Some(1000),
        ..Default::default()
    });
    let mut tasks = Vec::new();
    for n in 0..32 {
        let plan = plan.clone();
        let connection = connection.clone();
        tasks.push(tokio::spawn(async move {
            let member = format!("{n}");
            (
                member,
                plan.acquire(&connection, &format!("{n}"), 5000).await,
            )
        }));
    }
    let mut winners = Vec::new();
    for task in tasks {
        let (member, result) = task.await.unwrap();
        match result {
            Ok(_) => winners.push(member),
            Err(error) => assert_eq!(error, Refusal::Capacity),
        }
    }
    assert_eq!(
        winners.len(),
        4,
        "one shared atomic capacity bound across callers"
    );
    let mut foreign = SlotPlan::default();
    foreign.capacities.push((
        format!(
            "test:admission:{}:capacity",
            tenant_tag(&format!("foreign-{unique}"))
        ),
        4,
    ));
    assert!(foreign.acquire(&connection, "foreign", 5000).await.is_ok());
    foreign.release(&connection, "foreign").await.unwrap();
    for member in winners {
        plan.release(&connection, &member).await.unwrap();
    }
    // Token refusal must not charge rate or capacity. This Lua-only case uses
    // a fixed snapshot; concurrent token snapshots are serialized by PG tests.
    let mut refuse = SlotPlan::default();
    refuse
        .capacities
        .push((format!("test:admission:{tag}:reject-capacity"), 2));
    let rate_key = format!("test:admission:{tag}:reject-rate");
    refuse
        .rate(rate_key.clone(), &RateLimit::new(1, 3600))
        .unwrap();
    refuse.token_key = Some(format!("test:admission:{tag}:reject-tokens"));
    refuse.tokens = Some(crate::quota_store::reservations::TokenEnvelope {
        estimate: 11,
        daily_limit: Some(10),
        ..Default::default()
    });
    assert_eq!(
        refuse.acquire(&connection, "bad", 5000).await,
        Err(Refusal::Tokens)
    );
    let cardinality: i64 = redis::cmd("ZCARD")
        .arg(&refuse.capacities[0].0)
        .query_async(&mut connection.clone())
        .await
        .unwrap();
    let tat: Option<String> = redis::cmd("GET")
        .arg(&rate_key)
        .query_async(&mut connection.clone())
        .await
        .unwrap();
    assert_eq!(cardinality, 0);
    assert!(tat.is_none(), "refused token intent must not consume GCRA");
    refuse.tokens.as_mut().unwrap().estimate = 1;
    assert_eq!(refuse.acquire(&connection, "good", 5000).await, Ok(1));
    refuse.release(&connection, "good").await.unwrap();
    assert!(
        matches!(refuse.acquire(&connection,"limited",5000).await,Err(Refusal::Rate(n)) if n>0)
    );
    let mut expiry = SlotPlan::default();
    expiry
        .capacities
        .push((format!("test:admission:{tag}:expiry"), 1));
    assert!(expiry.acquire(&connection, "old", 80).await.is_ok());
    assert_eq!(
        expiry.acquire(&connection, "blocked", 80).await,
        Err(Refusal::Capacity)
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        expiry.renew(&connection, "old", 80).await.is_err(),
        "renew cannot resurrect an expired lease"
    );
    assert!(expiry.acquire(&connection, "new", 80).await.is_ok());
    expiry.release(&connection, "new").await.unwrap();
    // Exact owned keys only; no wildcard cleanup or FLUSHDB.
    let _: i64 = redis::cmd("DEL")
        .arg(&rate_key)
        .query_async(&mut connection.clone())
        .await
        .unwrap();
}
