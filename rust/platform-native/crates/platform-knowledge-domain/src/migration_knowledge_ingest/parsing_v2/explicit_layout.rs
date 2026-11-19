use super::*;
fn enabled(config: &Config) -> bool {
    config
        .layout
        .as_ref()
        .is_some_and(|layout| layout.mode == LayoutMode::Explicit)
}
fn failure(config: &Config, error: IngestError) -> Result<BTreeMap<usize, PageEvidence>> {
    if config
        .layout
        .as_ref()
        .is_some_and(|layout| layout.allow_known_failure_native_fallback)
    {
        Ok(BTreeMap::new())
    } else {
        Err(error)
    }
}
/// An explicitly selected server profile uses real native PDF raster output.
/// No measurements are created: the evidence carries `measurements: None`.
pub(super) async fn standard_evidence(
    service: &IngestionService,
    dataset: &Dataset,
    doc: &Value,
    native: &DocIR,
    config: &Config,
) -> Result<BTreeMap<usize, PageEvidence>> {
    if !enabled(config) {
        return Ok(BTreeMap::new());
    }
    let result = async {
        let profile = config
            .layout_profile()?
            .ok_or_else(|| invalid("explicit layout profile absent"))?;
        let key = doc["metadata"]["original_file_key"]
            .as_str()
            .ok_or_else(|| invalid("explicit layout requires original source"))?;
        if !key.starts_with(&format!(
            "knowledge/documents/{}/{}/original/",
            dataset.tenant_id, native.doc_id
        )) {
            return Err(IngestError::Permission);
        }
        let bytes = service.assets.download_original(key).await?;
        if ingest::sha256(&bytes) != native.content_hash {
            return Err(IngestError::GenerationChanged);
        }
        let backend = service
            .vision
            .as_ref()
            .ok_or_else(|| invalid("explicit layout requires existing raster backend"))?
            .backend
            .clone();
        let bytes = std::sync::Arc::new(bytes);
        let count = {
            let backend = backend.clone();
            let bytes = bytes.clone();
            tokio::task::spawn_blocking(move || backend.page_count(&bytes))
                .await
                .map_err(|_| invalid("explicit raster count task failed"))??
        };
        if count == 0 || count > profile.budget.max_pages || native.pages.len() != count {
            return Err(invalid("explicit raster/native page identities disagree"));
        }
        let mut result = BTreeMap::new();
        let mut total_bytes = 0usize;
        let started = std::time::Instant::now();
        for page in &native.pages {
            if page.page_number == 0
                || page.page_number > count
                || started.elapsed() >= profile.budget.deadline
            {
                return Err(invalid("explicit raster page/deadline budget exceeded"));
            }
            let backend = backend.clone();
            let bytes = bytes.clone();
            let index = page.page_number - 1;
            let rendered =
                tokio::task::spawn_blocking(move || backend.render(&bytes, index, Some(150)))
                    .await
                    .map_err(|_| invalid("explicit raster task failed"))??;
            if rendered.page_index != index || rendered.png.len() > profile.budget.max_image_bytes {
                return Err(invalid("explicit raster identity/image budget exceeded"));
            }
            total_bytes = total_bytes.saturating_add(rendered.png.len());
            if total_bytes > profile.budget.max_document_image_bytes {
                return Err(invalid("explicit document raster memory budget exceeded"));
            }
            result.insert(
                page.page_number,
                PageEvidence {
                    measurements: None,
                    image: Some(rendered.into()),
                },
            );
        }
        Ok(result)
    }
    .await;
    match result {
        Ok(evidence) => Ok(evidence),
        Err(error @ IngestError::GenerationChanged) => Err(error),
        Err(error @ IngestError::Permission) => Err(error),
        Err(error) => failure(config, error),
    }
}
/// Reuse the scanned candidate's already rendered, hash-verified owned crops.
pub(super) async fn vision_evidence(
    service: &IngestionService,
    plan: &ingest::hierarchy::CandidatePlan,
    config: &Config,
) -> Result<BTreeMap<usize, PageEvidence>> {
    if !enabled(config) {
        return Ok(BTreeMap::new());
    }
    let result = async {
        let profile = config
            .layout_profile()?
            .ok_or_else(|| invalid("explicit layout profile absent"))?;
        let mut result = BTreeMap::new();
        let mut total_bytes = 0usize;
        for row in plan
            .segment_rows
            .iter()
            .filter(|row| row.content_type == "image")
        {
            let page = row.metadata["page_number"]
                .as_u64()
                .ok_or_else(|| invalid("vision page identity absent"))?
                as usize;
            let receipt = plan
                .object_manifest
                .iter()
                .find(|receipt| {
                    receipt["attachment_id"] == row.image_attachment_id.as_deref().unwrap_or("")
                })
                .ok_or_else(|| invalid("vision crop receipt absent"))?;
            let key = receipt["storage_key"]
                .as_str()
                .ok_or_else(|| invalid("vision crop key absent"))?;
            let png = service
                .assets
                .download_bounded(key, profile.budget.max_image_bytes)
                .await?;
            total_bytes = total_bytes.saturating_add(png.len());
            if total_bytes > profile.budget.max_document_image_bytes {
                return Err(invalid("explicit vision raster memory budget exceeded"));
            }
            if receipt["sha256"] != ingest::sha256(&png) {
                return Err(IngestError::GenerationChanged);
            }
            let width = row.metadata["width"]
                .as_u64()
                .ok_or_else(|| invalid("vision width absent"))? as usize;
            let height = row.metadata["height"]
                .as_u64()
                .ok_or_else(|| invalid("vision height absent"))? as usize;
            if result
                .insert(
                    page,
                    PageEvidence {
                        measurements: None,
                        image: Some(PageImage { png, width, height }),
                    },
                )
                .is_some()
            {
                return Err(invalid("duplicate vision raster page"));
            }
        }
        Ok(result)
    }
    .await;
    match result {
        Ok(evidence) => Ok(evidence),
        Err(error @ IngestError::GenerationChanged) => Err(error),
        Err(error @ IngestError::Permission) => Err(error),
        Err(error) => failure(config, error),
    }
}
