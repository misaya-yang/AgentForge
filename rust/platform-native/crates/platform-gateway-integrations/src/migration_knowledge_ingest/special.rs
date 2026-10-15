//! Crash-recoverable cross-collection publication. Candidate points use fresh
//! generation IDs; PostgreSQL source/version authority flips atomically before
//! old point cleanup and the negative fence is released only after cleanup.
use super::{
    Dataset, IngestError, Result, canonical_json, dataset_ingestion_identity, integer, object,
    sha256, string,
};
use super::{
    assets::AssetStorage,
    hierarchy::CandidatePlan,
    service::IngestionService,
    sql,
    store::{KnowledgeWriteStore, PublicationLease},
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use std::collections::{BTreeMap, BTreeSet};
pub const SOURCE_MANIFEST_KEY: &str = "_special_source_manifest";
fn fail(message: &str) -> IngestError {
    IngestError::Validation(message.into())
}
pub fn point_plan(plan: &CandidatePlan) -> Result<BTreeMap<String, Vec<String>>> {
    let mut result = BTreeMap::new();
    let mut all = BTreeSet::new();
    for (name, points) in &plan.points_by_collection {
        if name.trim().is_empty() {
            return Err(fail("special candidate collection is missing"));
        }
        let mut ids = vec![];
        for p in points {
            let id = p["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| fail("special candidate point identity is invalid"))?;
            if !all.insert(id.to_owned()) {
                return Err(fail("special candidate point ids overlap"));
            }
            ids.push(id.to_owned())
        }
        ids.sort();
        result.insert(name.clone(), ids);
    }
    if all.is_empty() {
        return Err(fail("special candidate has no points"));
    }
    Ok(result)
}
pub fn object_plan(plan: &CandidatePlan) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for receipt in &plan.object_manifest {
        let key = string(receipt, "storage_key").trim();
        let digest = string(receipt, "sha256").trim().to_lowercase();
        if key.is_empty()
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || result.insert(key.into(), digest).is_some()
        {
            return Err(fail("special candidate object receipt is invalid"));
        }
    }
    Ok(result)
}
pub fn source_kind(plan: &CandidatePlan, objects: &BTreeMap<String, String>) -> Result<String> {
    let inferred = if plan.total_pages.is_some() || !objects.is_empty() {
        "vision"
    } else {
        "hierarchy"
    };
    if plan.source_kind == "text" {
        if inferred != "hierarchy" || plan.source_hash != sha256(plan.content.as_bytes()) {
            return Err(fail("text candidate source identity differs"));
        }
        return Ok("text".into());
    }
    if !plan.source_kind.is_empty() && plan.source_kind != inferred {
        return Err(fail("special candidate source kind differs"));
    }
    Ok(inferred.into())
}
pub fn vision_page_texts(
    plan: &CandidatePlan,
    objects: &BTreeMap<String, String>,
) -> Result<Vec<Value>> {
    let generation = uuid::Uuid::parse_str(&plan.generation_id)
        .map_err(|_| fail("vision generation invalid"))?
        .simple()
        .to_string();
    let re = regex::Regex::new(r"^page_([1-9][0-9]*)_g([0-9a-f]{32})$").unwrap();
    let mut pages = BTreeMap::new();
    for receipt in &plan.object_manifest {
        let caps = re
            .captures(string(receipt, "attachment_id"))
            .ok_or_else(|| fail("vision page generation identity differs"))?;
        let number: usize = caps[1]
            .parse()
            .map_err(|_| fail("vision page identity invalid"))?;
        let key = string(receipt, "storage_key");
        if caps.get(2).unwrap().as_str() != generation
            || !objects.contains_key(key)
            || pages.contains_key(&number)
        {
            return Err(fail("vision page object receipt is incomplete"));
        }
        pages.insert(number,json!({"page_number":number,"text":plan.extracted_texts.get(&number).cloned().unwrap_or_default(),"storage_key":key,"sha256":objects[key]}));
    }
    if Some(pages.len()) != plan.total_pages || objects.is_empty() {
        return Err(fail("vision page receipt count differs"));
    }
    let pages: Vec<_> = pages.into_values().collect();
    if reconstruct(&pages) != plan.content {
        return Err(fail("vision page texts differ from candidate content"));
    }
    Ok(pages)
}
fn reconstruct(pages: &[Value]) -> String {
    pages
        .iter()
        .filter(|p| !string(p, "text").is_empty())
        .map(|p| {
            format!(
                "[Page {}]\n{}",
                integer(p, "page_number"),
                string(p, "text")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
pub fn plan_hash(
    plan: &CandidatePlan,
    objects: &BTreeMap<String, String>,
    source: &Value,
) -> String {
    let rows: Vec<_> = plan
        .segment_rows
        .iter()
        .map(|row| {
            let mut v = row.to_value();
            v.as_object_mut().unwrap().remove("image_url");
            v
        })
        .collect();
    sha256(canonical_json(&json!({"source_hash":plan.source_hash,"content":plan.content,"points":plan.points_by_collection,"rows":rows,"objects":objects,"source_manifest":source,"summary":plan.summary_row})).as_bytes())
}
#[derive(Clone)]
pub struct SpecialPublicationCoordinator {
    pub service: IngestionService,
}
impl SpecialPublicationCoordinator {
    async fn old_source(
        &self,
        conn: &mut PgConnection,
        dataset: &Dataset,
        current: &Value,
        index: &Value,
    ) -> Result<Value> {
        let doc = string(current, "document_id");
        let meta = object(&current["metadata"]);
        let previous = object(&meta[SOURCE_MANIFEST_KEY]);
        let content = string(current, "content");
        let hash = sha256(content.as_bytes());
        if previous
            .get("content_hash")
            .is_some_and(|v| v != &json!(hash))
        {
            return Err(fail("old source content manifest differs"));
        }
        let mut objects: BTreeMap<String, String> =
            serde_json::from_value(object(&previous["objects"]))?;
        let images = KnowledgeWriteStore::segments_on(conn, &dataset.dataset_id, doc)
            .await?
            .into_iter()
            .filter(|s| s.content_type == "image")
            .collect::<Vec<_>>();
        let mut pages = vec![];
        let re = regex::Regex::new(r"^page_([1-9][0-9]*)(?:_g[0-9a-f]{32})?$").unwrap();
        for row in &images {
            let attachment = row
                .image_attachment_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| fail("old image object identity is missing"))?;
            let name = row
                .image_filename
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| fail("old image object identity is missing"))?;
            let key = AssetStorage::image_key(&dataset.tenant_id, doc, attachment, name)?;
            let bytes = self.service.assets.download_original(&key).await?;
            let digest = sha256(&bytes);
            if objects.get(&key).is_some_and(|d| d != &digest) {
                return Err(fail("old image bytes changed"));
            }
            objects.insert(key.clone(), digest.clone());
            let number = row.metadata["page_number"]
                .as_i64()
                .or_else(|| re.captures(attachment).and_then(|c| c[1].parse().ok()))
                .filter(|n| *n > 0)
                .ok_or_else(|| fail("old image page order is unavailable"))?;
            let marker = format!("[Page {number}]");
            let text = if row.text == marker {
                ""
            } else {
                row.text
                    .strip_prefix(&format!("{marker}\n"))
                    .ok_or_else(|| fail("old image page text unavailable"))?
            };
            pages.push(json!({"page_number":number,"text":text,"storage_key":key,"sha256":digest}));
        }
        pages.sort_by_key(|p| integer(p, "page_number"));
        if !images.is_empty()
            && (pages
                .iter()
                .map(|p| integer(p, "page_number"))
                .collect::<BTreeSet<_>>()
                .len()
                != pages.len()
                || reconstruct(&pages) != content)
        {
            return Err(fail("old image page texts differ from source"));
        }
        let kind = string(&previous, "source_kind");
        if !["", "text", "hierarchy", "vision"].contains(&kind)
            || (!images.is_empty() && !["", "vision"].contains(&kind))
        {
            return Err(fail("old source kind invalid"));
        }
        if kind == "vision"
            && (previous["page_texts"] != json!(pages) || previous["objects"] != json!(objects))
        {
            return Err(fail("old vision page receipts differ from serving rows"));
        }
        if kind == "text"
            && (!objects.is_empty()
                || previous
                    .get("source_hash")
                    .is_some_and(|v| v != &json!(hash))
                || previous.get("page_texts").is_some_and(|v| v != &json!([])))
        {
            return Err(fail("old text source receipt differs"));
        }
        let index = previous.get("index_config").unwrap_or(index);
        if !index.is_object() {
            return Err(fail("old source index config invalid"));
        }
        Ok(
            json!({"generation_id":string(&previous,"generation_id"),"source_hash":previous.get("source_hash").cloned().unwrap_or(json!(hash)),"content_hash":hash,"source_kind":if kind.is_empty(){if images.is_empty(){"text"}else{"vision"}}else{kind},"original_source_key":previous.get("original_source_key").cloned().unwrap_or(meta["original_file_key"].clone()),"index_config":index,"page_texts":pages,"objects":objects}),
        )
    }
    async fn delete_points(&self, dataset: &Dataset, manifest: &Value, key: &str) -> Result<()> {
        for (name, entry) in manifest["collections"]
            .as_object()
            .ok_or_else(|| fail("special collection manifest missing"))?
        {
            let ids: Vec<String> = serde_json::from_value(entry[key].clone())?;
            if !ids.is_empty() {
                let mut scoped = dataset.clone();
                scoped.collection_name = Some(name.clone());
                self.service
                    .indexer
                    .vectors
                    .delete_points(&scoped, &ids)
                    .await?;
            }
        }
        Ok(())
    }
    async fn delete_objects(&self, dataset: &Dataset, manifest: &Value) -> Result<()> {
        let generation = uuid::Uuid::parse_str(string(manifest, "generation_id"))
            .map_err(|_| fail("special generation invalid"))?
            .simple()
            .to_string();
        let re = regex::Regex::new(
            r"(?:^|/)(page_([1-9][0-9]*)_g([0-9a-f]{32}))_(page_([1-9][0-9]*)\.png)$",
        )
        .unwrap();
        for key in manifest["planned_object_keys"]
            .as_array()
            .ok_or_else(|| fail("special object plan missing"))?
        {
            let key = key
                .as_str()
                .ok_or_else(|| fail("special object key invalid"))?;
            let c = re
                .captures(key)
                .ok_or_else(|| fail("special object key is outside generation"))?;
            if c.get(3).unwrap().as_str() != generation
                || c[2] != c[5]
                || AssetStorage::image_key(
                    &dataset.tenant_id,
                    string(manifest, "document_id"),
                    &c[1],
                    &c[4],
                )? != key
            {
                return Err(fail("special object key has another owner"));
            }
            if self.service.assets.exists(key).await? {
                self.service.assets.delete_key(key).await?;
                if self.service.assets.exists(key).await? {
                    return Err(fail("special object cleanup could not be verified"));
                }
            }
        }
        Ok(())
    }
    async fn finish(
        &self,
        dataset: &Dataset,
        publication: &mut PublicationLease,
        manifest: &Value,
        committed: bool,
        resume: bool,
    ) -> Result<()> {
        if committed {
            self.delete_points(dataset, manifest, "old_point_ids")
                .await?
        } else {
            if ["prepared", "points_written"].contains(&string(manifest, "phase")) {
                self.delete_points(dataset, manifest, "candidate_point_ids")
                    .await?
            }
            self.delete_objects(dataset, manifest).await?
        }
        let lifecycle = crate::migration_knowledge_query::lexical_lifecycle::LexicalLifecycle::new(
            self.service.store.pool.clone(),
            self.service.indexer.vectors.clone(),
        );
        let context = lifecycle
            .active_publication_context(&dataset.dataset_id)
            .await?;
        let revision = publication.revision;
        let mut tx = publication.connection().begin().await?;
        let ex = string(manifest, "execution_id");
        let doc = string(manifest, "document_id");
        let generation = string(manifest, "generation_id");
        if resume {
            KnowledgeWriteStore::resume_special(&mut tx, manifest).await?;
        } else {
            if !committed {
                sqlx::query("UPDATE knowledge.documents SET status='error',error='special publication aborted after cleanup',completed_at=NOW(),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2").bind(doc).bind(&dataset.dataset_id).execute(&mut *tx).await?;
                if string(manifest, "phase") != "aborted" {
                    KnowledgeWriteStore::advance_special(
                        &mut tx,
                        ex,
                        doc,
                        &dataset.dataset_id,
                        generation,
                        string(manifest, "source_hash"),
                        manifest["plan_hash"].as_str(),
                        string(manifest, "phase"),
                        "aborted",
                        None,
                        None,
                    )
                    .await?;
                }
            }
            let result = KnowledgeWriteStore::reconcile_special(
                &mut tx,
                ex,
                doc,
                &dataset.dataset_id,
                generation,
            )
            .await?;
            if !["reconciled", "already_terminal"].contains(&result) {
                return Err(fail("special publication could not reconcile"));
            }
        }
        if committed && !resume {
            KnowledgeWriteStore::update_status(&mut tx, doc, "completed", Some(100.0), None)
                .await?;
        }
        let certification = if let Some(context) = &context {
            Some(
                lifecycle
                    .recertify_active_publication(context, revision, &mut tx)
                    .await?,
            )
        } else {
            None
        };
        let final_revision: i64 = sqlx::query_scalar(sql::FINISH_INDEX_PUBLICATION_0)
            .bind(&dataset.dataset_id)
            .fetch_one(&mut *tx)
            .await?;
        if let (Some(context), Some(certification)) = (&context, &certification) {
            if integer(certification, "target_revision") != final_revision {
                return Err(IngestError::GenerationChanged);
            }
            lifecycle
                .settle_active_publication(context, certification, &mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn recover_unfinished(&self, dataset: &Dataset) -> Result<bool> {
        let current = self.service.store.get_dataset(&dataset.dataset_id).await?;
        if integer(&current, "content_revision") >= 0 {
            return Ok(false);
        }
        let mut publication = self
            .service
            .store
            .publication_lease(&dataset.dataset_id, &dataset_ingestion_identity(&current))
            .await?;
        if !publication.recovered {
            return Err(fail("special recovery lost negative revision"));
        }
        let owner =
            KnowledgeWriteStore::active_special(publication.connection(), &dataset.dataset_id)
                .await?;
        let committed = string(&owner, "phase") == "committed";
        let resume = string(&owner, "phase") == "preparing";
        self.finish(dataset, &mut publication, &owner, committed, resume)
            .await?;
        publication.release().await?;
        Ok(true)
    }
    pub async fn abort_preparing(
        &self,
        dataset: &Dataset,
        execution: &str,
        resume: bool,
    ) -> Result<()> {
        let identity = dataset_ingestion_identity(&dataset.to_value());
        let mut publication = self
            .service
            .store
            .publication_lease_bound(&dataset.dataset_id, &identity, Some(execution))
            .await?;
        let owner =
            KnowledgeWriteStore::active_special(publication.connection(), &dataset.dataset_id)
                .await?;
        if string(&owner, "execution_id") != execution || string(&owner, "phase") != "preparing" {
            return Err(fail("failed preparation lost owner"));
        }
        self.finish(dataset, &mut publication, &owner, false, resume)
            .await?;
        publication.release().await
    }
    pub async fn publish(
        &self,
        mut plan: CandidatePlan,
        dataset: &Dataset,
        execution: &str,
        expected_content: &str,
        metadata_patch: &Value,
    ) -> Result<Vec<String>> {
        let ids = point_plan(&plan)?;
        let objects = object_plan(&plan)?;
        let identity = dataset_ingestion_identity(&dataset.to_value());
        let mut publication = self
            .service
            .store
            .publication_lease_bound(&dataset.dataset_id, &identity, Some(execution))
            .await?;
        if publication.recovered {
            return Err(fail("negative special publication requires recovery"));
        }
        let mut committed = false;
        let result=async{
            let old=self.service.indexer.vectors.document_point_ids_by_collection(dataset,&plan.document_id).await?;if ids.iter().any(|(name,ids)|ids.iter().any(|id|old.get(name).is_some_and(|v|v.contains(id)))){return Err(fail("special candidate would overwrite an old point"))}let mut collections=json!({});for name in ids.keys().chain(old.keys()).collect::<BTreeSet<_>>(){collections[name]=json!({"candidate_point_ids":ids.get(name).cloned().unwrap_or_default(),"old_point_ids":old.get(name).cloned().unwrap_or_default()})}
            let current=KnowledgeWriteStore::document_on(publication.connection(),&dataset.dataset_id,&plan.document_id).await?;if string(&current,"content")!=expected_content{return Err(fail("special candidate source changed"))}let meta=object(&current["metadata"]);let content_hash=sha256(plan.content.as_bytes());let kind=source_kind(&plan,&objects)?;let pages=if kind=="vision"{vision_page_texts(&plan,&objects)?}else{vec![]};let pending=object(&meta["_document_pending_restore_version"]);let restore=!pending.as_object().unwrap().is_empty();let index=object(&dataset.index_config);let mut restore_source=json!({});let version_number;let previous_version=integer(&pending,"previous_version");let current_version=integer(&current,"current_version");let existing:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2").bind(&plan.document_id).bind(current_version as i32).fetch_optional(publication.connection()).await?;if existing.as_ref().is_some_and(|v|sha256(string(v,"content").as_bytes())!=string(v,"content_hash")){return Err(fail("old version content hash differs"))}let has_old=old.values().any(|v|!v.is_empty());let baseline=existing.is_none()&&has_old;if restore&&existing.is_none(){return Err(fail("restore has no active old version"))}
            if restore{version_number=integer(&pending,"candidate_version");let v:Value=sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2").bind(&plan.document_id).bind(version_number as i32).fetch_optional(publication.connection()).await?.ok_or_else(||fail("pending restore missing"))?;restore_source=object(&v["metadata"][SOURCE_MANIFEST_KEY]);if string(&v,"change_type")!="pending_restore"||string(&v,"content")!=plan.content||string(&v,"content_hash")!=content_hash||restore_source["source_kind"]!=kind||restore_source["source_hash"]!=plan.source_hash||restore_source["content_hash"]!=content_hash||restore_source["index_config"]!=index{return Err(fail("pending restore source identity differs"))}if kind=="vision"{let saved=restore_source["page_texts"].as_array().ok_or_else(||fail("restore page receipts absent"))?;let receipt=|p:&Value|json!([p["page_number"],p["text"],p["sha256"]]);if saved.iter().map(receipt).collect::<Vec<_>>()!=pages.iter().map(receipt).collect::<Vec<_>>(){return Err(fail("pending restore page receipts differ"))}}}else{let next:i32=sqlx::query_scalar(sql::CREATE_DOCUMENT_VERSION_0).bind(&plan.document_id).fetch_one(publication.connection()).await?;version_number=next as i64+i64::from(baseline);}
            let original=if restore{string(&restore_source,"original_source_key")}else{string(&meta,"original_file_key")};if kind=="vision"&&original.is_empty(){return Err(fail("vision original source key unavailable"))}let source=json!({"generation_id":plan.generation_id,"source_hash":plan.source_hash,"content_hash":content_hash,"source_kind":kind,"original_source_key":original,"index_config":index,"page_texts":pages,"objects":objects});let mut patch=object(metadata_patch);patch[SOURCE_MANIFEST_KEY]=source.clone();if kind=="text"{for(k,v)in [("processing_mode",json!("text_only")),("original_file_key",json!(original)),("extracted_images",json!([])),("image_count",json!(0)),("images_embedded",json!(false)),("embedded_image_count",json!(0)),("pages_processed",json!(0)),("total_pages",json!(0)),("l1_segments",json!(0)),("l2_segments",json!(0)),("l3_segments",json!(0)),("total_vectors",json!(plan.segment_ids.len())),("segments_created",json!(plan.segment_ids.len()))]{patch[k]=v}}
            let old_source=if existing.is_some()||baseline{let mut prior=current.clone();if let Some(existing)=&existing{prior["content"]=existing["content"].clone()}Some(self.old_source(publication.connection(),dataset,&prior,&index).await?)}else{None};let disabled:BTreeSet<i32>=if kind=="text"{sqlx::query_scalar("SELECT position FROM knowledge.segments WHERE document_id=$1 AND content_type='text' AND disabled_by IS NOT NULL").bind(&plan.document_id).fetch_all(publication.connection()).await?.into_iter().collect()}else{BTreeSet::new()};let disabled_ids:BTreeSet<_>=plan.segment_rows.iter().filter(|r|disabled.contains(&r.position)).map(|r|r.segment_id.clone()).collect();for points in plan.points_by_collection.values_mut(){for p in points{p["payload"]["tenant_id"]=json!(dataset.tenant_id);p["payload"]["dataset_id"]=json!(dataset.dataset_id);p["payload"]["document_id"]=json!(plan.document_id);p["payload"]["source_version"]=json!(version_number);p["payload"]["source_hash"]=json!(content_hash);if p["id"].as_str().is_some_and(|id|disabled_ids.contains(id)){p["payload"]["enabled"]=json!(false)}}}for r in &mut plan.segment_rows{r.metadata["source_version"]=json!(version_number);r.metadata["source_hash"]=json!(content_hash)}let hash=plan_hash(&plan,&objects,&source);
            {let mut tx=publication.connection().begin().await?;KnowledgeWriteStore::advance_special(&mut tx,execution,&plan.document_id,&dataset.dataset_id,&plan.generation_id,&plan.source_hash,Some(&hash),"preparing","prepared",Some(&collections),Some(&json!(objects))).await?;tx.commit().await?;}
            for(name,points)in &plan.points_by_collection{if points.is_empty(){continue}let dimension=points[0]["vector"][""].as_array().map(Vec::len).or_else(||points[0]["vector"].as_array().map(Vec::len)).filter(|n|*n>0).ok_or_else(||fail("special candidate dense vectors required"))?;let mut scoped=dataset.clone();scoped.collection_name=Some(name.clone());self.service.indexer.vectors.ensure_collection(&scoped,dimension).await?;self.service.indexer.vectors.upsert_index_points(&scoped,points).await?;}
            {let mut tx=publication.connection().begin().await?;KnowledgeWriteStore::advance_special(&mut tx,execution,&plan.document_id,&dataset.dataset_id,&plan.generation_id,&plan.source_hash,Some(&hash),"prepared","points_written",None,None).await?;tx.commit().await?;}
            {let mut tx=publication.connection().begin().await?;KnowledgeWriteStore::require_identity(&mut tx,&dataset.dataset_id,Some(&identity)).await?;let latest:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.documents d WHERE document_id=$1 AND dataset_id=$2 FOR UPDATE").bind(&plan.document_id).bind(&dataset.dataset_id).fetch_one(&mut *tx).await?;if string(&latest,"content")!=expected_content{return Err(fail("special source changed before commit"))}if restore&&(latest["metadata"]["_document_pending_restore_version"]!=pending||previous_version<=integer(&latest,"current_version")||version_number<=previous_version){return Err(fail("special restore candidate owner changed"))}KnowledgeWriteStore::insert_segments(&mut tx,&plan.segment_rows).await?;sqlx::query(sql::DELETE_SEGMENTS_BY_DOCUMENT_2).bind(&plan.document_id).bind(&plan.segment_ids).execute(&mut *tx).await?;sqlx::query(sql::ACTIVATE_STAGED_SEGMENTS_1).bind(&plan.document_id).bind(&plan.segment_ids).execute(&mut *tx).await?;sqlx::query("UPDATE knowledge.documents SET content=$3,metadata=COALESCE(metadata,'{}'::jsonb)||$4::jsonb,word_count=$5,segment_count=$6,status='completed',progress=100,error=NULL,completed_at=NOW(),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2").bind(&plan.document_id).bind(&dataset.dataset_id).bind(&plan.content).bind(&patch).bind(plan.content.split_whitespace().count() as i32).bind(plan.segment_rows.len() as i32).execute(&mut *tx).await?;
                if let Some(summary)=&plan.summary_row{sqlx::query("INSERT INTO knowledge.document_summaries (document_id,summary,keywords,topics,vector_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (document_id) DO UPDATE SET summary=EXCLUDED.summary,keywords=EXCLUDED.keywords,topics=EXCLUDED.topics,vector_id=EXCLUDED.vector_id,updated_at=NOW()").bind(&plan.document_id).bind(string(summary,"summary")).bind(&summary["keywords"]).bind(&summary["topics"]).bind(string(summary,"vector_id")).execute(&mut *tx).await?;}else{sqlx::query("DELETE FROM knowledge.document_summaries WHERE document_id=$1").bind(&plan.document_id).execute(&mut *tx).await?;}
                if let Some(old)=old_source{if baseline{let mut old_meta=meta.clone();old_meta[SOURCE_MANIFEST_KEY]=old;let v=KnowledgeWriteStore::create_version(&mut tx,&plan.document_id,expected_content,if current_version==0{"created"}else{"updated"},Some(string(&current,"title")),&old_meta,None,None,false).await?;if integer(&v,"version_number")!=version_number-1{return Err(fail("old serving source snapshot changed"))}}else{set_version_manifest(&mut tx,&plan.document_id,current_version,&old).await?;}}
                if !restore{let v=KnowledgeWriteStore::create_version(&mut tx,&plan.document_id,&plan.content,if current_version==0{"created"}else{"updated"},Some(string(&current,"title")),&patch,None,None,true).await?;if integer(&v,"version_number")!=version_number{return Err(fail("special candidate version changed"))}}else{set_version_manifest(&mut tx,&plan.document_id,version_number,&source).await?;for(number,from,to,text)in [(previous_version,"pending_before_restore","updated",expected_content),(version_number,"pending_restore","restored",plan.content.as_str())]{if sqlx::query("UPDATE knowledge.document_versions SET change_type=$4 WHERE document_id=$1 AND version_number=$2 AND change_type=$3 AND content=$5 RETURNING version_id").bind(&plan.document_id).bind(number as i32).bind(from).bind(to).bind(text).fetch_optional(&mut *tx).await?.is_none(){return Err(fail("special restore snapshots changed"))}}sqlx::query("UPDATE knowledge.documents SET current_version=$2,metadata=(COALESCE(metadata,'{}'::jsonb)-'_document_pending_restore_version')||jsonb_build_object('_document_restored_source_version',$3::integer) WHERE document_id=$1").bind(&plan.document_id).bind(version_number as i32).bind(version_number as i32).execute(&mut *tx).await?;}
                sqlx::query("UPDATE knowledge.documents SET version_count=(SELECT COUNT(*) FROM knowledge.document_versions WHERE document_id=$1) WHERE document_id=$1").bind(&plan.document_id).execute(&mut *tx).await?;KnowledgeWriteStore::advance_special(&mut tx,execution,&plan.document_id,&dataset.dataset_id,&plan.generation_id,&plan.source_hash,Some(&hash),"points_written","committed",None,None).await?;tx.commit().await?;}
            committed=true;let owner=KnowledgeWriteStore::get_special(publication.connection(),execution).await?.ok_or_else(||fail("committed owner missing"))?;self.finish(dataset,&mut publication,&owner,true,false).await?;Ok(plan.segment_ids.clone())
        }.await;
        if result.is_err() && !committed {
            if let Some(owner) =
                KnowledgeWriteStore::get_special(publication.connection(), execution).await?
            {
                if string(&owner, "phase") != "committed" {
                    let _ = self
                        .finish(dataset, &mut publication, &owner, false, false)
                        .await;
                }
            }
        }
        publication.release().await?;
        result
    }
}
async fn set_version_manifest(
    conn: &mut PgConnection,
    document: &str,
    version: i64,
    source: &Value,
) -> Result<()> {
    let changed=sqlx::query("UPDATE knowledge.document_versions SET metadata=COALESCE(metadata,'{}'::jsonb)||jsonb_build_object($3::text,$4::jsonb) WHERE document_id=$1 AND version_number=$2 RETURNING version_id").bind(document).bind(version as i32).bind(SOURCE_MANIFEST_KEY).bind(source).fetch_optional(conn).await?;
    if changed.is_none() {
        return Err(fail("special source version manifest could not be saved"));
    }
    Ok(())
}
