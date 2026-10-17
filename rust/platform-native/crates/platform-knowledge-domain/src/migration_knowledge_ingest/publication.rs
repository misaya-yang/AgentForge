//! Stable-identity incremental publication and persisted-segment re-embedding.
use super::{
    Chunk, Dataset, IngestError, IngestionService, Result,
    assets::AssetStorage,
    integer, object, sql,
    store::{DocumentLease, KnowledgeWriteStore},
    string,
};
use serde_json::{Value, json};
use sqlx::Connection;
use std::collections::{BTreeMap, BTreeSet};
fn collection(dataset: &Dataset, row: &Chunk) -> Result<String> {
    let base = dataset
        .collection_name
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| IngestError::Validation("dataset collection is missing".into()))?;
    Ok(match row.level {
        1 => format!("{base}_summary"),
        2 => format!("{base}_sections"),
        _ => base.clone(),
    })
}
// Tag only contention at this exact pre-mutation admission point. A busy
// document lease or any later/ambiguous publication failure is not a retry.
fn publication_admission_error(error: IngestError) -> IngestError {
    match error {
        IngestError::LeaseBusy => IngestError::PublicationContended,
        other => other,
    }
}

impl IngestionService {
    pub(crate) async fn publish_incremental(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        identity: &str,
        content: &str,
        patch: &Value,
        pending: Option<&Value>,
        old: &[Chunk],
        chunks: &mut [Chunk],
        reembed: bool,
        image_points: BTreeMap<String, Vec<Value>>,
        replace_images: bool,
        docir: Option<&super::parsing_v2::integration::Prepared>,
    ) -> Result<Vec<String>> {
        let document = string(doc, "document_id");
        let image_prepared_ids: BTreeSet<String> = image_points
            .values()
            .flatten()
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        let positions: BTreeMap<_, _> = old
            .iter()
            .map(|s| ((s.content_type.clone(), s.position), s))
            .collect();
        let mut staged = vec![];
        let mut embed = vec![];
        let mut keep_ids = if reembed {
            old.iter().map(|r| r.segment_id.clone()).collect::<Vec<_>>()
        } else {
            vec![]
        };
        for row in chunks {
            let previous = positions.get(&(row.content_type.clone(), row.position));
            if let Some(previous) = previous {
                row.segment_id = previous.segment_id.clone();
                row.vector_id = previous.vector_id.clone();
                row.extra = previous.extra.clone();
            }
            if row.vector_id.as_deref().unwrap_or("").is_empty() {
                row.vector_id = Some(row.segment_id.clone())
            }
            if !keep_ids.contains(&row.segment_id) {
                keep_ids.push(row.segment_id.clone());
            }
            if !reembed {
                if let Some(previous) = previous {
                    if row.content_hash == previous.content_hash && row.content_hash.is_some() {
                        if previous.status != "indexing" {
                            continue;
                        }
                        let mut scoped = dataset.clone();
                        scoped.collection_name = Some(collection(dataset, row)?);
                        let point = self
                            .indexer
                            .snapshot_points(&scoped, &[row.vector_id.clone().unwrap()])
                            .await?
                            .into_iter()
                            .next();
                        if let Some(point) = point {
                            let p = &point["payload"];
                            if p["tenant_id"] != dataset.tenant_id
                                || p["dataset_id"] != dataset.dataset_id
                                || p["document_id"] != document
                                || p["segment_id"] != row.segment_id
                                || p["position"] != row.position
                                || p["text"] != row.text
                                || p.get("content_type").is_some_and(|v| v != "text")
                            {
                                return Err(IngestError::Validation(
                                    "staged text vector identity changed before replay".into(),
                                ));
                            }
                            let mut r = (**previous).clone();
                            r.enabled = false;
                            r.status = "indexing".into();
                            staged.push(r);
                            continue;
                        }
                    }
                }
            }
            let disabled = row.extra.get("disabled_by").is_some_and(|v| !v.is_null());
            let mut embedding = row.clone();
            embedding.enabled = !disabled;
            embed.push(embedding);
            let mut staging = row.clone();
            staging.enabled = false;
            staging.status = "indexing".into();
            staged.push(staging);
        }
        let texts: Vec<_> = embed
            .iter()
            .filter(|r| r.content_type != "image")
            .cloned()
            .collect();
        let mut prepared = self.indexer.prepare_chunks(dataset, &texts).await?;
        for (name, points) in image_points {
            prepared.entry(name).or_default().extend(points);
        }
        for row in embed
            .iter()
            .filter(|r| r.content_type == "image" && !image_prepared_ids.contains(&r.segment_id))
        {
            let attachment = row.image_attachment_id.as_deref().ok_or_else(|| {
                IngestError::Validation("image reembed attachment identity missing".into())
            })?;
            let filename = row
                .image_filename
                .as_deref()
                .ok_or_else(|| IngestError::Validation("image reembed filename missing".into()))?;
            let key = AssetStorage::image_key(&dataset.tenant_id, document, attachment, filename)?;
            let bytes = self.assets.download_original(&key).await?;
            let vector = self
                .indexer
                .embeddings
                .embed_image_with_context(dataset, &bytes, Some(&row.text))
                .await?;
            let name = collection(dataset, row)?;
            let mut scoped = dataset.clone();
            scoped.collection_name = Some(name.clone());
            let id = row.vector_id.clone().unwrap();
            let existing = self
                .indexer
                .snapshot_points(&scoped, &[id.clone()])
                .await?
                .into_iter()
                .next();
            let mut payload=existing.as_ref().map(|p|object(&p["payload"])).unwrap_or(json!({"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":row.segment_id,"position":row.position,"text":row.text,"content_type":"image","metadata":row.metadata}));
            payload["enabled"] = json!(row.enabled);
            prepared
                .entry(name)
                .or_default()
                .push(json!({"id":id,"vector":{"":vector},"payload":payload}));
        }
        let staged_ids: Vec<_> = staged.iter().map(|r| r.segment_id.clone()).collect();
        let keep: BTreeSet<_> = keep_ids.iter().collect();
        let excess: Vec<_> = old
            .iter()
            .filter(|r| {
                !reembed
                    && (r.content_type == "text" || replace_images)
                    && !keep.contains(&r.segment_id)
            })
            .collect();
        let mut affected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (name, points) in &prepared {
            for p in points {
                affected
                    .entry(name.clone())
                    .or_default()
                    .insert(string(p, "id").into());
            }
        }
        for row in &excess {
            if let Some(id) = &row.vector_id {
                affected
                    .entry(collection(dataset, row)?)
                    .or_default()
                    .insert(id.clone());
            }
        }
        KnowledgeWriteStore::update_status(
            lease.connection(),
            document,
            "indexing",
            Some(50.0),
            None,
        )
        .await?;
        let lifecycle = crate::migration_knowledge_query::lexical_lifecycle::LexicalLifecycle::new(
            self.store.pool.clone(),
            self.indexer.vectors.clone(),
        );
        let lexical = lifecycle
            .active_publication_context(&dataset.dataset_id)
            .await?;
        let mut publication = self
            .store
            .publication_lease(&dataset.dataset_id, identity)
            .await
            .map_err(publication_admission_error)?;
        let mut backups = BTreeMap::new();
        let mut publishing = dataset.clone();
        publishing.content_revision = Some(publication.revision);
        for (name, ids) in &affected {
            let mut scope = publishing.clone();
            scope.collection_name = Some(name.clone());
            let ids: Vec<_> = ids.iter().cloned().collect();
            let mut snapshots = if publication.recovered {
                self.indexer.load_rollback_backups(&scope).await?
            } else {
                vec![]
            };
            if snapshots.is_empty() {
                snapshots = self.indexer.snapshot_points(&scope, &ids).await?;
            }
            self.indexer
                .persist_rollback_backups(&scope, &snapshots, publication.revision)
                .await?;
            backups.insert(name.clone(), snapshots);
        }
        let result = async {
            KnowledgeWriteStore::require_identity(
                publication.connection(),
                &dataset.dataset_id,
                Some(identity),
            )
            .await?;
            self.indexer
                .publish_prepared(&publishing, &prepared)
                .await?;
            for row in &excess {
                if let Some(id) = &row.vector_id {
                    let mut scope = publishing.clone();
                    scope.collection_name = Some(collection(dataset, row)?);
                    self.indexer.delete_points(&scope, &[id.clone()]).await?;
                }
            }
            KnowledgeWriteStore::commit_text_publication_with_docir(
                &mut publication,
                &dataset.dataset_id,
                document,
                &staged,
                &keep_ids,
                &staged_ids,
                identity,
                content,
                patch,
                string(doc, "content"),
                pending,
                replace_images,
                lexical.as_ref().map(|c| (&lifecycle, c)),
                docir.map(|prepared| (prepared, &self.assets)),
            )
            .await?;
            Ok::<_, IngestError>(staged_ids.clone())
        }
        .await;
        if let Err(error) = result {
            let rollback = async {
                for (name, snapshots) in &backups {
                    let mut scope = publishing.clone();
                    scope.collection_name = Some(name.clone());
                    self.indexer.restore_points(&scope, snapshots).await?;
                    let old_ids: BTreeSet<_> =
                        snapshots.iter().filter_map(|p| p["id"].as_str()).collect();
                    let new: Vec<_> = affected[name]
                        .iter()
                        .filter(|id| !old_ids.contains(id.as_str()))
                        .cloned()
                        .collect();
                    self.indexer.delete_points(&scope, &new).await?;
                }
                let revision = publication.revision;
                let mut tx = publication.connection().begin().await?;
                let certification = if let Some(context) = &lexical {
                    Some(
                        lifecycle
                            .recertify_active_publication(context, revision, &mut tx)
                            .await?,
                    )
                } else {
                    None
                };
                let final_revision: i64 = sqlx::query_scalar(sql::FINISH_INDEX_PUBLICATION_0)
                    .bind(&dataset.dataset_id)
                    .fetch_one(&mut *tx)
                    .await?;
                if let (Some(context), Some(certification)) = (&lexical, &certification) {
                    if integer(certification, "target_revision") != final_revision {
                        return Err(IngestError::GenerationChanged);
                    }
                    lifecycle
                        .settle_active_publication(context, certification, &mut tx)
                        .await?;
                }
                tx.commit().await?;
                Ok::<_, IngestError>(())
            }
            .await;
            if rollback.is_ok() {
                for (name, _) in &backups {
                    let mut scope = publishing.clone();
                    scope.collection_name = Some(name.clone());
                    let _ = self.indexer.clear_rollback_backups(&scope).await;
                }
            }
            publication.release().await?;
            return Err(error);
        }
        for (name, _) in &backups {
            let mut scope = publishing.clone();
            scope.collection_name = Some(name.clone());
            let _ = self.indexer.clear_rollback_backups(&scope).await;
        }
        publication.release().await?;
        result
    }
}

#[cfg(test)]
mod publication_contention_tests {
    use super::*;

    #[test]
    fn only_prepublication_lease_contention_is_tagged_for_same_generation_retry() {
        assert_eq!(
            publication_admission_error(IngestError::LeaseBusy),
            IngestError::PublicationContended
        );
        for error in [
            IngestError::GenerationChanged,
            IngestError::Cancelled,
            IngestError::NotFound,
            IngestError::Permission,
            IngestError::Database,
            IngestError::Storage,
            IngestError::Validation("invalid generation".into()),
            IngestError::Parser("parse failure".into()),
            IngestError::Index("uncertain vector outcome".into()),
        ] {
            assert_eq!(publication_admission_error(error.clone()), error);
        }
    }

    #[test]
    fn contention_retains_existing_conflict_message() {
        assert_eq!(
            IngestError::PublicationContended.to_string(),
            IngestError::LeaseBusy.to_string()
        );
    }
}
