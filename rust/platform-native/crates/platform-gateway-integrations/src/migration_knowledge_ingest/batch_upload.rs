//! Canonical bounded multipart batch ingestion with disk staging and retry-safe
//! per-file failures. Source admission starts only after the 50-file preflight.
use super::{
    Actor, IngestError, Result,
    http::{WriteApiState, read_error},
    require_index_writable, string,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
pub(super) fn routes(state: WriteApiState) -> Router {
    let limit = state
        .service
        .maximum_batch_upload_bytes
        .clamp(1024 * 1024, 48 * 1024 * 1024)
        + 1024 * 1024;
    Router::new()
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/batch-upload",
            post(upload).layer(DefaultBodyLimit::max(limit)),
        )
        .with_state(state)
}
struct Spool(PathBuf);
impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Staged {
    filename: String,
    mime: String,
    path: PathBuf,
    size: usize,
    error: Option<IngestError>,
}
async fn upload(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    mut multipart: Multipart,
) -> Response {
    let result = async {
        if actor.anonymous() || actor.roles.iter().any(|r| r.trim().eq_ignore_ascii_case("guest")) {
            return Err(IngestError::Permission);
        }
        let dataset_row = s.service.store.require_dataset_access(&actor, &dataset, "editor").await?;
        require_index_writable(&dataset_row)?;
        let batch = uuid::Uuid::new_v4().to_string();
        let root = s.service.parser.config.scratch_root.join(format!("batch_{}", uuid::Uuid::new_v4()));
        // Source catches tempfile/open/write/flush failures for each file. Keep
        // that partial-result behavior even if the shared spool cannot open.
        let spool_ready = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&s.service.parser.config.scratch_root)?;
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
            builder.create(&root)
        })();
        let spool = Spool(root);
        let max_file = s.maximum_upload_bytes.clamp(1024*1024, 48*1024*1024);
        let max_batch = s.service.maximum_batch_upload_bytes.clamp(1024*1024, 48*1024*1024);
        let mut staged = vec![];
        let mut accepted_bytes = 0usize;
        while let Some(mut field) = multipart.next_field().await.map_err(|_| IngestError::Validation("invalid multipart upload".into()))? {
            if field.name() != Some("files") { continue; }
            if staged.len() >= 50 { return Err(IngestError::Validation("Maximum 50 files allowed per batch".into())); }
            let filename = field.file_name().filter(|v| !v.is_empty()).unwrap_or("unknown").to_owned();
            let mime = field.content_type().unwrap_or("").to_owned();
            let ext = std::path::Path::new(&filename).extension().and_then(|v| v.to_str()).unwrap_or("").to_lowercase();
            let path = spool.0.join(format!("{}.{ext}", staged.len()));
            let mut size = 0usize;
            let mut error = None;
            if !["pdf", "docx", "txt", "md", "html"].contains(&ext.as_str()) {
                error = Some(IngestError::Validation(format!("Unsupported file type: {}. Allowed: .pdf, .docx, .txt, .md, .html", if ext.is_empty() {String::new()} else {format!(".{ext}")})));
            } else if spool_ready.is_err() { error = Some(IngestError::Storage); }
            let mut file = if error.is_none() {
                let mut options = tokio::fs::OpenOptions::new(); options.write(true).create_new(true);
                #[cfg(unix)] { options.mode(0o600); }
                match options.open(&path).await { Ok(file) => Some(file), Err(_) => {error=Some(IngestError::Storage);None} }
            } else { None };
            while let Some(chunk) = field.chunk().await.map_err(|_| IngestError::Validation("invalid multipart upload".into()))? {
                // Always drain rejected fields so later files retain their indices.
                if error.is_some() { continue; }
                size = size.saturating_add(chunk.len());
                if size > max_file {
                    error = Some(IngestError::Validation(format!("File too large: {:.1}MB exceeds limit of {}MB",size as f64/1024./1024.,max_file/(1024*1024))));
                } else if accepted_bytes.saturating_add(size) > max_batch {
                    error = Some(IngestError::Validation(format!("Batch upload exceeds aggregate limit of {}MB",max_batch/(1024*1024))));
                } else if let Some(file) = file.as_mut() {
                    if file.write_all(&chunk).await.is_err() { error = Some(IngestError::Storage); }
                }
            }
            if error.is_none() {
                if let Some(file) = file.as_mut() { if file.flush().await.is_err() {error=Some(IngestError::Storage);} }
            }
            drop(file);
            if error.is_none() {
                if size == 0 { error=Some(IngestError::Validation("Empty files are not accepted".into())); }
                else {
                    let cap = if ext=="html" {super::parser::MAX_HTML_BYTES} else {super::parser::MAX_SOURCE_BYTES};
                    if size > cap {error=Some(IngestError::Validation(format!("{filename} exceeds the {}MB parser limit for .{ext} files",cap/(1024*1024))));}
                }
            }
            if error.is_none() { accepted_bytes += size; }
            staged.push(Staged {filename,mime,path,size,error});
        }
        if staged.is_empty() {return Err(IngestError::Validation("No files provided".into()));}
        let total=staged.len(); let (mut documents,mut errors)=(vec![],vec![]);
        for (index,file) in staged.into_iter().enumerate() {
            let mut started=false;let filename=file.filename.clone();
            let outcome:Result<_> = async {
                if let Some(error)=file.error {return Err(error);}
                let bytes=tokio::fs::read(&file.path).await?;
                if bytes.len()!=file.size {return Err(IngestError::Storage);}
                s.service.parser.require_supported_source(&bytes,&filename,&file.mime)?;
                started=true;
                let mut document=s.service.create_document_from_upload(&actor,&dataset,&filename,&bytes,&file.mime,json!({}),"text_only").await?;
                document["batch_id"]=json!(batch);Ok(document)
            }.await;
            let _=tokio::fs::remove_file(&file.path).await;
            match outcome {
                Ok(doc)=>documents.push((index,doc)),
                Err(error)=> {let safe=!started&&matches!(error,IngestError::Validation(_));errors.push(json!({"file_index":index,"filename":filename,"retry_safe":safe,"error":if safe{error.to_string()}else{"Upload state is uncertain; check the document list before retrying".into()}}));}
            }
        }
        let mut queued=vec![];
        for(index,doc) in documents {
            if s.service.store.enqueue(&dataset,string(&doc,"document_id"),None,None,None,false).await.unwrap_or(false) {queued.push(doc);}
            else {errors.push(json!({"file_index":index,"document_id":doc["document_id"],"filename":doc["title"],"error":"Document was not accepted by the durable ingestion queue","retry_safe":false}));}
        }
        drop(spool);
        Ok(json!({"batch_id":batch,"total":total,"accepted":queued.len(),"rejected":errors.len(),"documents":queued,"errors":errors}))
    }.await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => read_error(error, StatusCode::BAD_REQUEST, false),
    }
}
