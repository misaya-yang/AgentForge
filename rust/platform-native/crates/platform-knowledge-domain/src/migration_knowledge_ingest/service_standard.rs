//! Standard worker ingestion, preserving the existing publication authority.
use super::*;
impl IngestionService {
    pub(super) async fn ingest_standard(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        identity: &str,
        index: &Value,
        processing_mode: &str,
        action: &str,
        execution: &str,
    ) -> Result<Vec<String>> {
        let document_id = string(doc, "document_id");
        let mut metadata = object(&doc["metadata"]);
        if metadata.get("structured_parsing").is_some() {
            return Err(IngestError::Validation(
                "structured parsing is disabled until a trusted source receipt exists".into(),
            ));
        }
        if let Some(restored) = metadata.get("_document_restored_source_version") {
            let number = restored
                .as_i64()
                .filter(|n| *n > 0 && *n == integer(doc, "current_version"))
                .ok_or(IngestError::GenerationChanged)?;
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.document_versions WHERE document_id=$1 AND version_number=$2 AND change_type='restored' AND content=$3)").bind(document_id).bind(number as i32).bind(string(doc,"content")).fetch_one(crate::database::connection(lease.connection())).await?;
            if !valid {
                return Err(IngestError::GenerationChanged);
            }
        }
        if let Some(checkpoint) = self
            .load_pipeline_checkpoint::<crate::migration_knowledge_ingest::pipeline::SpecialParsed>(
                "special_parsed",
            )
            .await?
        {
            if checkpoint.schema != "knowledge-special-parsed/v1"
                || checkpoint.identity != identity
                || checkpoint.execution != execution
                || checkpoint.source_sha256
                    != crate::migration_knowledge_ingest::pipeline::source_identity(
                        dataset, doc, index,
                    )
            {
                return Err(IngestError::GenerationChanged);
            }
            return crate::migration_knowledge_ingest::special::SpecialPublicationCoordinator {
                service: self.clone(),
            }
            .publish_with_docir(
                checkpoint.plan,
                dataset,
                execution,
                &checkpoint.expected_content,
                &checkpoint.patch,
                checkpoint.docir.as_ref(),
            )
            .await;
        }
        let old =
            KnowledgeWriteStore::segments_on(lease.connection(), &dataset.dataset_id, document_id)
                .await?;
        let repair_rows: Vec<_> = old
            .iter()
            .filter(|r| {
                r.content_type == "text"
                    && (r.status == "indexing" || (r.status == "completed" && r.enabled))
            })
            .cloned()
            .collect();
        if let Some(mut checkpoint) = self
            .load_pipeline_checkpoint::<crate::migration_knowledge_ingest::pipeline::Parsed>(
                "parsed",
            )
            .await?
        {
            if checkpoint.schema != "knowledge-parsed/v1"
                || checkpoint.identity != identity
                || checkpoint.execution != execution
                || checkpoint.source_sha256
                    != crate::migration_knowledge_ingest::pipeline::source_identity(
                        dataset, doc, index,
                    )
            {
                return Err(IngestError::GenerationChanged);
            }
            return self
                .publish_incremental(
                    lease,
                    dataset,
                    doc,
                    identity,
                    &checkpoint.content,
                    &checkpoint.patch,
                    checkpoint.pending.as_ref(),
                    &checkpoint.old,
                    &mut checkpoint.chunks,
                    checkpoint.reembed,
                    checkpoint.image_points,
                    checkpoint.replace_images,
                    checkpoint.docir.as_ref(),
                )
                .await;
        }
        if matches!(
            crate::migration_knowledge_ingest::pipeline::stage(),
            crate::migration_knowledge_ingest::pipeline::Stage::Embed
                | crate::migration_knowledge_ingest::pipeline::Stage::Index
        ) {
            return Err(IngestError::GenerationChanged);
        }
        let action = if action == "reembed" && repair_rows.is_empty() {
            "ingest"
        } else {
            action
        };

        let mut effective_mode = processing_mode.to_owned();
        if effective_mode == "auto" && action != "reembed" {
            if let Some(resolved) = metadata["processing_mode"]
                .as_str()
                .filter(|m| ["text_only", "scanned", "multimodal"].contains(m))
            {
                effective_mode = resolved.into();
            }
        }

