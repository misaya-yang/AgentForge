//! Concrete worker/publication seam. Kept as a child until primary mounts it.
use super::*;
use crate::migration_knowledge_ingest::{
    self as ingest, Chunk, Dataset, IngestionService,
    assets::AssetStorage,
    chunking::ChunkingConfig,
    parser::{Block, DocIR, PageIR},
    store::DocumentLease,
};
use crate::migration_knowledge_query::{model_plane, types::UserContext};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u8,
    pub profile_id: String,
    pub atomic_chunks: bool,
    #[serde(default)]
    pub layout: Option<LayoutConfig>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    #[serde(default)]
    pub mode: LayoutMode,
    pub provider: String,
    pub model: String,
    pub revision: String,
    #[serde(default)]
    pub min_text_coverage: Option<f64>,
    #[serde(default)]
    pub image_ratio_threshold: Option<f64>,
    #[serde(default)]
    pub table_density_threshold: Option<f64>,
    pub max_output_tokens: u64,
    pub deadline_seconds: u64,
    pub allow_known_failure_native_fallback: bool,
}
impl Config {
    pub fn resolve(index: &Value) -> Result<Option<Self>> {
        let Some(value) = index.get("parsing_v2").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let config: Self = serde_json::from_value(value.clone())
            .map_err(|_| invalid("invalid parsing_v2 configuration"))?;
        if config.version != 2 || config.profile_id.trim().is_empty() || !config.atomic_chunks {
            return Err(invalid(
                "parsing_v2 requires version=2, profile_id and atomic_chunks=true",
            ));
        }
        if config.layout.as_ref().is_some_and(|layout| {
            layout.mode == LayoutMode::Thresholds
                && (layout.min_text_coverage.is_none()
                    || layout.image_ratio_threshold.is_none()
                    || layout.table_density_threshold.is_none())
        }) {
            return Err(invalid("threshold layout requires all policy thresholds"));
        }
        if let Some(profile) = config.layout_profile()? {
            profile.validate()?;
        }
        Ok(Some(config))
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(ingest::sha256(
            ingest::canonical_json(&serde_json::to_value(self)?).as_bytes(),
        ))
    }
    fn layout_profile(&self) -> Result<Option<LayoutProfile>> {
        self.layout
            .as_ref()
            .map(|layout| {
                Ok(LayoutProfile {
                    mode: layout.mode,
                    fingerprint: self.fingerprint()?,
                    provider: layout.provider.clone(),
                    model: layout.model.clone(),
                    revision: layout.revision.clone(),
                    min_text_coverage: layout.min_text_coverage.unwrap_or(0.7),
                    image_ratio_threshold: layout.image_ratio_threshold.unwrap_or(0.8),
                    table_density_threshold: layout.table_density_threshold.unwrap_or(0.4),
                    allow_known_failure_native_fallback: layout.allow_known_failure_native_fallback,
                    budget: Budgets {
                        max_output_tokens: layout.max_output_tokens,
                        deadline: Duration::from_secs(layout.deadline_seconds),
                        ..Default::default()
                    },
                })
            })
            .transpose()
    }
}
/// Cannot be constructed by a caller from unverified request metadata.
pub struct Prepared {
    scope: ProcessingScope,
    pages: Vec<PageV2>,
    native: DocIR,
    actor: UserContext,
    expected_content: String,
    pending_prefix: String,
}
pub struct Published {
    pub patch: Value,
    pub document: DocV2,
}
fn actor_matches(actor: &UserContext, tenant: &str, user: &str) -> Result<()> {
    if actor.anonymous() || actor.tenant_id != tenant || actor.user_id != user {
        return Err(IngestError::Permission);
    }
    Ok(())
}
async fn authority(
    conn: &mut PgConnection,
    dataset: &str,
    document: &str,
    execution: &str,
    expected_content: &str,
) -> Result<(String, String)> {
    let row:Value=sqlx::query_scalar("SELECT jsonb_build_object('tenant',ds.tenant_id,'actor',COALESCE(e.triggered_by,d.created_by)) FROM knowledge.documents d JOIN knowledge.datasets ds ON ds.dataset_id=d.dataset_id JOIN knowledge.document_pipeline_executions e ON e.document_id=d.document_id AND e.dataset_id=d.dataset_id WHERE d.document_id=$1 AND d.dataset_id=$2 AND e.execution_id=$3 AND e.status='running' AND d.metadata->>'_document_pipeline_execution_id'=e.execution_id AND d.content=$4 AND ds.is_deleted=FALSE")
        .bind(document).bind(dataset).bind(execution).bind(expected_content).fetch_optional(conn).await?.ok_or(IngestError::GenerationChanged)?;
    let tenant = row["tenant"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(IngestError::Permission)?;
    let user = row["actor"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(IngestError::Permission)?;
    Ok((tenant.into(), user.into()))
}
fn prefix(tenant: &str, document: &str, execution: &str) -> String {
    format!(
        "knowledge/docir-v2/{}/{}/{}/",
        ingest::sha256(tenant.as_bytes()),
        ingest::sha256(document.as_bytes()),
        ingest::sha256(execution.as_bytes())
    )
}
/// Actual streaming page outputs, retaining their page numbers. No geometry or
/// table structure is invented by text-only decoders; richer existing IR wins.
pub fn streaming_ir(
    document: &str,
    source_hash: &str,
    filename: &str,
    mime: &str,
    pages: Vec<ingest::streaming::PageContent>,
) -> Result<DocIR> {
    let mut ids = BTreeSet::new();
    let mut result = Vec::new();
    for page in pages {
        if page.page_number == 0 || !ids.insert(page.page_number) {
            return Err(invalid("invalid streaming page identity"));
        }
        ingest::parser::require_text_budget(&page.text)?;
        result.push(PageIR{page_number:page.page_number,blocks:ingest::structured::markdown_to_blocks(&page.text,"native-streaming","existing-backend/v1")?,parser:"native-streaming".into(),parser_version:"existing-backend/v1".into(),page_width:None,page_height:None,confidence:None,hard_page:false,metadata:json!({"signals_missing":["region_coverage","image_area_ratio","table_area_density"]})});
    }
    Ok(DocIR {
        doc_id: document.into(),
        content_hash: source_hash.into(),
        filename: filename.into(),
        mime: Some(mime.into()),
        schema_version: "1".into(),
        pages: result,
        metadata: json!({"source":"existing_streaming_backend"}),
    })
}
/// Run after obtaining the existing real native/structured/streaming IR. Persist
/// an orphan-recovery prefix before object writes; this is not an active IR ref.
pub async fn prepare(
    service: &IngestionService,
    lease: &mut DocumentLease,
    dataset: &Dataset,
    doc: &Value,
    execution: &str,
    native: DocIR,
    evidence: &BTreeMap<usize, PageEvidence>,
    config: &Config,
) -> Result<Prepared> {
    let document = ingest::string(doc, "document_id");
    let expected = ingest::string(doc, "content");
    if lease.document_id != document
        || lease.dataset_id != dataset.dataset_id
        || native.doc_id != document
        || native.content_hash.len() != 64
        || !native.content_hash.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(IngestError::GenerationChanged);
    }
    let (tenant, user) = authority(
        lease.connection(),
        &dataset.dataset_id,
        document,
        execution,
        expected,
    )
    .await?;
    if tenant != dataset.tenant_id {
        return Err(IngestError::Permission);
    }
    let actor = model_plane::verified_actor_scope().ok_or(IngestError::Permission)?;
    actor_matches(&actor, &tenant, &user)?;
    // The worker's actor was refreshed before admission. Refresh again before a
    // potentially paid layout dispatch, using the owning actor authority.
    let actor = service
        .vector_job_actor_authority
        .as_ref()
        .ok_or(IngestError::Permission)?
        .resolve(&tenant, &user)
        .await?;
    actor_matches(&actor, &tenant, &user)?;
    let scope = ProcessingScope {
        tenant_id: tenant.clone(),
        dataset_id: dataset.dataset_id.clone(),
        document_id: document.into(),
        execution_id: Some(execution.into()),
        parser_profile: config.fingerprint()?,
    };
    let layout = config.layout_profile()?;
    if layout.as_ref().is_some_and(|profile| {
        profile.mode == LayoutMode::Explicit && !profile.allow_known_failure_native_fallback
    }) && native.pages.iter().any(|page| {
        evidence
            .get(&page.page_number)
            .and_then(|e| e.image.as_ref())
            .is_none()
    }) {
        return Err(invalid("explicit layout requires real rendered pages"));
    }
    let mut pages = model_plane::run_current(
        model_plane::ModelResource {
            dataset_id: Some(scope.dataset_id.clone()),
            document_id: Some(scope.document_id.clone()),
            execution_id: scope.execution_id.clone(),
        },
        actor.clone(),
        prepare_pages(&native, &scope, evidence, layout.as_ref()),
    )
    .await?;

    for page in &mut pages {
        if layout.is_some() && !evidence.contains_key(&page.page_number) {
            page.route = json!({"selected":"native","reason":if layout.as_ref().is_some_and(|profile|profile.mode==LayoutMode::Explicit){"explicit_render_unavailable"}else{"measured_layout_signals_unavailable"},"profile":scope.parser_profile});
        }
    }
    let pending_prefix = prefix(&tenant, document, execution);
    let candidate = json!({"execution_id":execution,"source_sha256":native.content_hash,"profile":scope.parser_profile,"object_prefix":pending_prefix});
    let changed=sqlx::query("UPDATE knowledge.documents SET metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{_docir_v2_candidate}',$4::jsonb,TRUE) WHERE document_id=$1 AND dataset_id=$2 AND metadata->>'_document_pipeline_execution_id'=$3 AND content=$5")
        .bind(document).bind(&dataset.dataset_id).bind(execution).bind(&candidate).bind(expected).execute(lease.connection()).await?.rows_affected();
    if changed != 1 {
        return Err(IngestError::GenerationChanged);
    }
    let recorded=sqlx::query("UPDATE knowledge.document_pipeline_executions SET manifest=COALESCE(manifest,'{}'::jsonb)||jsonb_build_object('docir_v2_candidate',$2::jsonb) WHERE execution_id=$1 AND dataset_id=$3 AND document_id=$4 AND status='running'").bind(execution).bind(&candidate).bind(&dataset.dataset_id).bind(document).execute(lease.connection()).await?.rows_affected();
    if recorded != 1 {
        return Err(IngestError::GenerationChanged);
    }
    Ok(Prepared {
        scope,
        pages,
        native,
        actor,
        expected_content: expected.into(),
        pending_prefix,
    })
}
impl Prepared {
    /// Replaces standard character chunking only for the explicitly selected v2
    /// profile. Table/formula bodies bypass split/merge but keep token safeguards.
    pub fn chunks(
        &self,
        chunker: &ingest::chunking::Chunker,
        config: &ChunkingConfig,
        title: &str,
        source_type: &str,
    ) -> Result<Vec<Chunk>> {
        if config.mode == "hierarchical" {
            return Err(invalid(
                "atomic DocIR profile requires standard chunking; hierarchy profile is unchanged",
            ));
        }
        let mut result = Vec::new();
        let mut sections = Vec::new();
        for page in &self.pages {
            for block in &page.blocks {
                if let Structure::Heading { level } = &block.structure {
                    sections.truncate(level.saturating_sub(1) as usize);
                    sections.push(block.text.clone());
                }
                let text = match &block.structure {
                    Structure::Table { markdown, html, .. } => {
                        if !markdown.is_empty() {
                            markdown.clone()
                        } else {
                            html.clone().unwrap_or_else(|| block.text.clone())
                        }
                    }
                    Structure::Formula { latex, .. } => format!("$$\n{latex}\n$$"),
                    _ => block.text.clone(),
                };
                if text.trim().is_empty() {
                    continue;
                }
                let mut rows = if matches!(
                    block.structure,
                    Structure::Table { .. } | Structure::Formula { .. }
                ) {
                    ingest::parser::require_text_budget(&text)?;
                    vec![serde_json::from_value(
                        json!({"segment_id":"pending","dataset_id":self.scope.dataset_id,"document_id":self.scope.document_id,"position":0,"level":3,"text":text,"token_count":chunker.tokens.count(&text),"metadata":{},"enabled":true,"status":"pending","word_count":text.split_whitespace().count(),"keywords":[],"source_reference":{}}),
                    )?]
                } else {
                    chunker.standard_chunks(
                        &text,
                        config,
                        &self.scope.dataset_id,
                        &self.scope.document_id,
                        title,
                        source_type,
                    )?
                };
                for row in &mut rows {
                    let position = i32::try_from(result.len())
                        .map_err(|_| invalid("chunk position overflow"))?;
                    row.position = position;
                    row.segment_id =
                        ingest::stable_segment_id(&self.scope.document_id, "text", position);
                    row.index_node_id = Some(ingest::stable_index_node_id(
                        &self.scope.document_id,
                        "text",
                        position,
                    ));
                    let reference = json!({"schema":"docir-publication-ref/v2","tenant_id":self.scope.tenant_id,"dataset_id":self.scope.dataset_id,"document_id":self.scope.document_id,"execution_id":self.scope.execution_id,"source_sha256":self.native.content_hash,"parser_profile":self.scope.parser_profile,"page_number":page.page_number,"block_id":block.id,"region":block.region,"coordinate_space":page.coordinate_space});
                    row.metadata["docir_v2"] = reference.clone();
                    row.metadata["structure"] = serde_json::to_value(&block.structure)?;
                    row.metadata["section_path"] = json!(sections);
                    row.source_reference = reference;
                    row.page_start = Some(page.page_number as i32);
                    row.page_end = Some(page.page_number as i32);
                    row.page_number = Some(page.page_number as i32);
                    row.section_header = sections.last().cloned();
                    row.created_by = Some(self.actor.user_id.clone());
                    row.source_type = Some(source_type.into());
                    row.citation_text = Some(block.text.clone());
                    let hash=ingest::sha256(ingest::canonical_json(&json!({"text":row.text,"docir":row.metadata["docir_v2"],"structure":row.metadata["structure"]})).as_bytes());
                    row.content_hash = Some(hash.clone());
                    row.index_node_hash = Some(hash);
                    result.push(row.clone());
                    if result.len() > ingest::chunking::MAX_CHUNK_OUTPUTS {
                        return Err(invalid("atomic chunk output budget exceeded"));
                    }
                }
            }
        }
        Ok(result)
    }
    /// Call within the existing commit_text_publication transaction, after that
    /// transaction has selected/created/restored the actual document version and
    /// before segment insertion/metadata activation. A failure rolls SQL back.
    pub async fn finalize(
        &self,
        conn: &mut PgConnection,
        assets: &AssetStorage,
        negative_revision: i64,
    ) -> Result<Published> {
        self.finalize_for_content(conn, assets, negative_revision, &self.expected_content)
            .await
    }
    pub async fn finalize_for_content(
        &self,
        conn: &mut PgConnection,
        assets: &AssetStorage,
        negative_revision: i64,
        expected_content: &str,
    ) -> Result<Published> {
        let execution = self
            .scope
            .execution_id
            .as_deref()
            .ok_or(IngestError::GenerationChanged)?;
        let (tenant, user) = authority(
            conn,
            &self.scope.dataset_id,
            &self.scope.document_id,
            execution,
            expected_content,
        )
        .await?;
        actor_matches(&self.actor, &tenant, &user)?;
        if negative_revision >= 0 {
            return Err(IngestError::GenerationChanged);
        }
        let final_revision = final_revision(negative_revision)?;
        let actual:i64=sqlx::query_scalar("SELECT content_revision FROM knowledge.datasets WHERE dataset_id=$1 AND tenant_id=$2 AND is_deleted=FALSE FOR UPDATE").bind(&self.scope.dataset_id).bind(&tenant).fetch_one(&mut *conn).await?;
        if actual != negative_revision {
            return Err(IngestError::GenerationChanged);
        }
        let version:String=sqlx::query_scalar("SELECT v.version_id FROM knowledge.document_versions v JOIN knowledge.documents d ON d.document_id=v.document_id AND d.current_version=v.version_number WHERE d.document_id=$1 AND d.dataset_id=$2 AND v.change_type NOT IN ('pending_restore','pending_before_restore') FOR UPDATE OF v").bind(&self.scope.document_id).bind(&self.scope.dataset_id).fetch_optional(&mut *conn).await?.ok_or(IngestError::GenerationChanged)?;
        let document = DocV2 {
            schema: "docir/v2".into(),
            filename: self.native.filename.clone(),
            mime: self.native.mime.clone(),
            source: SourceIdentity {
                tenant_id: tenant,
                dataset_id: self.scope.dataset_id.clone(),
                document_id: self.scope.document_id.clone(),
                document_version: version,
                publication_generation: final_revision.to_string(),
                source_sha256: self.native.content_hash.clone(),
                parser_profile: self.scope.parser_profile.clone(),
                execution_id: Some(execution.into()),
            },
            pages: self.pages.clone(),
            native_metadata: self.native.metadata.clone(),
        };
        let bytes = serde_json::to_vec(&document)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(invalid("DocIR object exceeds publication budget"));
        }
        let hash = ingest::sha256(&bytes);
        let key = format!("{}{hash}.json", self.pending_prefix);
        if assets.exists(&key).await? {
            let stored = assets.download_bounded(&key, 16 * 1024 * 1024).await?;
            if ingest::sha256(&stored) != hash {
                return Err(IngestError::GenerationChanged);
            }
        } else {
            assets
                .backend
                .upload(
                    &key,
                    bytes,
                    "application/json",
                    BTreeMap::from([("sha256".into(), hash.clone())]),
                )
                .await
                .map_err(|_| IngestError::Storage)?;
            let stored = assets.download_bounded(&key, 16 * 1024 * 1024).await?;
            if ingest::sha256(&stored) != hash {
                return Err(IngestError::Storage);
            }
        }
        let manifest = json!({"schema":"docir/v2","object_key":key,"object_sha256":hash,"source":document.source});
        let changed=sqlx::query("UPDATE knowledge.document_pipeline_executions SET manifest=COALESCE(manifest,'{}'::jsonb)||jsonb_build_object('docir_v2',$2::jsonb) WHERE execution_id=$1 AND status='running'").bind(execution).bind(&manifest).execute(&mut *conn).await?.rows_affected();
        if changed != 1 {
            return Err(IngestError::GenerationChanged);
        }
        sqlx::query("UPDATE knowledge.document_versions SET metadata=COALESCE(metadata,'{}'::jsonb)||jsonb_build_object('docir_v2',$2::jsonb) WHERE version_id=$1 AND document_id=$3").bind(&document.source.document_version).bind(&manifest).bind(&self.scope.document_id).execute(&mut *conn).await?;
        Ok(Published {
            patch: json!({"docir_v2":manifest,"_docir_v2_candidate":null}),
            document,
        })
    }
}
/// Caller for the current standard worker path. Rich structured IR is consumed
/// directly; PDF fallback uses actual streaming page output, never an invented
/// page 1 covering concatenated multi-page text. No existing source path changes
/// unless this v2 profile was explicitly selected.
pub async fn prepare_standard(
    service: &IngestionService,
    lease: &mut DocumentLease,
    dataset: &Dataset,
    doc: &Value,
    execution: &str,
    content: &str,
    rich_ir: Option<DocIR>,
    config: &Config,
) -> Result<Prepared> {
    let explicit = config
        .layout
        .as_ref()
        .is_some_and(|layout| layout.mode == LayoutMode::Explicit);
    let pdf = ingest::string(doc, "mime_type") == "application/pdf"
        || ingest::string(doc, "title")
            .to_ascii_lowercase()
            .ends_with(".pdf");
    let rich_ir = rich_ir
        .filter(|ir| !explicit || !pdf || ir.metadata["source"] == "existing_streaming_backend");
    let native = if let Some(ir) = rich_ir {
        ir
    } else {
        let document = ingest::string(doc, "document_id");
        let mime = ingest::string(doc, "mime_type");
        let filename = ingest::string(doc, "title");
        if mime == "application/pdf" || filename.to_ascii_lowercase().ends_with(".pdf") {
            let key = doc["metadata"]["original_file_key"]
                .as_str()
                .ok_or_else(|| invalid("v2 PDF requires its real original source"))?;
            if !key.starts_with(&format!(
                "knowledge/documents/{}/{}/original/",
                dataset.tenant_id, document
            )) {
                return Err(IngestError::Permission);
            }
            let bytes = service
                .assets
                .download_bounded(key, 100 * 1024 * 1024)
                .await?;
            let hash = ingest::sha256(&bytes);
            let loader = service
                .streaming
                .as_ref()
                .ok_or_else(|| invalid("v2 PDF requires existing native streaming backend"))?;
            let mut loader = loader.clone();
            loader.extract_images = false;
            let source = ingest::streaming::PdfSource::Bytes(std::sync::Arc::new(bytes));
            let count = loader.get_page_count(&source).await?;
            if count == 0 || count > ingest::parser::MAX_PAGES {
                return Err(invalid("v2 streaming page budget exceeded"));
            }
            let mut pages = Vec::new();
            for start in (1..=count).step_by(20) {
                pages.extend(
                    loader
                        .load_page_range(&source, start, (start + 19).min(count))
                        .await?,
                );
            }
            streaming_ir(document, &hash, filename, mime, pages)?
        } else {
            // Existing decoded Office/text content has no verified page geometry.
            // Keep the native parser's unknown-region semantics; never map it to
            // physical Office/PDF page coordinates.
            let parser = service.parser.clone();
            let bytes = content.as_bytes().to_vec();
            let id = document.to_owned();

            let mut ir = tokio::task::spawn_blocking(move || {
                parser.parse(&id, &bytes, "decoded.txt", "text/plain")
            })
            .await
            .map_err(|_| invalid("v2 text adapter task failed"))??;
            ir.filename = filename.into();
            ir.mime = Some(mime.into());
            ir.metadata["coordinate_provenance"] = json!("logical_text_page_only");
            ir
        }
    };
    // Existing PageSignals exposes has_text_layer/text_length/image_count only.
    // Those counts cannot honestly become area coverage ratios. Until an owning
    // native layout backend supplies measured region coverage, keep native output
    // and record that fact; the explicit evidence API above remains usable.
    let evidence =
        explicit_layout::standard_evidence(service, dataset, doc, &native, config).await?;
    prepare(
        service, lease, dataset, doc, execution, native, &evidence, config,
    )
    .await
}
impl Published {
    /// SQL rows carry the resolved citation. Prepared vector metadata retains the
    /// stable execution publication-ref; resolve_reference resolves the same
    /// authoritative version without inventing a version before commit.
    pub fn stamp_chunks(&self, chunks: &mut [Chunk]) -> Result<()> {
        for chunk in chunks {
            if chunk.source_reference["schema"] != "docir-publication-ref/v2" {
                continue;
            }
            let page = chunk.source_reference["page_number"]
                .as_u64()
                .ok_or_else(|| invalid("citation page absent"))? as usize;
            let block = chunk.source_reference["block_id"]
                .as_str()
                .ok_or_else(|| invalid("citation block absent"))?;
            let citation = self
                .document
                .citation(page, block)
                .ok_or_else(|| invalid("citation is not in published IR"))?;
            chunk.metadata["citation"] = serde_json::to_value(citation)?;
        }
        Ok(())
    }
}
/// Vector results can expose a stable reference before the version transaction
/// exists. This reader resolves it through the actual version's fenced manifest,
/// with current dataset ACL/generation checks before and after storage I/O.
pub async fn resolve_reference(
    store: &crate::migration_knowledge_query::store::QueryStore,
    assets: &AssetStorage,
    user: &UserContext,
    expected: &crate::migration_knowledge_query::types::IndexGeneration,
    reference: &Value,
) -> Result<Citation> {
    if reference["schema"] != "docir-publication-ref/v2" || reference["tenant_id"] != user.tenant_id
    {
        return Err(IngestError::Permission);
    }
    let dataset = reference["dataset_id"]
        .as_str()
        .ok_or(IngestError::Permission)?;
    let document = reference["document_id"]
        .as_str()
        .ok_or(IngestError::Permission)?;
    let execution = reference["execution_id"]
        .as_str()
        .ok_or(IngestError::Permission)?;
    store.require_generation(user, dataset, expected).await?;
    let manifest:Value=sqlx::query_scalar("SELECT e.manifest->'docir_v2' FROM knowledge.document_pipeline_executions e JOIN knowledge.documents d ON d.document_id=e.document_id AND d.dataset_id=e.dataset_id JOIN knowledge.document_versions v ON v.document_id=d.document_id AND v.version_id=e.manifest->'docir_v2'->'source'->>'document_version' WHERE d.dataset_id=$1 AND d.document_id=$2 AND e.execution_id=$3 AND v.change_type NOT IN ('pending_restore','pending_before_restore')")
        .bind(dataset).bind(document).bind(execution).fetch_optional(&store.pool).await?.ok_or(IngestError::NotFound)?;
    let key = manifest["object_key"]
        .as_str()
        .filter(|key| key.starts_with(&prefix(&user.tenant_id, document, execution)))
        .ok_or(IngestError::Permission)?;
    let bytes = assets.download_bounded(key, 16 * 1024 * 1024).await?;
    if manifest["object_sha256"] != ingest::sha256(&bytes) {
        return Err(IngestError::GenerationChanged);
    }
    let ir: DocV2 = serde_json::from_slice(&bytes)?;
    if ir.source.tenant_id != user.tenant_id
        || ir.source.dataset_id != dataset
        || ir.source.document_id != document
        || ir.source.execution_id.as_deref() != Some(execution)
        || reference["source_sha256"] != ir.source.source_sha256
        || reference["parser_profile"] != ir.source.parser_profile
        || manifest["source"] != serde_json::to_value(&ir.source)?
    {
        return Err(IngestError::Permission);
    }
    let page = reference["page_number"]
        .as_u64()
        .ok_or(IngestError::Permission)? as usize;
    let block = reference["block_id"]
        .as_str()
        .ok_or(IngestError::Permission)?;
    let citation = ir.citation(page, block).ok_or(IngestError::NotFound)?;
    if reference["region"] != serde_json::to_value(citation.region)?
        || reference["coordinate_space"] != citation.coordinate_space
    {
        return Err(IngestError::Permission);
    }
    store.require_generation(user, dataset, expected).await?;
    Ok(citation)
}

fn final_revision(negative: i64) -> Result<i64> {
    if negative >= 0 {
        return Err(IngestError::GenerationChanged);
    }
    negative
        .checked_abs()
        .and_then(|n| n.checked_add(1))
        .ok_or(IngestError::GenerationChanged)
}
#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;

pub fn hash_source_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut input = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut block = [0u8; 8192];
    loop {
        let len = input.read(&mut block)?;
        if len == 0 {
            break;
        }
        digest.update(&block[..len]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
#[path = "vision_integration.rs"]
mod vision_integration;
pub use vision_integration::prepare_vision;

#[path = "explicit_layout.rs"]
mod explicit_layout;
