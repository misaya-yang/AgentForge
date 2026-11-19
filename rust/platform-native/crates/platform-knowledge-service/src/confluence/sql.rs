//! Canonical Python Knowledge source-generation SQL, same parameter order.

pub const BEGIN_CONFLUENCE_DOCUMENT_SYNC: &str = r###"
            UPDATE documents
            SET status = 'syncing',
                progress = 0,
                error = NULL,
                metadata = COALESCE(metadata, '{}'::jsonb)
                    || jsonb_build_object(
                        '_confluence_sync_generation',
                        $4::jsonb
                    ),
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND (
                    (
                        status IN ('completed', 'error', 'waiting')
                        AND NOT (
                            COALESCE(metadata, '{}'::jsonb)
                            ? '_confluence_sync_generation'
                        )
                    )
                    OR (
                        status = 'syncing'
                        AND (
                            metadata -> '_confluence_sync_generation'
                                ->> 'generation' = $3
                            OR (
                                metadata -> '_confluence_sync_generation'
                                    -> 'source' -> 'source_owner' = $5::jsonb
                                AND metadata -> '_confluence_sync_generation'
                                    ->> 'generation' IS DISTINCT FROM $3
                                AND COALESCE(
                                    NULLIF(
                                        metadata -> '_confluence_sync_generation'
                                            ->> 'started_at',
                                        ''
                                    )::timestamptz,
                                    '-infinity'::timestamptz
                                ) <= NOW() - make_interval(secs => $6)
                            )
                        )
                    )
              )
              AND COALESCE(enabled, TRUE) = TRUE
              AND COALESCE(archived, FALSE) = FALSE
              AND NOT (
                    COALESCE(metadata, '{}'::jsonb)
                    ? '_document_lifecycle_reindex'
              )
              AND NOT (
                    COALESCE(metadata, '{}'::jsonb)
                    ? '_document_upload_generation'
              )
              AND NOT (
                    COALESCE(metadata, '{}'::jsonb)
                    ? '_document_upload_failed'
              )
            RETURNING document_id
            "###;

pub const ABORT_CONFLUENCE_DOCUMENT_SYNC: &str = r###"
            UPDATE documents
            SET status = 'error',
                progress = 100,
                error = $4,
                metadata = COALESCE(metadata, '{}'::jsonb)
                    - '_confluence_sync_generation',
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status = 'syncing'
              AND metadata -> '_confluence_sync_generation'
                    ->> 'generation' = $3
            RETURNING document_id
            "###;

