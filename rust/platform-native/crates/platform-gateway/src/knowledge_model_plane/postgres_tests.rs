//! Explicit opt-in authority tests. Never connect or migrate during unit tests.
use super::*;
#[tokio::test]
#[ignore = "requires authority-admitted proposal and dedicated disposable PostgreSQL test database"]
async fn concurrent_reservation_replay_conflict_and_dispatch_fence() {
    let dsn = std::env::var("AGENTFORGE_KNOWLEDGE_MODEL_TEST_DSN")
        .expect("explicit dedicated test DSN required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&dsn)
        .await
        .expect("test database unavailable");
    let tenant = format!("kn-model-test-{}", uuid::Uuid::new_v4());
    let user = "fixture";
    let call = "one";
    sqlx::query("INSERT INTO gateway.user_quotas(tenant_id,user_id,daily_token_limit,monthly_token_limit,monthly_cost_limit_cents,requests_per_day) VALUES($1,$2,12,12,1,1)").bind(&tenant).bind(user).execute(&pool).await.unwrap();
    let lease_id = uuid::Uuid::new_v4().to_string();
    let hash = "a".repeat(64);
    let token = json!({"lease_id":lease_id,"tenant_id":tenant,"user_id":user,"call_id":call,"request_sha256":hash,"expires_at_unix":now_ms()/1000+60});
    let body = json!({"scope":{"actor":{"tenant_id":tenant,"user_id":user}}});
    let reserve = |hash: String, body: Value| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        let token = token.clone();
        let lease_id = lease_id.clone();
        async move {
            sqlx::query_scalar::<_,Value>("SELECT gateway.reserve_knowledge_model_call($1,$2,$3,$4,$5::uuid,$6,$7,'{}','{}',10,2,0)").bind(tenant).bind(user).bind(call).bind(hash).bind(lease_id).bind(token).bind(body).fetch_one(&pool).await
        }
    };
    let mut tasks = Vec::new();
    for _ in 0..12 {
        tasks.push(tokio::spawn(reserve(hash.clone(), body.clone())))
    }
    for task in tasks {
        let row = task.await.unwrap().unwrap();
        assert_eq!(row["lease_id"], lease_id)
    }
    let usage:(i64,i32)=sqlx::query_as("SELECT current_daily_tokens,current_daily_requests FROM gateway.user_quotas WHERE tenant_id=$1 AND user_id=$2").bind(&tenant).bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(usage, (12, 1));
    // The quota is full, but identical replay must still succeed.
    assert!(reserve(hash.clone(), body.clone()).await.is_ok());
    let mut conflict = body.clone();
    conflict["changed"] = json!(true);
    assert!(reserve(hash.clone(), conflict).await.is_err());
    let mut claims = Vec::new();
    for _ in 0..12 {
        let pool = pool.clone();
        let id = lease_id.clone();
        claims.push(tokio::spawn(async move{sqlx::query("UPDATE gateway.knowledge_model_calls SET state='dispatched',dispatched_at=NOW() WHERE lease_id=$1::uuid AND state='reserved' AND expires_at>NOW()").bind(id).execute(&pool).await.unwrap().rows_affected()}))
    }
    let mut winners = 0;
    for claim in claims {
        winners += claim.await.unwrap()
    }
    assert_eq!(winners, 1);
    sqlx::query("UPDATE gateway.knowledge_model_calls SET state='unknown' WHERE lease_id=$1::uuid")
        .bind(&lease_id)
        .execute(&pool)
        .await
        .unwrap();
    let redispatch=sqlx::query("UPDATE gateway.knowledge_model_calls SET state='dispatched' WHERE lease_id=$1::uuid AND state='reserved'").bind(&lease_id).execute(&pool).await.unwrap().rows_affected();
    assert_eq!(redispatch, 0);
    // Cleanup only rows carrying this newly generated test tenant.
    sqlx::query("DELETE FROM gateway.knowledge_model_calls WHERE tenant_id=$1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM gateway.user_quotas WHERE tenant_id=$1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}
