//! Atomic hidden source-generation publication under the existing document lease.
use super::legacy_sync::LegacySyncService;
use super::*;
use super::{client::scalar, sync::Execution};
use sqlx::Connection;
impl LegacySyncService {
    pub async fn publish_page(
        &self,
        e: &Execution,
        credentials: &credentials::Credentials,
        page_id: &str,
    ) -> Result<(String, bool, bool)> {
        if self.worker.is_none() {
            return Err(Error::unavailable("knowledge worker is unavailable"));
        }
        self.guard(e).await?;
        let page = self.page(credentials, page_id).await?;
        if scalar(&page["id"]) != page_id {
            return Err(Error::new(
                StatusCode::BAD_GATEWAY,
                "Confluence page identity changed",
            ));
        }
        let body = ["storage", "view", "atlas_doc_format"]
            .iter()
            .find_map(|k| {
                let v = &page["body"][*k];
                v["value"].as_str().or(v.as_str()).filter(|s| !s.is_empty())
            })
            .unwrap_or("");
        let version = page["version"]["number"].as_i64().unwrap_or(1);
        let title = text(&page, "title");
        let hash = ingest::sha256(format!("{title}:{version}:{body}").as_bytes());
        let prior: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(p) FROM confluence_pages p WHERE binding_id=$1 AND page_id=$2",
        )
        .bind(text(&e.binding, "binding_id"))
        .bind(page_id)
        .fetch_optional(&self.pool)
        .await?;
        let sync_images = e.binding["sync_images"] == true;
        let attachments = if sync_images {
            self.attachments(e, credentials, page_id).await?
        } else {
            vec![]
        };
        let manifest = attachment_manifest(&attachments);
        let max_image = e.binding["image_max_size_bytes"]
            .as_u64()
            .filter(|n| *n > 0)
            .unwrap_or(3145728);
        let fingerprint = json!({"page_id":page_id,"page_version":version,"content_hash":hash,"attachments":manifest,"image_policy":{"max_image_size_bytes":max_image,"max_images_per_page":50}});
        let generation = format!(
            "confluence-{}",
            ingest::sha256(ingest::canonical_json(&fingerprint).as_bytes())
        );
        let id = prior
            .as_ref()
            .map(|p| text(p, "document_id"))
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_URL,
                    format!(
                        "ai-platform:confluence:{}:{}:{page_id}",
                        text(&e.binding, "dataset_id"),
                        text(&e.binding, "connection_id")
                    )
                    .as_bytes(),
                )
                .to_string()
            });
        if let Some(prior) = &prior {
            if text(prior, "content_hash") == hash {
                if !sync_images {
                    return Ok((id, false, false));
                }
                let doc: Option<Value> = sqlx::query_scalar(
                    "SELECT metadata FROM documents WHERE document_id=$1 AND dataset_id=$2",
                )
                .bind(&id)
                .bind(text(&e.binding, "dataset_id"))
                .fetch_optional(&self.pool)
                .await?;
                if doc
                    .as_ref()
                    .is_some_and(|m| m["_confluence_attachment_manifest"] == json!(manifest))
                {
                    return Ok((id, false, false));
                }
            }
        }
        self.guard(e).await?;
        let labels = self
            .provider(credentials, &["pages", page_id, "labels"], &[])
            .await
            .ok()
            .and_then(|v| v["results"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|v| json!(text(v, "name")))
            .collect::<Vec<_>>();
        let content = super::parser::extract(body, false);
        let markdown = super::parser::extract(body, true);
        let url = if text(&page["_links"], "webui").is_empty() {
            format!("confluence://{page_id}")
        } else {
            format!(
                "https://{}{}",
                credentials.domain,
                text(&page["_links"], "webui")
            )
        };
        let source_owner = if text(&e.binding, "binding_id").is_empty() {
            json!({"kind":"direct","id":format!("{}:{page_id}",text(&e.binding,"connection_id"))})
        } else {
            json!({"kind":"binding","id":e.binding["binding_id"]})
        };
        let source = json!({"source_owner":source_owner,"connection_id":e.binding["connection_id"],"page_id":page_id,"page_version":version,"content_hash":hash});
        let marker = json!({"generation":generation,"source":source,"started_at":chrono::Utc::now().to_rfc3339(),"status":"syncing","version":1});
        let store = &self.ingestion.store;
        let dataset = text(&e.binding, "dataset_id");
        let mut lease = store
            .document_lease(dataset, &id)
            .await
            .map_err(|_| Error::conflict("Confluence source document lease is busy"))?;
        let mut tx = lease.connection().begin().await?;
        self.guard_on(e, &mut tx).await?;
        let doc: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(d) FROM documents d WHERE document_id=$1")
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await?;
        let new = doc.is_none();
        if let Some(doc) = &doc {
            if text(doc, "dataset_id") != dataset || doc["source_type"] != "confluence" {
                return Err(Error::conflict(
                    "Deterministic Confluence document identity conflict",
                ));
            }
            let receipt = &doc["metadata"]["_confluence_image_source_generation"];
            if receipt["complete"] == true
                && receipt["sync_generation"] == generation
                && ["waiting", "parsing", "splitting", "indexing", "completed"]
                    .contains(&text(doc, "status"))
            {
                tx.commit().await?;
                lease
                    .release()
                    .await
                    .map_err(|_| Error::unavailable("Confluence source lease release failed"))?;
                return Ok((id, true, false));
            }
        } else {
            let mut initial_metadata = e.binding["extra_metadata"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            initial_metadata.insert("_confluence_sync_generation".into(), marker.clone());
            let initial = json!({"document_id":id,"dataset_id":dataset,"title":title,"source_type":"confluence","source_uri":url,"mime_type":"text/plain","size_bytes":content.len(),"status":"syncing","progress":0,"content":"","metadata":initial_metadata});
            ingest::store::KnowledgeWriteStore::insert_document_on(&mut tx, &initial)
                .await
                .map_err(|_| Error::unavailable("Confluence source document insertion failed"))?;
        }
        let claimed: Option<String> =
            sqlx::query_scalar(super::sql::BEGIN_CONFLUENCE_DOCUMENT_SYNC)
                .bind(&id)
                .bind(dataset)
                .bind(&generation)
                .bind(&marker)
                .bind(&source["source_owner"])
                .bind(3600i32)
                .fetch_optional(&mut *tx)
                .await?;
        if claimed.is_none() {
            return Err(Error::conflict(
                "Confluence document generation is owned by another source update",
            ));
        }
        tx.commit().await?;
        let prepared=async {
            let images=self.image_sources(e,credentials,&page,&id,&generation,&attachments,max_image,lease.connection()).await?;
            let mut metadata=json!({"confluence_page_id":page_id,"confluence_space_key":e.binding["space_key"],"confluence_version":version,"confluence_labels":labels,"confluence_author":page["authorId"],"confluence_updated_at":page["version"]["createdAt"],"markdown_content":markdown,"_confluence_attachment_manifest":manifest});
            for (key,value) in images.as_object().ok_or_else(||Error::unavailable("Confluence image source shape failed"))?{metadata[key]=value.clone();}
            let mut tx=lease.connection().begin().await?;self.guard_on(e,&mut tx).await?;
            let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM documents d WHERE document_id=$1 AND dataset_id=$2").bind(&id).bind(dataset).fetch_one(&mut *tx).await?;
            let previous=text(&current,"content");
            if !previous.is_empty()&&previous!=content {
                let next:i32=sqlx::query_scalar(ingest::sql::CREATE_DOCUMENT_VERSION_0).bind(&id).fetch_one(&mut *tx).await?;
                sqlx::query(ingest::sql::CREATE_DOCUMENT_VERSION_1).bind(uuid::Uuid::new_v4().to_string()).bind(&id).bind(next).bind(previous).bind(ingest::sha256(previous.as_bytes())).bind(current["confluence_version"].as_i64().map(|n|n as i32)).bind(text(&current,"updated_at").parse::<chrono::DateTime<chrono::Utc>>().ok().map(|v|v.naive_utc())).bind(text(&current,"title")).bind(&current["metadata"]).bind(previous.split_whitespace().count()as i32).bind("updated").bind(format!("Confluence sync: {title} (v{version})")).bind(&e.actor.user_id).execute(&mut *tx).await?;
                sqlx::query(ingest::sql::CREATE_DOCUMENT_VERSION_2).bind(next).bind(&id).execute(&mut *tx).await?;
            }
            let changed:Option<String>=sqlx::query_scalar(super::sql::PREPARE_CONFLUENCE_DOCUMENT_UPDATE).bind(&id).bind(dataset).bind(title).bind(&content).bind(version as i32).bind(&metadata).bind(&generation).fetch_optional(&mut *tx).await?;
            if changed.is_none(){return Err(Error::conflict("Confluence document generation changed during source replacement"));}
            if !text(&e.binding,"binding_id").is_empty() { sqlx::query("INSERT INTO confluence_pages(id,binding_id,document_id,page_id,space_key,title,version,content_hash,parent_page_id,status,labels,web_url,author,confluence_updated_at,image_count,last_synced_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'synced',$10,$11,$12,$13::text::timestamptz,$14,NOW()) ON CONFLICT(binding_id,page_id) DO UPDATE SET document_id=EXCLUDED.document_id,space_key=EXCLUDED.space_key,title=EXCLUDED.title,version=EXCLUDED.version,content_hash=EXCLUDED.content_hash,parent_page_id=EXCLUDED.parent_page_id,status='synced',labels=EXCLUDED.labels,web_url=EXCLUDED.web_url,author=EXCLUDED.author,confluence_updated_at=EXCLUDED.confluence_updated_at,image_count=EXCLUDED.image_count,last_synced_at=NOW(),updated_at=NOW()")
                .bind(format!("{}:{page_id}",text(&e.binding,"binding_id"))).bind(text(&e.binding,"binding_id")).bind(&id).bind(page_id).bind(text(&e.binding,"space_key")).bind(title).bind(version as i32).bind(&hash).bind(page["parentId"].as_str()).bind(json!(labels)).bind(&url).bind(page["authorId"].as_str()).bind(page["version"]["createdAt"].as_str()).bind(images["image_count"].as_i64().unwrap_or(0)as i32).execute(&mut *tx).await?; }
            tx.commit().await?;Ok::<_,Error>(())
        }.await;
        if prepared.is_err() {
            let mut tx = lease.connection().begin().await?;
            let _: Option<String> = sqlx::query_scalar(super::sql::ABORT_CONFLUENCE_DOCUMENT_SYNC)
                .bind(&id)
                .bind(dataset)
                .bind(&generation)
                .bind("Confluence source preparation failed")
                .fetch_optional(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        lease
            .release()
            .await
            .map_err(|_| Error::unavailable("Confluence source lease release failed"))?;
        prepared?;
        Ok((id, true, new))
    }
}
pub fn attachment_manifest(items: &[Value]) -> Vec<Value> {
    let mut out=items.iter().map(|item|json!({"attachment_id":scalar(&item["id"]),"filename":item["title"],"media_type":item["mediaType"],"file_size":item["fileSize"],"updated_at":item["version"]["createdAt"]})).collect::<Vec<_>>();
    out.sort_by(|a, b| {
        (text(a, "attachment_id"), text(a, "filename"))
            .cmp(&(text(b, "attachment_id"), text(b, "filename")))
    });
    out
}