pub const PREPARE_CONFLUENCE_DOCUMENT_UPDATE: &str = r###"
            UPDATE documents
            SET title = $3,
                content = $4,
                confluence_version = $5,
                metadata = (
                    COALESCE(metadata, '{}'::jsonb)
                    - '_confluence_sync_generation'
                    - '_document_restored_source_version'
                    - 'images_embedded'
                    - 'embedded_image_count'
                ) || $6::jsonb,
                status = 'waiting',
                progress = 0,
                error = NULL,
                started_at = NULL,
                completed_at = NULL,
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status = 'syncing'
              AND COALESCE(enabled, TRUE) = TRUE
              AND COALESCE(archived, FALSE) = FALSE
              AND NOT (
                    COALESCE(metadata, '{}'::jsonb)
                    ? '_document_lifecycle_reindex'
              )
              AND NOT (
                    COALESCE(metadata, '{}'::jsonb)
                    ? '_document_upload_generation'
              )
              AND NOT (
                    COALESCE(metadata, '{}'::jsonb)
                    ? '_document_upload_failed'
              )
              AND metadata -> '_confluence_sync_generation'
                    ->> 'generation' = $7
            RETURNING document_id
            "###;

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use serde_json::json;
    use sqlx::{Connection, PgConnection};
    #[tokio::test]
    #[ignore = "requires isolated KNOWLEDGE_TEST_DATABASE_URL fixture with canonical schema"]
    async fn source_generation_claim_publish_abort_fences_postgres() {
        let url = std::env::var("KNOWLEDGE_TEST_DATABASE_URL").expect("isolated fixture DSN");
        let mut c = PgConnection::connect(&url).await.unwrap();
        // Session-local table preserves canonical column types/defaults without touching fixture rows.
        sqlx::query("CREATE TEMP TABLE documents (LIKE knowledge.documents INCLUDING DEFAULTS)")
            .execute(&mut c)
            .await
            .unwrap();
        sqlx::query("INSERT INTO documents(document_id,dataset_id,title,status,content) VALUES('native-doc','native-dataset','Old','completed','previous generation')").execute(&mut c).await.unwrap();
        let owner = json!({"binding_id":"binding-a","connection_id":"connection-a"});
        let marker = json!({"generation":"g1","started_at":chrono::Utc::now().to_rfc3339(),"source":{"source_owner":owner}});
        let claimed: Option<String> = sqlx::query_scalar(BEGIN_CONFLUENCE_DOCUMENT_SYNC)
            .bind("native-doc")
            .bind("native-dataset")
            .bind("g1")
            .bind(&marker)
            .bind(&owner)
            .bind(3600i32)
            .fetch_optional(&mut c)
            .await
            .unwrap();
        assert_eq!(claimed.as_deref(), Some("native-doc"));
        let content: String =
            sqlx::query_scalar("SELECT content FROM documents WHERE document_id='native-doc'")
                .fetch_one(&mut c)
                .await
                .unwrap();
        assert_eq!(content, "previous generation");
        let stolen: Option<String> = sqlx::query_scalar(BEGIN_CONFLUENCE_DOCUMENT_SYNC)
            .bind("native-doc")
            .bind("native-dataset")
            .bind("g2")
            .bind(json!({"generation":"g2"}))
            .bind(json!({"binding_id":"other"}))
            .bind(3600i32)
            .fetch_optional(&mut c)
            .await
            .unwrap();
        assert!(stolen.is_none());
        let wrong: Option<String> = sqlx::query_scalar(PREPARE_CONFLUENCE_DOCUMENT_UPDATE)
            .bind("native-doc")
            .bind("native-dataset")
            .bind("Wrong")
            .bind("wrong generation")
            .bind(2i32)
            .bind(json!({}))
            .bind("g2")
            .fetch_optional(&mut c)
            .await
            .unwrap();
        assert!(wrong.is_none());
        let published: Option<String> = sqlx::query_scalar(PREPARE_CONFLUENCE_DOCUMENT_UPDATE)
            .bind("native-doc")
            .bind("native-dataset")
            .bind("New")
            .bind("published generation")
            .bind(2i32)
            .bind(json!({"receipt":"complete"}))
            .bind("g1")
            .fetch_optional(&mut c)
            .await
            .unwrap();
        assert!(published.is_some());
        let row:(String,String,bool)=sqlx::query_as("SELECT status,content,metadata ? '_confluence_sync_generation' FROM documents WHERE document_id='native-doc'").fetch_one(&mut c).await.unwrap();
        assert_eq!(
            row,
            ("waiting".into(), "published generation".into(), false)
        );
        let late_abort: Option<String> = sqlx::query_scalar(ABORT_CONFLUENCE_DOCUMENT_SYNC)
            .bind("native-doc")
            .bind("native-dataset")
            .bind("g1")
            .bind("late cancellation")
            .fetch_optional(&mut c)
            .await
            .unwrap();
        assert!(late_abort.is_none());
        sqlx::query("UPDATE documents SET metadata=jsonb_build_object('_document_upload_generation','busy') WHERE document_id='native-doc'").execute(&mut c).await.unwrap();
        let upload_fenced: Option<String> = sqlx::query_scalar(BEGIN_CONFLUENCE_DOCUMENT_SYNC)
            .bind("native-doc")
            .bind("native-dataset")
            .bind("g3")
            .bind(json!({"generation":"g3"}))
            .bind(&owner)
            .bind(3600i32)
            .fetch_optional(&mut c)
            .await
            .unwrap();
        assert!(upload_fenced.is_none());
    }
}
