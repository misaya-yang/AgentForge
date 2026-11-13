//! PostgreSQL coverage for spaces.
//! DDL and cleanup use the owner/migrator URL. The repository uses the
//! knowledge_api URL, because tenant binding requires that login.
//! Without those URLs the test stays ignored instead of passing silently.
use ai_platform_knowledge_domain::spaces::{
    ItemFilter, ItemView, MemberInput, NewItem, NewSpace, SharePut, SpaceError, Spaces,
};
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

fn filter(view: ItemView) -> ItemFilter {
    ItemFilter {
        view,
        kind: None,
        query: None,
        space_id: None,
        cursor: None,
        limit: 50,
    }
}

async fn connect(url: &str) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await
        .map_err(|_| "test database connection failed")
        .unwrap()
}

async fn ensure_spaces(owner: &PgPool) {
    let mut connection = owner
        .acquire()
        .await
        .map_err(|_| "owner connection failed")
        .unwrap();
    // The migrator login cannot see knowledge until it assumes the schema owner.
    sqlx::query("SET ROLE ai_gateway_owner")
        .execute(&mut *connection)
        .await
        .map_err(|_| "owner role is unavailable")
        .unwrap();
    let present: bool = sqlx::query_scalar("SELECT to_regclass('knowledge.spaces') IS NOT NULL")
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| "space schema probe failed")
        .unwrap();
    if present {
        return;
    }
    sqlx::raw_sql(include_str!(
        "../../../../../database/migrations/2026_08_post_kb_v1/030_knowledge_spaces.sql"
    ))
    .execute(&mut *connection)
    .await
    .map_err(|_| "030 migration was not applied")
    .unwrap();
}