        if effective_mode == "auto" && action != "reembed" {
            if let Some(key) = metadata["original_file_key"].as_str() {
                let detection = if integer(doc, "size_bytes") > self.large_file_threshold as i64 {
                    self.detect_large_pdf(key).await
                } else {
                    let bytes = self.assets.download_original(key).await?;
                    let samples = if self.image_extractor.pdf.is_some() {
                        self.image_extractor.sample_pdf(&bytes).await.ok()
                    } else {
                        None
                    };
                    crate::migration_knowledge_ingest::detection::DocumentTypeDetector::default()
                        .detect(
                            &bytes,
                            string(doc, "title"),
                            string(doc, "mime_type"),
                            samples
                                .as_ref()
                                .map(|(count, pages)| (*count, pages.as_slice())),
                        )
                };
                let (mode, receipt) = match detection {
                    Ok(result) => (
                        result.recommended_mode.as_str().to_owned(),
                        serde_json::to_value(result)?,
                    ),
                    Err(_) => (
                        "text_only".into(),
                        json!({"fallback":true,"reason":"detection_failed"}),
                    ),
                };
                let changed=sqlx::query("UPDATE knowledge.documents SET detection_result=$3::jsonb,metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{processing_mode}',to_jsonb($4::text),TRUE),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2 AND status='parsing' AND COALESCE(metadata->>'processing_mode','text_only')='auto'").bind(document_id).bind(&dataset.dataset_id).bind(receipt).bind(&mode).execute(crate::database::connection(lease.connection())).await?.rows_affected();
                if changed != 1 {
                    return Err(IngestError::GenerationChanged);
                }
                effective_mode = mode;
            } else {
                effective_mode = "text_only".into()
            }
        }
        let parsing_config = crate::migration_knowledge_ingest::structured::resolve_config(index)?;
        let v2_config =
            crate::migration_knowledge_ingest::parsing_v2::integration::Config::resolve(index)?;
        let mut rich_ir = None;
        let mut prepared_v2 = None;
        let pending_owned = metadata
            .get("_document_pending_restore_version")
            .filter(|v| v.is_object())
            .cloned();
        let pending = pending_owned.as_ref();
        let mut content = string(doc, "content").to_owned();
        let mut patch = json!({});
        if let Some(pending) = pending {
            let number = integer(pending, "candidate_version");
            if integer(pending, "previous_version") <= 0
                || number <= integer(pending, "previous_version")
            {
                return Err(IngestError::GenerationChanged);
            }
            let target:Value=sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2 AND change_type='pending_restore'").bind(document_id).bind(number as i32).fetch_optional(crate::database::connection(lease.connection())).await?.ok_or(IngestError::GenerationChanged)?;
            content = string(&target, "content").to_owned();
            if target["metadata"]["_special_source_manifest"].is_object() {
                return self
                    .ingest_special_restore(lease, dataset, doc, index, execution, &target)
                    .await;
            }
        }

