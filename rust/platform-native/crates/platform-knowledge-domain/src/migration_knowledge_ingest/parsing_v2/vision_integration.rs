use super::*;
/// Preserve the existing vision candidate, including every image point and crop.
/// Add opt-in text/table/formula chunks to that same fenced candidate generation.
pub async fn prepare_vision(
    service: &IngestionService,
    lease: &mut DocumentLease,
    dataset: &Dataset,
    doc: &Value,
    execution: &str,
    plan: &mut ingest::hierarchy::CandidatePlan,
    config: &Config,
) -> Result<Prepared> {
    let objects = ingest::special::object_plan(plan)?;
    let page_texts = ingest::special::vision_page_texts(plan, &objects)?;
    let pages = page_texts
        .iter()
        .map(|page| {
            Ok(ingest::streaming::PageContent::new(
                page["page_number"]
                    .as_u64()
                    .ok_or_else(|| invalid("vision page identity absent"))?
                    as usize,
                page["text"]
                    .as_str()
                    .ok_or_else(|| invalid("vision text absent"))?
                    .into(),
                vec![],
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut native = streaming_ir(
        &plan.document_id,
        &plan.source_hash,
        ingest::string(doc, "title"),
        ingest::string(doc, "mime_type"),
        pages,
    )?;
    for page in &mut native.pages {
        let image = plan.segment_rows.iter().find(|row| {
            row.content_type == "image" && row.metadata["page_number"] == page.page_number
        });
        if let Some(image) = image {
            let width = image.metadata["width"]
                .as_u64()
                .ok_or_else(|| invalid("vision width absent"))?;
            let height = image.metadata["height"]
                .as_u64()
                .ok_or_else(|| invalid("vision height absent"))?;
            let key = page_texts
                .iter()
                .find(|p| p["page_number"] == page.page_number)
                .and_then(|p| p["storage_key"].as_str())
                .ok_or_else(|| invalid("vision owned crop absent"))?;
            page.page_width = Some(width as f64);
            page.page_height = Some(height as f64);
            page.metadata["coordinate_space"] = json!("render_pixels");
            page.blocks.push(Block {
                block_id: format!("page-{}-figure", page.page_number),
                kind: "figure".into(),
                text: String::new(),
                order: page.blocks.len(),
                bbox: Some(json!([0, 0, width, height])),
                parser: "native-vision-render".into(),
                parser_version: "existing-backend/v1".into(),
                confidence: None,
                table: None,
                formula: None,
                figure: Some(json!({"caption":"","crop":key})),
                metadata: json!({"source_image_segment":image.segment_id}),
            });
        }
    }
    let evidence = super::explicit_layout::vision_evidence(service, plan, config).await?;
    let prepared = prepare(
        service, lease, dataset, doc, execution, native, &evidence, config,
    )
    .await?;
    let chunking = ChunkingConfig::from_value(&dataset.index_config["chunking"])?;
    let mut chunks = prepared.chunks(
        &service.chunker,
        &chunking,
        ingest::string(doc, "title"),
        ingest::string(doc, "source_type"),
    )?;
    for chunk in &mut chunks {
        chunk.segment_id = ingest::hierarchy::candidate_id(
            &dataset.dataset_id,
            &plan.document_id,
            &plan.generation_id,
            3,
            chunk.position,
        );
        chunk.vector_id = Some(chunk.segment_id.clone());
        chunk.enabled = false;
        chunk.status = "indexing".into();
    }
    let points = service.indexer.prepare_chunks(dataset, &chunks).await?;
    for (collection, mut points) in points {
        plan.points_by_collection
            .entry(collection)
            .or_default()
            .append(&mut points);
    }
    plan.segment_ids
        .extend(chunks.iter().map(|chunk| chunk.segment_id.clone()));
    plan.l3_count += chunks.len();
    plan.segment_rows.extend(chunks);
    Ok(prepared)
}
