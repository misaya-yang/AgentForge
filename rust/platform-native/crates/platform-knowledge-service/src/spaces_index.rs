//! Debounced space indexing. Content hashes that did not change are not rewritten.
use crate::migration_knowledge_ingest::IngestionService;
use crate::migration_knowledge_query::types::UserContext;
use crate::spaces_http::{SpaceChange, SpaceIndexHook};
use ai_platform_knowledge_domain::spaces::{self, IndexPlan, Spaces};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Clone)]
struct Indexer {
    spaces: Spaces,
    ingest: IngestionService,
    tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

pub fn hook(spaces: Spaces, ingest: IngestionService) -> SpaceIndexHook {
    let indexer = Indexer {
        spaces,
        ingest,
        tasks: Arc::new(Mutex::new(HashMap::new())),
    };
    SpaceIndexHook {
        notify: Some(Arc::new(move |user, change| indexer.schedule(user, change))),
    }
}

impl Indexer {
    fn schedule(&self, user: UserContext, change: SpaceChange) {
        let indexer = self.clone();
        let tasks = self.tasks.clone();
        let key = match &change {
            SpaceChange::Item(id) => format!("item:{id}"),
            SpaceChange::Space(id) => format!("space:{id}"),
        };
        tokio::spawn(async move {
            let mut guard = tasks.lock().await;
            if let Some(previous) = guard.remove(&key) {
                previous.abort();
            }
            let task = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                indexer.run(user, change).await;
            });
            guard.insert(key, task);
        });
    }

    async fn run(&self, user: UserContext, change: SpaceChange) {
        match change {
            SpaceChange::Item(item_id) => self.index_item(&user, &item_id).await,
            SpaceChange::Space(space_id) => match self.spaces.list_tree(&user, &space_id).await {
                Ok(items) => {
                    for item in items {
                        self.index_item(&user, &item.item_id).await;
                    }
                }
                Err(error) => warn!(?error, "space access sync skipped"),
            },
        }
    }

    async fn index_item(&self, user: &UserContext, item_id: &str) {
        let snapshot = match self.spaces.index_snapshot(user, item_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!(?error, "space index snapshot skipped");
                return;
            }
        };
        let (tenant_visible, grants, viewers) = match self.spaces.access_viewers(user, item_id).await {
            Ok(value) => value,
            Err(error) => {
                warn!(?error, "space access list skipped");
                return;
            }
        };
        if let Err(error) = self
            .spaces
            .sync_dataset_access(user, &snapshot.dataset_id, tenant_visible, &grants)
            .await
        {
            warn!(?error, "space dataset access sync skipped");
        }
        let metadata = spaces::document_metadata(&snapshot, &viewers);
        match spaces::plan(&snapshot) {
            IndexPlan::Skip => {
                if let Some(document_id) = snapshot.document_id.as_deref() {
                    let _ = self
                        .spaces
                        .note_indexed(user, &snapshot.dataset_id, document_id, &metadata)
                        .await;
                }
            }
            IndexPlan::Archive { document_id } => {
                if self
                    .ingest
                    .set_document_archived(user, &snapshot.dataset_id, &document_id, true, Some("space item removed"))
                    .await
                    .is_err()
                {
                    warn!("space document archive failed");
                }
            }
            IndexPlan::Replace { document_id, expected_version, markdown } => {
                match self
                    .ingest
                    .replace_text_source(user, &snapshot.dataset_id, &document_id, expected_version, &markdown)
                    .await
                {
                    Ok(_) => {
                        let _ = self.spaces.note_indexed(user, &snapshot.dataset_id, &document_id, &metadata).await;
                    }
                    Err(_) => self.create_document(user, &snapshot, &markdown, &metadata).await,
                }
            }
            IndexPlan::Create { markdown } => self.create_document(user, &snapshot, &markdown, &metadata).await,
        }
    }

    async fn create_document(&self, user: &UserContext, snapshot: &spaces::IndexSnapshot, markdown: &str, metadata: &Value) {
        if let Some(previous) = snapshot.document_id.as_deref() {
            let _ = self
                .ingest
                .set_document_archived(user, &snapshot.dataset_id, previous, true, Some("space item replaced"))
                .await;
            let _ = self.spaces.clear_document(user, &snapshot.item_id).await;
        }
        match self
            .ingest
            .create_document_from_text(user, &snapshot.dataset_id, &snapshot.title, markdown, metadata.clone())
            .await
        {
            Ok(document) => {
                let document_id = document
                    .get("document_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if document_id.is_empty() {
                    warn!("space document create returned no id");
                    return;
                }
                if self.spaces.bind_document(user, &snapshot.item_id, &document_id).await.is_err() {
                    warn!("space document bind failed");
                }
            }
            Err(_) => warn!("space document create failed"),
        }
    }
}