        if effective_mode == "scanned" && action != "reembed" {
            return self
                .ingest_vision(lease, dataset, doc, index, execution, None)
                .await;
        }
        let reembed = action == "reembed";
        let streamed = !reembed
            && pending.is_none()
            && effective_mode != "multimodal"
            && integer(doc, "size_bytes") > self.large_file_threshold as i64;
        if streamed {
            let result = self
                .extract_large_file(lease, dataset, doc, v2_config.is_some())
                .await?;
            rich_ir = result.native_ir;
            content = result.text;
            patch["total_pages"] = json!(result.total_pages);
            patch["streaming_processed"] = json!(true);
        }
        let mut chunks = if reembed {
            repair_rows
        } else {
            KnowledgeWriteStore::update_status(
                lease.connection(),
                document_id,
                "parsing",
                Some(10.0),
                None,
            )
            .await?;
            if !streamed
                && pending.is_none()
                && metadata.get("_document_restored_source_version").is_none()
                && content.trim().chars().count() < self.reextraction_min_chars
                && metadata["original_file_key"]
                    .as_str()
                    .is_some_and(|key| !key.is_empty())
            {
                let mut receipt_failed = false;
                let reextracted: Result<String> = async {
                    let key = string(&metadata, "original_file_key");
                    let source = Arc::new(self.assets.download_original(key).await?);
                    let filename = string(&metadata, "original_filename").to_owned();
                    let mime = string(&metadata, "original_mime_type").to_owned();
                    let used_multimodal = ["multimodal", "scanned"]
                        .contains(&effective_mode.as_str())
                        && self.multimodal_reextraction_enabled;
                    let mut text = if used_multimodal {
                        let result = self
                            .image_extractor
                            .extract(&filename, &source, None)
                            .await?;
                        let images: Vec<_> = result
                            .images
                            .into_iter()
                            .filter(|i| i.embeddable())
                            .collect();
                        if !images.is_empty()
                            && metadata["extracted_images"]
                                .as_array()
                                .is_none_or(|v| v.is_empty())
                        {
                            let mut receipt_doc = doc.clone();
                            receipt_doc["metadata"] = metadata.clone();
                            // Every persistence error is fatal, even if its underlying
                            // classification is storage or a missing object.
                            receipt_failed = true;
                            metadata = self
                                .persist_image_receipt(
                                    lease,
                                    dataset,
                                    &receipt_doc,
                                    &effective_mode,
                                    &images,
                                )
                                .await?;
                            receipt_failed = false;
                        }
                        result.text
                    } else {
                        let parser = self.parser.clone();
                        let permit = self
                            .cpu
                            .clone()
                            .acquire_owned()
                            .await
                            .map_err(|_| IngestError::Cancelled)?;
                        let source = source.clone();
                        let name = filename.clone();
                        let content_type = mime.clone();
                        let (text, _) = tokio::task::spawn_blocking(move || {
                            let _permit = permit;
                            parser.extract_text_from_bytes(&source, &name, &content_type)
                        })
                        .await
                        .map_err(|error| {
                            if error.is_cancelled() {
                                IngestError::Cancelled
                            } else {
                                IngestError::Storage
                            }
                        })??;
                        text
                    };
                    if crate::migration_knowledge_ingest::reextraction::needs_legacy_pdf_ocr(
                        used_multimodal,
                        &text,
                        &filename,
                        &mime,
                        self.streaming_ocr_enabled,
                        self.reextraction_min_chars,
                    ) {
                        // Canonical whole-PDF OCR deliberately rejects in this release.
                        // Reuse that boundary instead of silently indexing short text.
                        let parser = self.parser.clone();
                        let ocr =
                            tokio::task::spawn_blocking(move || parser.ocr_pdf_bytes(&source))
                                .await
                                .map_err(|error| {
                                    if error.is_cancelled() {
                                        IngestError::Cancelled
                                    } else {
                                        IngestError::Storage
                                    }
                                })??;
                        if !ocr.trim().is_empty() {
                            text = ocr;
                        }
                    }
                    Ok(text)
                }
                .await;
                if let Some(text) = crate::migration_knowledge_ingest::reextraction::settle(
                    reextracted,
                    receipt_failed,
                    &content,
                )? {
                    content = text;
                }
                // Source does not mark ocr_processed here. The replacement text
                // stays private until publish_incremental's atomic activation.
            }
            require_text_budget(&content)?;
            let has_images = ["multimodal", "scanned"].contains(&effective_mode.as_str())
                && metadata["extracted_images"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty());
            if content.trim().is_empty() && !has_images {
                return Err(IngestError::Validation(
                    "document has no text to index".into(),
                ));
            }
            if let Some(config) = parsing_config {
                let hash = sha256(content.as_bytes());
                let config_hash =
                    sha256(canonical_json(&serde_json::to_value(&config)?).as_bytes());
                let cache = crate::migration_knowledge_ingest::structured::PostgresPageCache {
                    pool: self.store.pool.clone(),
                    tenant_id: dataset.tenant_id.clone(),
                    dataset_id: dataset.dataset_id.clone(),
                    document_id: document_id.into(),
                    generation_key: hash.clone(),
                    parser_config_hash: config_hash.clone(),
                    page_content_hashes: BTreeMap::from([(1, hash.clone())]),
                };
                let mut cascade =
                    crate::migration_knowledge_ingest::structured::ParserCascade::new(
                        config.clone(),
                        self.structured_backends.clone(),
                    )?;
                cascade.persistent = Some(cache.clone());
                let ir = if let Some(cached) = cache.load_ir(&cascade.bundle_version()).await? {
                    cached
                } else {
                    let mut job = crate::migration_knowledge_ingest::structured::PageJob::text(
                        document_id,
                        &content,
                        Some(string(doc, "mime_type").into()),
                    );
                    job.options["parser_config_hash"] = json!(config_hash);
                    let ir = cascade
                        .parse_document(
                            document_id,
                            vec![job],
                            &hash,
                            string(doc, "title"),
                            Some(string(doc, "mime_type").into()),
                        )
                        .await?;
                    cache
                        .store_ir(&ir, &config, &json!({"page_count":ir.pages.len()}))
                        .await?;
                    ir
                };
                content = crate::migration_knowledge_ingest::structured::render_document_markdown(
                    &ir, false,
                );
                if rich_ir.is_none() {
                    rich_ir = Some(ir.clone());
                }
                patch["parsing_ir"] = json!({"generation_key":hash,"parser_bundle":cascade.bundle_version(),"parser_config_hash":config_hash});
            }

            KnowledgeWriteStore::update_status(
                lease.connection(),
                document_id,
                "splitting",
                Some(30.0),
                None,
            )
            .await?;
            let config = ChunkingConfig::from_value(&index["chunking"])?;
            if let Some(profile) = &v2_config {
                prepared_v2 = Some(
                    crate::migration_knowledge_ingest::parsing_v2::integration::prepare_standard(
                        self,
                        lease,
                        dataset,
                        doc,
                        execution,
                        &content,
                        rich_ir.take(),
                        profile,
                    )
                    .await?,
                );
            }
            if index["chunking"]["mode"]
                .as_str()
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("hierarchical"))
            {
                return self
                    .ingest_hierarchy(
                        lease,
                        dataset,
                        doc,
                        index,
                        execution,
                        &content,
                        None,
                        Some(&patch),
                    )
                    .await;
            }
            if let Some(prepared) = &prepared_v2 {
                prepared.chunks(
                    &self.chunker,
                    &config,
                    string(doc, "title"),
                    string(doc, "source_type"),
                )?
            } else {
                let worker = self.chunker.clone();
                let text = content.clone();
                let ds = dataset.dataset_id.clone();
                let id = document_id.to_owned();
                let name = string(doc, "title").to_owned();
                let source = string(doc, "source_type").to_owned();
                let permit = self
                    .cpu
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| IngestError::Cancelled)?;
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    worker.standard_chunks(&text, &config, &ds, &id, &name, &source)
                })
                .await
                .map_err(|_| IngestError::Parser("document chunking task failed".into()))??
            }
        };
        let mut image_points = BTreeMap::new();
        let mut replace_images = false;
        if !reembed && pending.is_none() {
            let receipts = metadata
                .get("extracted_images")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let receipts = receipts.as_array().ok_or_else(|| {
                IngestError::Validation("durable image receipt must be a list".into())
            })?;
            let required = ["multimodal", "scanned"].contains(&effective_mode.as_str())
                && (!receipts.is_empty() || integer(&metadata, "image_count") > 0);
            if required && receipts.is_empty() {
                return Err(IngestError::Validation(
                    "document declares images without a durable rebuild receipt".into(),
                ));
            }
            let image_provider = dataset.embedding_provider.as_deref().unwrap_or("local");
            let unified = [
                "unified_multimodal",
                "unified",
                "cross_modal",
                "dashscope_multimodal",
                "aliyun_multimodal",
                "multimodal",
            ]
            .contains(&image_provider);
            if required && !unified {
                if content.trim().is_empty() {
                    return Err(IngestError::Validation(
                        "image-only documents require a unified multimodal dataset".into(),
                    ));
                }
                patch["image_indexing"] = json!({"status":"skipped","reason":"text_only_dataset","receipt_count":receipts.len(),"indexed_count":0});
            }
            if required && unified && metadata["images_embedded"].as_bool() != Some(true) {
                let position = chunks
                    .iter()
                    .chain(old.iter().filter(|r| r.content_type == "text"))
                    .map(|r| r.position)
                    .max()
                    .unwrap_or(-1)
                    + 1;
                let (rows, points) = self
                    .prepare_receipt_images(dataset, document_id, receipts, &old, position)
                    .await?;
                chunks.extend(rows);
                image_points = points;
                replace_images = true;
            }
        }
        if crate::migration_knowledge_ingest::pipeline::stage()
            == crate::migration_knowledge_ingest::pipeline::Stage::Parse
        {
            let checkpoint = crate::migration_knowledge_ingest::pipeline::Parsed {
                schema: "knowledge-parsed/v1".into(),
                identity: identity.into(),
                execution: execution.into(),
                source_sha256: crate::migration_knowledge_ingest::pipeline::source_identity(
                    dataset, doc, index,
                ),
                content: content.clone(),
                patch: patch.clone(),
                pending: pending.cloned(),
                old: old.clone(),
                chunks: chunks.clone(),
                reembed,
                image_points: image_points.clone(),
                replace_images,
                docir: prepared_v2.clone(),
            };
            self.pipeline_checkpoint(
                "parsed",
                &checkpoint,
                Some(crate::migration_knowledge_ingest::pipeline::Stage::Embed),
                execution,
            )
            .await?;
        }
        self.publish_incremental(
            lease,
            dataset,
            doc,
            identity,
            &content,
            &patch,
            pending,
            &old,
            &mut chunks,
            reembed,
            image_points,
            replace_images,
            prepared_v2.as_ref(),
        )
        .await
    }
}
