//! Attachment manifests and worker-rebuildable image source receipts.
use super::*;
use super::{client::scalar, sync::Execution};
use std::collections::BTreeSet;
impl Sources {
    pub async fn attachments(
        &self,
        e: &Execution,
        creds: &credentials::Credentials,
        id: &str,
    ) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut cursor = String::new();
        let mut seen = BTreeSet::new();
        loop {
            self.guard(e).await?;
            let mut params = vec![("limit", "50")];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.as_str()));
            }
            let data = match self
                .provider(creds, &["pages", id, "attachments"], &params)
                .await
            {
                Ok(data) => data,
                Err(e) if e.detail == "Confluence provider resource not found" => {
                    return Ok(vec![]);
                }
                Err(e) => return Err(e),
            };
            let rows = data["results"].as_array().ok_or_else(|| {
                Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Invalid attachment discovery response",
                )
            })?;
            out.extend(
                rows.iter()
                    .filter(|v| {
                        [
                            "image/png",
                            "image/jpeg",
                            "image/jpg",
                            "image/gif",
                            "image/bmp",
                            "image/webp",
                        ]
                        .contains(&text(v, "mediaType").to_lowercase().as_str())
                    })
                    .cloned(),
            );
            let next = text(&data["_links"], "next");
            if next.is_empty() || rows.len() < 50 {
                break;
            }
            let link = reqwest::Url::parse("https://pagination.invalid/")
                .unwrap()
                .join(next)
                .map_err(|_| {
                    Error::new(StatusCode::BAD_GATEWAY, "Invalid attachment pagination")
                })?;
            cursor = link
                .query_pairs()
                .find(|(k, _)| k == "cursor")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            if cursor.is_empty() || !seen.insert(cursor.clone()) {
                return Err(Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Incomplete attachment pagination",
                ));
            }
        }
        Ok(out)
    }
    pub async fn image_sources(
        &self,
        e: &Execution,
        creds: &credentials::Credentials,
        page: &Value,
        document: &str,
        generation: &str,
        attachments: &[Value],
        limit: u64,
        conn: &mut PgConnection,
    ) -> Result<Value> {
        self.guard_on(e, conn).await?;
        let tenant = &e.actor.tenant_id;
        // Validate scope using the canonical key builder before deleting any prefix.
        ingest::assets::AssetStorage::image_key(tenant, document, "scope", "probe")
            .map_err(|_| Error::forbidden())?;
        self.ingestion
            .assets
            .backend
            .delete_prefix(&format!("knowledge/confluence/{tenant}/{document}/images/"))
            .await
            .map_err(|_| Error::unavailable("Confluence image storage cleanup is unavailable"))?;
        let body = page["body"]["storage"]["value"].as_str().unwrap_or("");
        let mut contexts = super::parser::image_contexts(body);
        let fresh = if e.binding["sync_images"] == true {
            self.attachments(e, creds, &scalar(&page["id"])).await?
        } else {
            vec![]
        };
        let select = |items: &[Value]| {
            let mut items = items
                .iter()
                .filter(|v| v["fileSize"].as_u64().unwrap_or(0) <= limit)
                .cloned()
                .collect::<Vec<_>>();
            items.sort_by_key(|v| (scalar(&v["id"]), text(v, "title").to_owned()));
            items.truncate(50);
            super::publication::attachment_manifest(&items)
        };
        if select(&fresh) != select(attachments) {
            return Err(Error::conflict(
                "Confluence attachment manifest changed while source assets were prepared",
            ));
        }
        let mut selected = fresh
            .iter()
            .filter(|v| v["fileSize"].as_u64().unwrap_or(0) <= limit)
            .collect::<Vec<_>>();
        selected.sort_by_key(|v| (scalar(&v["id"]), text(v, "title").to_owned()));
        selected.truncate(50);
        let mut receipts = vec![];
        let mut skipped = vec![];
        for attachment in selected {
            self.guard_on(e, conn).await?;
            let link = text(attachment, "downloadLink");
            let relative = if link.starts_with("/wiki") {
                link.to_owned()
            } else {
                format!("/wiki{link}")
            };
            let url = if link.starts_with("https://") {
                reqwest::Url::parse(link)
            } else {
                reqwest::Url::parse(&format!("https://{}{}", creds.domain, relative))
            }
            .map_err(|_| Error::new(StatusCode::BAD_GATEWAY, "Invalid attachment download link"))?;
            if url.scheme() != "https"
                || url.host_str() != Some(creds.domain.as_str())
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Attachment download origin is unavailable",
                ));
            }
            let download_client = reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .https_only(true)
                .timeout(std::time::Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .map_err(|_| Error::unavailable("Attachment client unavailable"))?;
            let mut response = download_client
                .get(url)
                .basic_auth(&creds.email, Some(&creds.token))
                .send()
                .await
                .map_err(|_| Error::new(StatusCode::BAD_GATEWAY, "Attachment download failed"))?;
            if !response.status().is_success() {
                return Err(Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Attachment download failed",
                ));
            }
            let mut bytes = vec![];
            let mut oversized = false;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Error::new(StatusCode::BAD_GATEWAY, "Attachment download failed"))?
            {
                if bytes.len() as u64 + chunk.len() as u64 > limit {
                    oversized = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            let id = scalar(&attachment["id"]);
            let filename = text(attachment, "title");
            let mime = text(attachment, "mediaType");
            if bytes.is_empty() || oversized {
                let mut entry =
                    super::publication::attachment_manifest(&[attachment.clone()]).remove(0);
                entry["reason"] = json!("processing_skipped");
                skipped.push(entry);
                continue;
            }
            self.guard_on(e, conn).await?;
            let key = ingest::assets::AssetStorage::image_key(tenant, document, &id, filename)
                .map_err(|_| Error::new(StatusCode::BAD_GATEWAY, "Invalid attachment identity"))?;
            self.ingestion
                .assets
                .put(&key, &bytes, mime)
                .await
                .map_err(|_| Error::unavailable("Confluence image storage failed"))?;
            self.guard_on(e, conn).await?;
            let url = self
                .ingestion
                .assets
                .image_url(&key)
                .await
                .map_err(|_| Error::unavailable("Confluence image receipt unavailable"))?;
            let context = contexts
                .iter()
                .position(|(key, name, _)| (!key.is_empty() && key == &id) || name == filename)
                .map(|index| contexts.remove(index).2)
                .unwrap_or_default();
            let mut description = None;
            if let Some(vlm) = &self.image_vlm {
                self.guard_on(e, conn).await?;
                let title = text(page, "title");
                let vlm_context = match (title.is_empty(), context.is_empty()) {
                    (false, false) => format!("{title} - {context}"),
                    (false, true) => title.to_owned(),
                    _ => context.clone(),
                };
                if let Ok(Ok(result)) = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    vlm.describe_image(
                        &bytes,
                        None,
                        image_type(filename),
                        Some(&vlm_context),
                        1500,
                    ),
                )
                .await
                {
                    description = Some(result.description);
                }
                self.guard_on(e, conn).await?;
            }
            receipts.push(json!({"image_id":id,"filename":filename,"storage_url":url,"storage_key":key,"mime_type":mime,"size_bytes":bytes.len(),"context_text":context,"source_location":format!("confluence:{}",scalar(&page["id"])),"confluence_attachment_id":id,"attachment_updated_at":attachment["version"]["createdAt"],"vlm_description":description}));
        }
        let body = ["storage", "view", "atlas_doc_format"]
            .iter()
            .find_map(|k| {
                page["body"][*k]["value"]
                    .as_str()
                    .or(page["body"][*k].as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("");
        let version = page["version"]["number"].as_i64().unwrap_or(1);
        let hash = ingest::sha256(format!("{}:{version}:{body}", text(page, "title")).as_bytes());
        Ok(
            json!({"image_count":receipts.len(),"processing_mode":if receipts.is_empty(){"text"}else{"multimodal"},"extracted_images":receipts,"skipped_confluence_attachments":skipped,"_confluence_image_source_generation":{"page_id":scalar(&page["id"]),"page_version":version,"content_hash":hash,"sync_generation":generation,"complete":true}}),
        )
    }
}

fn image_type(filename: &str) -> &'static str {
    let filename = filename.to_lowercase();
    for (kind, keywords) in [
        (
            "table",
            &["table", "表格", "matrix", "grid", "fee", "price", "rate"][..],
        ),
        (
            "chart",
            &[
                "chart",
                "graph",
                "图表",
                "diagram",
                "flow",
                "架构",
                "架构图",
            ][..],
        ),
        (
            "diagram",
            &["diagram", "流程", "sequence", "uml", "erd"][..],
        ),
    ] {
        if keywords.iter().any(|word| filename.contains(word)) {
            return kind;
        }
    }
    "general"
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_hint_precedence_matches_source() {
        assert_eq!(image_type("PRICES-chart.png"), "table");
        assert_eq!(image_type("diagram.png"), "chart");
        assert_eq!(image_type("流程.png"), "diagram");
        assert_eq!(image_type("photo.png"), "general");
    }
}
