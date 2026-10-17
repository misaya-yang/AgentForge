//! Version/segment commit under the existing publication lease.
use super::*;
impl KnowledgeWriteStore {
    pub async fn commit_text_publication(
        publication: &mut PublicationLease,
        dataset: &str,
        document: &str,
        chunks: &[Chunk],
        keep_ids: &[String],
        staged_ids: &[String],
        expected: &str,
        content: &str,
        metadata_patch: &Value,
        expected_content: &str,
        pending: Option<&Value>,
        replace_images: bool,
        lexical: Option<(
            &crate::migration_knowledge_query::lexical_lifecycle::LexicalLifecycle,
            &crate::migration_knowledge_query::lexical_lifecycle::ActivePublicationContext,
        )>,
    ) -> Result<i64> {
        Self::commit_text_publication_with_docir(
            publication,
            dataset,
            document,
            chunks,
            keep_ids,
            staged_ids,
            expected,
            content,
            metadata_patch,
            expected_content,
            pending,
            replace_images,
            lexical,
            None,
        )
        .await
    }
    pub async fn commit_text_publication_with_docir(
        publication: &mut PublicationLease,
        dataset: &str,
        document: &str,
        chunks: &[Chunk],
        keep_ids: &[String],
        staged_ids: &[String],
        expected: &str,
        content: &str,
        metadata_patch: &Value,
        expected_content: &str,
        pending: Option<&Value>,
        replace_images: bool,
        lexical: Option<(
            &crate::migration_knowledge_query::lexical_lifecycle::LexicalLifecycle,
            &crate::migration_knowledge_query::lexical_lifecycle::ActivePublicationContext,
        )>,
        docir: Option<(
            &crate::migration_knowledge_ingest::parsing_v2::integration::Prepared,
            &crate::migration_knowledge_ingest::assets::AssetStorage,
        )>,
    ) -> Result<i64> {
        let publication_revision = publication.revision;
        let mut tx = sqlx::Connection::begin(publication.connection()).await?;
        Self::require_identity(&mut tx, dataset, Some(expected)).await?;
        let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.documents d WHERE document_id=$1 AND dataset_id=$2 AND status='indexing' FOR UPDATE").bind(document).bind(dataset).fetch_optional(&mut *tx).await?.ok_or(IngestError::GenerationChanged)?;
        if string(&current, "content") != expected_content {
            return Err(IngestError::GenerationChanged);
        }
        if let Some(pending) = pending {
            let previous = integer(pending, "previous_version");
            let candidate = integer(pending, "candidate_version");
            if current["metadata"]["_document_pending_restore_version"] != *pending
                || previous <= integer(&current, "current_version")
                || candidate <= previous
            {
                return Err(IngestError::GenerationChanged);
            }
            for (number, status, next, text) in [
                (
                    previous,
                    "pending_before_restore",
                    "updated",
                    expected_content,
                ),
                (candidate, "pending_restore", "restored", content),
            ] {
                if sqlx::query("UPDATE knowledge.document_versions SET change_type=$4 WHERE document_id=$1 AND version_number=$2 AND change_type=$3 AND content=$5 RETURNING version_id").bind(document).bind(number as i32).bind(status).bind(next).bind(text).fetch_optional(&mut *tx).await?.is_none(){return Err(IngestError::GenerationChanged)}
            }
            sqlx::query("UPDATE knowledge.documents SET current_version=$2,version_count=COALESCE(version_count,0)+2,metadata=(COALESCE(metadata,'{}'::jsonb)-'_document_pending_restore_version')||jsonb_build_object('_document_restored_source_version',$2::integer) WHERE document_id=$1").bind(document).bind(candidate as i32).execute(&mut *tx).await?;
        } else if !content.trim().is_empty() {
            let current_version = integer(&current, "current_version");
            let active: Option<Value> = if current_version > 0 {
                sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2 AND change_type NOT IN ('pending_restore','pending_before_restore') FOR UPDATE")
                    .bind(document)
                    .bind(current_version as i32)
                    .fetch_optional(&mut *tx)
                    .await?
            } else {
                None
            };
            if !active
                .as_ref()
                .is_some_and(|version| source_version_matches(version, content))
            {
                let has_published_version: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.document_versions WHERE document_id=$1 AND change_type NOT IN ('pending_restore','pending_before_restore'))")
                    .bind(document)
                    .fetch_one(&mut *tx)
                    .await?;
                let mut version_metadata = object(&current["metadata"]);
                if let (Some(target), Some(patch)) =
                    (version_metadata.as_object_mut(), metadata_patch.as_object())
                {
                    target.extend(
                        patch
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                }
                if let Some(metadata) = version_metadata.as_object_mut() {
                    for key in [
                        "_document_ingest_action",
                        "_document_recover_stage",
                        "_document_pipeline_execution_id",
                        "_document_lifecycle_reindex",
                    ] {
                        metadata.remove(key);
                    }
                }
                Self::create_version(
                    &mut tx,
                    document,
                    content,
                    if has_published_version {
                        "updated"
                    } else {
                        "created"
                    },
                    Some(string(&current, "title")),
                    &version_metadata,
                    None,
                    None,
                    true,
                )
                .await?;
            }
        }
        let mut metadata_patch = metadata_patch.clone();
        let mut finalized_chunks = chunks.to_vec();
        if let Some((prepared, assets)) = docir {
            let published = prepared
                .finalize(&mut tx, assets, publication_revision)
                .await?;
            published.stamp_chunks(&mut finalized_chunks)?;
            metadata_patch
                .as_object_mut()
                .ok_or(IngestError::GenerationChanged)?
                .extend(
                    published
                        .patch
                        .as_object()
                        .ok_or(IngestError::GenerationChanged)?
                        .clone(),
                );
        }
        Self::insert_segments(&mut tx, &finalized_chunks).await?;
        sqlx::query(sql::DELETE_SEGMENTS_BY_DOCUMENT_1)
            .bind(document)
            .bind(keep_ids)
            .bind("text")
            .execute(&mut *tx)
            .await?;
        if replace_images {
            sqlx::query("DELETE FROM knowledge.segments WHERE document_id=$1 AND content_type='image' AND NOT(segment_id=ANY($2::text[]))").bind(document).bind(keep_ids).execute(&mut *tx).await?;
        }
        sqlx::query(sql::ACTIVATE_STAGED_SEGMENTS_1)
            .bind(document)
            .bind(staged_ids)
            .execute(&mut *tx)
            .await?;
        let changed=sqlx::query("UPDATE knowledge.documents SET content=$3,metadata=COALESCE(metadata,'{}'::jsonb)||$4::jsonb,word_count=$5,segment_count=(SELECT COUNT(*) FROM knowledge.segments WHERE document_id=$1),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2 AND status='indexing'")
            .bind(document).bind(dataset).bind(content).bind(&metadata_patch).bind(content.split_whitespace().count() as i32).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(IngestError::GenerationChanged);
        }
        Self::update_status(&mut tx, document, "completed", Some(100.0), None).await?;
        let certification = if let Some((lifecycle, context)) = lexical {
            Some(
                lifecycle
                    .recertify_active_publication(context, publication_revision, &mut tx)
                    .await?,
            )
        } else {
            None
        };
        let revision: i64 = sqlx::query_scalar(sql::FINISH_INDEX_PUBLICATION_0)
            .bind(dataset)
            .fetch_one(&mut *tx)
            .await?;
        if let (Some((lifecycle, context)), Some(certification)) = (lexical, certification) {
            if integer(&certification, "target_revision") != revision {
                return Err(IngestError::GenerationChanged);
            }
            lifecycle
                .settle_active_publication(context, &certification, &mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok(revision)
    }
}