async fn cleanup(owner: &PgPool) {
    let mut connection = owner
        .acquire()
        .await
        .map_err(|_| "owner connection failed")
        .unwrap();
    sqlx::query("SET ROLE ai_gateway_owner")
        .execute(&mut *connection)
        .await
        .map_err(|_| "owner role is unavailable")
        .unwrap();
    sqlx::query("SELECT pg_catalog.set_config('search_path','pg_catalog, knowledge, public', false)")
        .execute(&mut *connection)
        .await
        .map_err(|_| "owner search_path setup failed")
        .unwrap();
    let statements = [
        "DELETE FROM knowledge.space_item_activity WHERE item_id IN (SELECT item_id FROM knowledge.space_items WHERE tenant_id IN ('tenant-a', 'tenant-b'))",
        "DELETE FROM knowledge.space_item_stars WHERE item_id IN (SELECT item_id FROM knowledge.space_items WHERE tenant_id IN ('tenant-a', 'tenant-b'))",
        "DELETE FROM knowledge.space_item_shares WHERE item_id IN (SELECT item_id FROM knowledge.space_items WHERE tenant_id IN ('tenant-a', 'tenant-b'))",
        "DELETE FROM knowledge.space_item_revisions WHERE item_id IN (SELECT item_id FROM knowledge.space_items WHERE tenant_id IN ('tenant-a', 'tenant-b'))",
        "DELETE FROM knowledge.space_items WHERE tenant_id IN ('tenant-a', 'tenant-b')",
        "DELETE FROM knowledge.space_members WHERE space_id IN (SELECT space_id FROM knowledge.spaces WHERE tenant_id IN ('tenant-a', 'tenant-b'))",
        "DELETE FROM knowledge.spaces WHERE tenant_id IN ('tenant-a', 'tenant-b')",
        "DELETE FROM knowledge.datasets WHERE tenant_id IN ('tenant-a', 'tenant-b') AND COALESCE(index_config->>'hidden_space','false') = 'true'",
    ];
    for (index, statement) in statements.iter().enumerate() {
        sqlx::query(statement)
            .execute(&mut *connection)
            .await
            .map_err(|error| {
                let code = match &error {
                    sqlx::Error::Database(database) => database
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "none".into()),
                    _ => "none".into(),
                };
                format!("space test cleanup failed at {index} ({code})")
            })
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires AI_GATEWAY_TEST_DATABASE_OWNER_URL and AI_GATEWAY_TEST_DATABASE_URL"]
async fn spaces_api() {
    let owner_url = std::env::var("AI_GATEWAY_TEST_DATABASE_OWNER_URL")
        .expect("AI_GATEWAY_TEST_DATABASE_OWNER_URL is unset");
    let api_url = std::env::var("AI_GATEWAY_TEST_DATABASE_URL")
        .expect("AI_GATEWAY_TEST_DATABASE_URL is unset");
    let owner = connect(&owner_url).await;
    let api = connect(&api_url).await;
    ensure_spaces(&owner).await;
    cleanup(&owner).await;
    let result = run(&owner, &api).await;
    cleanup(&owner).await;
    result.unwrap();
}

async fn run(owner: &PgPool, api: &PgPool) -> Result<(), String> {
    let creator = actor("tenant-a", "owner-a");
    let peer = actor("tenant-a", "peer-a");
    let stranger = actor("tenant-a", "stranger-a");
    let foreign = actor("tenant-b", "owner-b");
    let spaces = Spaces::new(api.clone());
    let created = with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        async move {
            spaces
                .create_space(
                    &creator,
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
    let hidden: (bool, bool) = sqlx::query_as(
        "SELECT COALESCE(index_config->>'hidden_space','false') = 'true', is_deleted FROM knowledge.datasets WHERE dataset_id = $1",
    )
    .bind(&created.dataset_id)
    .fetch_one(owner)
    .await
    .map_err(|_| "hidden dataset probe failed".to_owned())?;
    if !hidden.0 || hidden.1 {
        return Err("space dataset is visible or already deleted".into());
    }
    let item = with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        let space_id = created.space_id.clone();
        async move {
            spaces
                .create_item(
                    &creator,
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
    for view in [ItemView::All, ItemView::Mine, ItemView::Recent] {
        let page = with_actor_scope(creator.clone(), {
            let spaces = spaces.clone();
            let creator = creator.clone();
            async move { spaces.list_items(&creator, filter(view)).await }
        })
        .await
        .map_err(|_| "list view failed".to_owned())?;
        if !page.items.iter().any(|row| row.item_id == item.item_id) {
            return Err("created page is missing from a list view".into());
        }
    }
    with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        let item_id = item.item_id.clone();
        async move { spaces.star(&creator, &item_id, true).await }
    })
    .await
    .map_err(|_| "star failed".to_owned())?;
    let starred = with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        async move { spaces.list_items(&creator, filter(ItemView::Starred)).await }
    })
    .await
    .map_err(|_| "starred view failed".to_owned())?;
    if !starred.items.iter().any(|row| row.item_id == item.item_id && row.starred) {
        return Err("starred view missed the page".into());
    }
    with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        let item_id = item.item_id.clone();
        async move {
            spaces
                .put_shares(
                    &creator,
                    &item_id,
                    SharePut {
                        shares: vec![MemberInput {
                            principal_type: "user".into(),
                            principal_id: "peer-a".into(),
                            role: "viewer".into(),
                        }],
                    },
                )
                .await
        }
    })
    .await
    .map_err(|_| "share failed".to_owned())?;
    let shared = with_actor_scope(peer.clone(), {
        let spaces = spaces.clone();
        let peer = peer.clone();
        async move { spaces.list_items(&peer, filter(ItemView::Shared)).await }
    })
    .await
    .map_err(|_| "shared view failed".to_owned())?;
    if !shared.items.iter().any(|row| row.item_id == item.item_id && row.shared) {
        return Err("shared view missed the page".into());
    }
    let denied = with_actor_scope(stranger.clone(), {
        let spaces = spaces.clone();
        let stranger = stranger.clone();
        let item_id = item.item_id.clone();
        async move { spaces.get_item(&stranger, &item_id).await }
    })
    .await;
    if !matches!(denied, Err(SpaceError::NotFound)) {
        return Err("unauthorized item read was not hidden".into());
    }
    let other = with_actor_scope(foreign.clone(), {
        let spaces = spaces.clone();
        let foreign = foreign.clone();
        let item_id = item.item_id.clone();
        async move { spaces.get_item(&foreign, &item_id).await }
    })
    .await;
    if other.is_ok() {
        return Err("cross-tenant item read succeeded".into());
    }
    with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        let item_id = item.item_id.clone();
        async move { spaces.delete_item(&creator, &item_id).await }
    })
    .await
    .map_err(|_| "soft delete failed".to_owned())?;
    let trash = with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        async move { spaces.list_items(&creator, filter(ItemView::Trash)).await }
    })
    .await
    .map_err(|_| "trash view failed".to_owned())?;
    if !trash.items.iter().any(|row| row.item_id == item.item_id) {
        return Err("deleted page is missing from trash".into());
    }
    with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        let item_id = item.item_id.clone();
        async move { spaces.restore_item(&creator, &item_id).await }
    })
    .await
    .map_err(|_| "restore failed".to_owned())?;
    with_actor_scope(creator.clone(), {
        let spaces = spaces.clone();
        let creator = creator.clone();
        let space_id = created.space_id.clone();
        async move { spaces.archive_space(&creator, &space_id).await }
    })
    .await
    .map_err(|_| "archive space failed".to_owned())?;
    let deleted: bool = sqlx::query_scalar(
        "SELECT is_deleted FROM knowledge.datasets WHERE dataset_id = $1",
    )
    .bind(&created.dataset_id)
    .fetch_one(owner)
    .await
    .map_err(|_| "archived dataset probe failed".to_owned())?;
    if !deleted {
        return Err("archived space left its hidden dataset active".into());
    }
    Ok(())
}
