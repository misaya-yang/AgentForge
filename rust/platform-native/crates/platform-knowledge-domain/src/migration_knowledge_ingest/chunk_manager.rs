//! Canonical structured Chunk conversion/normalization and no-write previews.
use super::{
    Actor, Chunk, IngestError, IngestionService, Result,
    chunking::{Chunker, ChunkingConfig},
    object, sha256, string,
};
use md5::{Digest, Md5};
use serde_json::{Value, json};
fn hash_id(text: &str) -> String {
    format!("{:x}", Md5::digest(text.as_bytes()))[..12].into()
}
fn images(chunk: &Chunk) -> Vec<Value> {
    chunk
        .extra
        .get("associated_images")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
fn populate(chunk: &mut Chunk, chunker: &Chunker) {
    chunk.token_count = chunker.tokens.count(&chunk.text) as i32;
    chunk.word_count = chunk.text.split_whitespace().count() as i32;
    chunk
        .extra
        .insert("char_count".into(), json!(chunk.text.chars().count()));
    chunk
        .extra
        .insert("hash_id".into(), json!(hash_id(&chunk.text)));
}
impl Chunker {
    pub fn convert_structured_chunks(
        &self,
        input: &[Value],
        document: &str,
        name: &str,
        dataset: &str,
    ) -> Result<Vec<Chunk>> {
        if input.len() > super::chunking::MAX_CHUNK_OUTPUTS {
            return Err(IngestError::Validation(
                "structured parsing exceeds chunk output limit".into(),
            ));
        }
        let mut out = vec![];
        for (index, item) in input.iter().enumerate() {
            let kind = item["type"].as_str().unwrap_or("text");
            let text = item["text"]
                .as_str()
                .or(item["content"].as_str())
                .unwrap_or("");
            super::parser::require_text_budget(text)?;
            let mut metadata = object(&item["metadata"]);
            for (k, v) in [
                ("source_document", json!(name)),
                ("source_document_id", json!(document)),
                ("source_dataset_id", json!(dataset)),
                ("chunk_index", json!(index)),
                ("paragraph_index", json!(index)),
                ("structured_chunk_type", json!(kind)),
                (
                    "page_number",
                    item.get("page_number").cloned().unwrap_or(json!(0)),
                ),
                ("section_title", item["section_title"].clone()),
                (
                    "section_level",
                    item.get("section_level").cloned().unwrap_or(json!(0)),
                ),
            ] {
                metadata[k] = v;
            }
            if item["has_images"].as_bool() == Some(true)
                || item["images"].as_array().is_some_and(|v| !v.is_empty())
            {
                metadata["has_images"] = json!(true);
                metadata["image_count"] = json!(item["images"].as_array().map_or(0, Vec::len));
                metadata["associated_images"] = item.get("images").cloned().unwrap_or(json!([]));
            }
            let mut chunk = Chunk {
                dataset_id: dataset.into(),
                document_id: document.into(),
                segment_id: super::stable_segment_id(document, "text", index as i32),
                position: index as i32,
                text: text.into(),
                metadata,
                content_type: if ["image", "mixed"].contains(&kind) {
                    "mixed".into()
                } else {
                    "text".into()
                },
                ..Default::default()
            };
            populate(&mut chunk, self);
            out.push(chunk)
        }
        Ok(out)
    }
    pub fn normalize_structured_chunks(
        &self,
        input: Vec<Chunk>,
        config: &ChunkingConfig,
    ) -> Result<Vec<Chunk>> {
        fn emit(
            engine: &Chunker,
            out: &mut Vec<Chunk>,
            text: String,
            base: &Chunk,
            metadata: Value,
            associated: Vec<Value>,
            config: &ChunkingConfig,
        ) -> Result<()> {
            if text.trim().is_empty()
                || !text
                    .chars()
                    .any(|c| c.is_alphanumeric() || c == '_' || matches!(c as u32, 0x600..=0x6ff))
            {
                return Ok(());
            }
            let split = config.mode == "fixed_size"
                && ((config.use_token_count && engine.tokens.count(&text) > config.token_limit)
                    || (!config.use_token_count && text.chars().count() > config.max_chunk_size));
            if split {
                for part in engine.split(&text, config)? {
                    let mut row = base.clone();
                    row.text = part;
                    row.metadata = metadata.clone();
                    if row.metadata.get("structured_parent_id").is_none() {
                        row.metadata["structured_parent_id"] = json!(hash_id(&base.text))
                    }
                    row.content_type = "text".into();
                    row.extra
                        .insert("associated_images".into(), json!(associated));
                    populate(&mut row, engine);
                    out.push(row)
                }
            } else {
                let mut row = base.clone();
                row.text = text;
                row.metadata = metadata;
                row.content_type = "text".into();
                row.extra
                    .insert("associated_images".into(), json!(associated));
                populate(&mut row, engine);
                out.push(row)
            }
            if out.len() > super::chunking::MAX_CHUNK_OUTPUTS {
                return Err(IngestError::Validation(
                    "structured normalization exceeds chunk output limit".into(),
                ));
            }
            Ok(())
        }
        let mut normalized = vec![];
        let mut pending = String::new();
        let mut metadata = json!({});
        let mut associated = vec![];
        let mut base: Option<Chunk> = None;
        for row in input {
            if row.content_type != "text" {
                if !pending.is_empty() {
                    emit(
                        self,
                        &mut normalized,
                        std::mem::take(&mut pending),
                        base.as_ref().unwrap_or(&row),
                        std::mem::replace(&mut metadata, json!({})),
                        std::mem::take(&mut associated),
                        config,
                    )?;
                    base = None;
                }
                normalized.push(row);
                continue;
            }
            let text = row.text.trim();
            if text.is_empty() {
                continue;
            }
            let too_small = text.chars().count() < config.min_chunk_size
                || (config.mode == "fixed_size"
                    && config.use_token_count
                    && config
                        .min_chunk_tokens
                        .is_some_and(|n| self.tokens.count(text) < n));
            if too_small {
                if !pending.is_empty() {
                    pending.push_str("\n\n")
                }
                pending.push_str(text);
                metadata
                    .as_object_mut()
                    .unwrap()
                    .extend(object(&row.metadata).as_object().unwrap().clone());
                associated.extend(images(&row));
                if base.is_none() {
                    base = Some(row.clone())
                }
                if row.metadata["structured_chunk_type"] == "heading"
                    && metadata.get("section_title").is_none()
                {
                    metadata["section_title"] = json!(text)
                }
                continue;
            }
            let mut text = text.to_owned();
            let mut meta = object(&row.metadata);
            let mut imgs = images(&row);
            if !pending.is_empty() {
                text = format!("{pending}\n\n{text}");
                let mut combined = std::mem::replace(&mut metadata, json!({}));
                combined
                    .as_object_mut()
                    .unwrap()
                    .extend(meta.as_object().unwrap().clone());
                meta = combined;
                let mut combined = std::mem::take(&mut associated);
                combined.extend(imgs);
                imgs = combined;
                pending.clear();
                base = None;
            }
            emit(self, &mut normalized, text, &row, meta, imgs, config)?;
        }
        if !pending.is_empty() {
            if normalized
                .last()
                .is_some_and(|r: &Chunk| r.content_type == "text")
            {
                let last = normalized.pop().unwrap();
                let text = format!("{}\n\n{pending}", last.text);
                let mut meta = object(&last.metadata);
                meta.as_object_mut()
                    .unwrap()
                    .extend(metadata.as_object().unwrap().clone());
                let mut imgs = images(&last);
                imgs.extend(associated);
                emit(self, &mut normalized, text, &last, meta, imgs, config)?;
            } else {
                let row = base.unwrap_or_else(|| Chunk {
                    text: pending.clone(),
                    ..Default::default()
                });
                emit(
                    self,
                    &mut normalized,
                    pending,
                    &row,
                    metadata,
                    associated,
                    config,
                )?;
            }
        }
        if config.mode != "fixed_size" {
            normalized =
                self.merge_small(normalized, config.min_chunk_size, config.max_chunk_size)?
        }
        for (index, row) in normalized.iter_mut().enumerate() {
            row.position = index as i32;
            row.metadata["chunk_index"] = json!(index);
            row.metadata["paragraph_index"] = json!(index);
            populate(row, self);
        }
        Ok(normalized)
    }
}
impl IngestionService {
    pub async fn preview_chunking(
        &self,
        actor: &Actor,
        dataset: &str,
        text: &str,
        config: Option<&Value>,
    ) -> Result<Vec<Value>> {
        if let Some(config) = config {
            ChunkingConfig::from_value(config)?;
        }
        if dataset != "temp_preview" {
            self.store
                .require_dataset_access(actor, dataset, "viewer")
                .await?;
        }
        let selected =
            if let Some(config) = config.filter(|v| v.as_object().is_some_and(|m| !m.is_empty())) {
                config.clone()
            } else if dataset == "temp_preview" {
                json!({})
            } else {
                self.store.get_dataset(dataset).await?["index_config"]["chunking"].clone()
            };
        let config = ChunkingConfig::from_value(&selected)?;
        let id = format!(
            "preview_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let text = text.to_owned();
        let engine = self.chunker.clone();
        let dataset = dataset.to_owned();
        tokio::task::spawn_blocking(move||{let all=engine.process_document(&text,&config,&dataset,&id,"")?;let parents:std::collections::BTreeSet<_>=all.iter().filter_map(|r|r.parent_segment_id.clone()).collect();Ok(all.into_iter().filter(|r|!parents.contains(&r.segment_id)).map(|r|json!({"content":r.text,"token_count":r.token_count,"char_count":r.text.chars().count(),"metadata":r.metadata})).collect())}).await.map_err(|_|IngestError::Parser("chunk preview task failed".into()))?
    }
}
