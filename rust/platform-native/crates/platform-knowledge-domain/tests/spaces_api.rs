//! PostgreSQL coverage for spaces. Without a disposable database the check is
//! skipped; the tables are created from the unmeasured 030 proposal and removed
//! before the test returns.
use ai_platform_knowledge_domain::spaces::{NewItem, NewSpace, Spaces};
use ai_platform_service_primitives::knowledge_model::with_actor_scope;
use ai_platform_service_primitives::knowledge_types::UserContext;
use sqlx::PgPool;

fn actor(tenant: &str, user: &str) -> UserContext {
    UserContext {
        user_id: user.into(),
        tenant_id: tenant.into(),
        user_tier: "normal".into(),
        user_type: "user".into(),
        roles: vec!["user".into()],
        is_authenticated: true,
    }
}

fn business_ddl() -> &'static str {
    let sql = include_str!("../../../../../docs/design/knowledge-spaces-030.sql.proposal");
    let start = sql.find("-- BEGIN BUSINESS DDL").expect("marker") + "-- BEGIN BUSINESS DDL".len();
    let end = sql[start..].find("-- END BUSINESS DDL").expect("marker");
    sql[start..start + end].trim()
}

async fn reset(pool: &PgPool) {
    sqlx::query(
        "DROP TABLE IF EXISTS knowledge.space_item_activity, knowledge.space_item_stars, knowledge.space_item_shares, knowledge.space_item_revisions, knowledge.space_items, knowledge.space_members, knowledge.spaces CASCADE",
    )
    .execute(pool)
    .await
    .ok();
}

#[tokio::test]
async fn spaces_api() {
    let Ok(url) = std::env::var("AI_GATEWAY_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: AI_GATEWAY_TEST_DATABASE_URL is unset; successor 030 is not measured in this environment");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .map_err(|_| "test database connection failed")
        .unwrap();
    reset(&pool).await;
    let applied = sqlx::raw_sql(business_ddl()).execute(&pool).await;
    if let Err(error) = applied {
        reset(&pool).await;
        panic!("030 business DDL was not applied on the disposable database: {error}");
    }
    let result = run(&pool).await;
    reset(&pool).await;
    result.unwrap();
}

async fn run(pool: &PgPool) -> Result<(), String> {
    let owner = actor("tenant-a", "owner-a");
    let other = actor("tenant-b", "owner-b");
    let spaces = Spaces::new(pool.clone());
    let created = with_actor_scope(owner.clone(), {
        let spaces = spaces.clone();
        let owner = owner.clone();
        async move {
            spaces
                .create_space(
                    &owner,
                    NewSpace {
                        name: "Notes".into(),
                        icon: String::new(),
                        kind: "personal".into(),
                        visibility: "private".into(),
                    },
                )
                .await
        }
    })
    .await
    .map_err(|_| "create space failed".to_owned())?;
    let hidden: bool = sqlx::query_scalar(
        "SELECT COALESCE(index_config->>'hidden_space','false') = 'true' FROM knowledge.datasets WHERE dataset_id = $1",
    )
    .bind(&created.dataset_id)
    .fetch_one(pool)
    .await
    .map_err(|error| error.to_string())?;
    if !hidden {
        return Err("space dataset is visible to the knowledge catalog".into());
    }
    let item = with_actor_scope(owner.clone(), {
        let spaces = spaces.clone();
        let owner = owner.clone();
        let space_id = created.space_id.clone();
        async move {
            spaces
                .create_item(
                    &owner,
                    &space_id,
                    NewItem {
                        title: "Welcome".into(),
                        kind: "page".into(),
                        icon: String::new(),
                        parent_id: None,
                        position: None,
                        content_format: "markdown".into(),
                        source: String::new(),
                    },
                )
                .await
        }
    })
    .await
    .map_err(|_| "create item failed".to_owned())?;
    let foreign = with_actor_scope(other.clone(), {
        let spaces = spaces.clone();
        let other = other.clone();
        async move { spaces.get_item(&other, &item.item_id).await }
    })
    .await;
    if foreign.is_ok() {
        return Err("cross-tenant item read succeeded".into());
    }
    Ok(())
}
