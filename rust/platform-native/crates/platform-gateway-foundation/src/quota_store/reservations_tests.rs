//! Explicit isolated-DB lifecycle gate. Uses canonical installed schema only;
//! never creates tables/checkpoints, bypasses roles, or invents usage receipts.
use super::*;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore = "requires isolated canonical proposal DB and scoped Gateway-role DSN"]
async fn live_durable_token_intents_serialize_tenants_and_never_expire_dispatched_work() {
    let configured = std::env::var("AI_PLATFORM_ADMISSION_TEST_DATABASE_URL")
        .map_err(|_| ())
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&configured)
        .await
        .map_err(|_| ())
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&pool)
        .await
        .map_err(|_| ())
        .unwrap();
    assert!(
        database.starts_with("schema_successor_verify_")
            || database.starts_with("agentforge_test_"),
        "isolated database required"
    );
    let (user,session,superuser,createdb,createrole,replication,bypassrls,inherit):(String,String,bool,bool,bool,bool,bool,bool)=
        sqlx::query_as("SELECT current_user::text,session_user::text,rolsuper,rolcreatedb,rolcreaterole,rolreplication,rolbypassrls,rolinherit FROM pg_roles WHERE rolname=current_user")
        .fetch_one(&pool).await.map_err(|_|()).unwrap();
    assert_eq!(user, session);
    assert!(!superuser && !createdb && !createrole && !replication && !bypassrls && !inherit);
    let owner: String = sqlx::query_scalar(
        "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname='gateway'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|_| ())
    .unwrap();
    let expected = format!(
        "{}gateway",
        owner.strip_suffix("owner").expect("canonical owner")
    );
    assert_eq!(user, expected, "exact canonical Gateway scope required");
    let unique = uuid::Uuid::new_v4().to_string();
    let tenant = format!("gw07-{unique}");
    let subject = "same-subject";
    set_quota(&pool, &tenant, subject, 4).await;
    let store = ReservationStore::new(pool.clone());
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let store = store.clone();
        let tenant = tenant.clone();
        tasks.push(tokio::spawn(async move {
            let id = identity(&tenant, subject);
            match store.prepare(&id, 1, 10_000).await {
                Ok(prepared) => {
                    prepared.commit().await.unwrap();
                    Ok(id)
                }
                Err(error) => Err(error),
            }
        }));
    }
    let mut winners = Vec::new();
    for task in tasks {
        match task.await.unwrap() {
            Ok(id) => winners.push(id),
            Err(error) => assert_eq!(error, ReservationError::QuotaExceeded),
        }
    }
    assert_eq!(winners.len(), 4, "concurrent intents share one quota bound");
    let foreign = format!("foreign-{unique}");
    set_quota(&pool, &foreign, subject, 4).await;
    let foreign_id = identity(&foreign, subject);
    store
        .prepare(&foreign_id, 1, 10_000)
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
    // A forged tenant cannot mark another tenant's durable intent dispatched.
    let mut forged = winners[0].clone();
    forged.tenant = foreign.clone();
    assert!(store.mark_dispatched(&forged).await.is_err());
    for id in &winners {
        store.finish(id).await.unwrap();
    }
    store.finish(&foreign_id).await.unwrap();
    let expiry_user = "expiry-subject";
    set_quota(&pool, &tenant, expiry_user, 1).await;
    let id = identity(&tenant, expiry_user);
    store
        .prepare(&id, 1, 80)
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    let next = identity(&tenant, expiry_user);
    let prepared = store.prepare(&next, 1, 1000).await.unwrap();
    assert_eq!(
        prepared.envelope.pending, 0,
        "only never-dispatched expired intents are reclaimed"
    );
    prepared.commit().await.unwrap();
    store.mark_dispatched(&next).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    store.finish(&next).await.unwrap();
    assert!(
        matches!(
            store
                .prepare(&identity(&tenant, expiry_user), 1, 5000)
                .await,
            Err(ReservationError::QuotaExceeded)
        ),
        "completion/timeout without a canonical quota checkpoint cannot refund"
    );
    let state:String=sqlx::query_scalar("SELECT state FROM gateway.dispatch_token_reservations WHERE tenant_id=$1 AND user_id=$2 AND member=$3::text::uuid")
        .bind(&tenant).bind(expiry_user).bind(&next.member).fetch_one(&pool).await.map_err(|_|()).unwrap();
    assert_eq!(state, "outcome_unknown");
    pool.close().await;
}
fn identity(tenant: &str, user: &str) -> ReservationIdentity {
    ReservationIdentity {
        tenant: tenant.into(),
        user: user.into(),
        request: uuid::Uuid::new_v4().to_string(),
        service: "gw07-lifecycle-test".into(),
        request_type: "invoke".into(),
        member: uuid::Uuid::new_v4().to_string(),
        logical_id: uuid::Uuid::new_v4().to_string(),
    }
}
async fn set_quota(pool: &PgPool, tenant: &str, user: &str, limit: i64) {
    // Fixture policy through an existing table. No accounts or ledger evidence
    // are created, and no caller receives broader privileges for this gate.
    sqlx::query("INSERT INTO gateway.user_quotas(tenant_id,user_id,daily_token_limit,monthly_token_limit,overage_strategy) VALUES($1,$2,$3,$3,'hard_block')")
        .bind(tenant).bind(user).bind(limit).execute(pool).await.map_err(|_|()).unwrap();
}
