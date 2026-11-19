//! URL and batch source admission; the shared fetch pins public DNS per hop.
use super::{
    Actor, IngestError, IngestionService, Result, dataset_ingestion_identity, object,
    require_index_writable, require_public_metadata, string,
};
use serde_json::{Value, json};
use std::time::Duration;
pub fn redacted_source_url(raw: &str) -> Result<String> {
    let parts = super::display_url::split(raw)
        .ok_or_else(|| IngestError::Validation("invalid source URL".into()))?;
    if let Some((userinfo, _)) = parts.authority.as_deref().and_then(|v| v.rsplit_once('@')) {
        let (username, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        if !username.is_empty() || !password.is_empty() {
            return Err(IngestError::Validation(
                "URL userinfo is not allowed".into(),
            ));
        }
    }
    Ok(super::display_url::render(
        &parts,
        &super::display_url::redact_query(&parts.query),
    ))
}
impl IngestionService {
    pub async fn create_document_from_url(
        &self,
        actor: &Actor,
        dataset: &str,
        url: &str,
        title: Option<&str>,
        metadata: Value,
    ) -> Result<Value> {
        require_public_metadata(&metadata)?;
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let raw = url.trim();
        if raw.is_empty() {
            return Err(IngestError::Validation("url is required".into()));
        }
        let source_url = redacted_source_url(raw)?;
        let bytes = crate::migration_adapters::safe_fetch_with_policy(
            raw,
            10 * 1024 * 1024,
            5,
            Duration::from_secs(20),
        )
        .await
        .map_err(|e| IngestError::Validation(format!("Failed to fetch url: {e}")))?;
        let size = bytes.len();
        let filename = raw.to_owned();
        let parser = self.parser.clone();
        let (content, mime) = tokio::task::spawn_blocking(move || {
            parser.extract_text_from_bytes(&bytes, &filename, "")
        })
        .await
        .map_err(|_| IngestError::Parser("URL extraction task failed".into()))??;
        let id = uuid::Uuid::new_v4().to_string();
        let document = json!({"document_id":id,"dataset_id":dataset,"title":title.map(str::trim).filter(|s|!s.is_empty()).unwrap_or(&source_url),"source_type":"url","source_uri":source_url,"mime_type":if mime.is_empty(){"text/html"}else{&mime},"size_bytes":size,"status":"waiting","progress":0,"content":content,"metadata":metadata});
        self.store
            .insert_document(&document, &dataset_ingestion_identity(&d))
            .await?;
        Ok(super::service::with_display_status(
            self.store.get_document(dataset, &id).await?,
        ))
    }
    /// The source accepts the Dify process_rule object but intentionally does not
    /// override the authoritative generation-open rule snapshot with it.
    pub async fn batch_create_documents(
        &self,
        actor: &Actor,
        dataset: &str,
        documents: &[Value],
        _process_rule: Option<&Value>,
        batch_name: Option<&str>,
    ) -> Result<Value> {
        self.store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        let batch = batch_name
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("batch_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
            });
        let mut created = vec![];
        let mut errors = json!({});
        for (index, input) in documents.iter().enumerate() {
            let title = input["title"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("doc_{index}"));
            let result: Result<Value> = async {
                let doc = self
                    .create_document_from_text(
                        actor,
                        dataset,
                        &title,
                        string(input, "content"),
                        object(&input["metadata"]),
                    )
                    .await?;
                sqlx::query(
                    "UPDATE knowledge.documents SET batch=$2,updated_at=NOW() WHERE document_id=$1",
                )
                .bind(string(&doc, "document_id"))
                .bind(&batch)
                .execute(crate::database::executor(&self.store.pool))
                .await?;
                Ok(doc)
            }
            .await;
            match result {
                Ok(doc) => created.push(doc),
                Err(error) => errors[format!("doc_{index}")] = json!(error.to_string()),
            }
        }
        Ok(
            json!({"batch":batch,"success_count":created.len(),"failed_count":errors.as_object().map_or(0,|m|m.len()),"documents":created,"errors":errors}),
        )
    }
}
