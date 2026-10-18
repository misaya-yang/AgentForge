//! Citation identity comes from active SQL rows and their immutable publication,
//! never from Qdrant payloads or model-generated answer text.
use super::{
    store::QueryStore,
    types::{Dataset, QueryError, Result, RetrieveResult},
};
use crate::migration_knowledge_ingest::parsing_v2::{Citation, SourceIdentity};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::collections::BTreeMap;

fn citation(
    dataset: &Dataset,
    document: &str,
    reference: &Value,
    source: &Value,
) -> Result<Citation> {
    let source: SourceIdentity =
        serde_json::from_value(source.clone()).map_err(|_| QueryError::GenerationChanged)?;
    if reference["schema"] != "docir-publication-ref/v2"
        || source.tenant_id != dataset.tenant_id
        || source.dataset_id != dataset.dataset_id
        || source.document_id != document
        || reference["tenant_id"] != source.tenant_id
        || reference["dataset_id"] != source.dataset_id
        || reference["document_id"] != source.document_id
        || reference["execution_id"].as_str() != source.execution_id.as_deref()
        || source.execution_id.is_none()
        || reference["source_sha256"] != source.source_sha256
        || reference["parser_profile"] != source.parser_profile
        || source.document_version.is_empty()
        || source
            .publication_generation
            .parse::<u64>()
            .ok()
            .is_none_or(|n| n == 0)
    {
        return Err(QueryError::GenerationChanged);
    }
    let page = reference["page_number"]
        .as_u64()
        .filter(|n| *n > 0)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(QueryError::GenerationChanged)?;
    let block = reference["block_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(QueryError::GenerationChanged)?;
    let coordinate = reference["coordinate_space"]
        .as_str()
        .filter(|s| {
            [
                "page_points",
                "render_pixels",
                "native_page_units",
                "unknown",
            ]
            .contains(s)
        })
        .ok_or(QueryError::GenerationChanged)?;
    let region: Option<crate::migration_knowledge_ingest::parsing_v2::Region> =
        serde_json::from_value(reference["region"].clone())
            .map_err(|_| QueryError::GenerationChanged)?;
    if region.is_some_and(|r| {
        coordinate == "unknown"
            || [r.x0, r.y0, r.x1, r.y1].iter().any(|v| !v.is_finite())
            || r.x0 < 0.
            || r.y0 < 0.
            || r.x1 <= r.x0
            || r.y1 <= r.y0
    }) {
        return Err(QueryError::GenerationChanged);
    }
    Ok(Citation {
        source,
        page_number: page,
        block_id: block.into(),
        region,
        coordinate_space: coordinate.into(),
    })
}
impl QueryStore {
    pub(super) async fn enrich_region_citations(
        &self,
        dataset: &Dataset,
        results: &mut [RetrieveResult],
    ) -> Result<()> {
        let ids: Vec<_> = results
            .iter()
            .filter(|r| {
                r.metadata["source_reference"]["schema"] == "docir-publication-ref/v2"
                    || r.metadata["metadata"]["docir_v2"]["schema"] == "docir-publication-ref/v2"
            })
            .map(|r| r.segment_id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let rows: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('segment_id',s.segment_id,'document_id',s.document_id,'reference',s.source_reference,'manual',s.metadata->'manual_source_override','source',e.manifest->'docir_v2'->'source') FROM knowledge.segments s JOIN knowledge.documents d ON d.document_id=s.document_id AND d.dataset_id=s.dataset_id JOIN knowledge.datasets ds ON ds.dataset_id=s.dataset_id JOIN knowledge.document_pipeline_executions e ON e.execution_id=s.source_reference->>'execution_id' AND e.document_id=s.document_id AND e.dataset_id=s.dataset_id JOIN knowledge.document_versions v ON v.document_id=d.document_id AND v.version_id=e.manifest->'docir_v2'->'source'->>'document_version' WHERE s.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND s.segment_id=ANY($3::text[]) AND COALESCE(s.enabled,TRUE)=TRUE AND s.status='completed' AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') AND v.change_type NOT IN ('pending_restore','pending_before_restore')")
            .bind(&dataset.dataset_id).bind(&dataset.tenant_id).bind(&ids).fetch_all(crate::database::executor(&self.pool)).await?;
        let rows: BTreeMap<_, _> = rows
            .iter()
            .filter_map(|r| r["segment_id"].as_str().map(|id| (id, r)))
            .collect();
        for result in results.iter_mut().filter(|r| ids.contains(&r.segment_id)) {
            let row = rows
                .get(result.segment_id.as_str())
                .ok_or(QueryError::GenerationChanged)?;
            if row["document_id"] != result.document_id {
                return Err(QueryError::GenerationChanged);
            }
            if row["manual"] == true {
                // A user-edited body is no longer the immutable DocIR block's text.
                result.metadata["citation"] = Value::Null;
                result.metadata["source_reference"] = Value::Null;
                if let Some(metadata) = result.metadata.as_object_mut() {
                    metadata.remove("document_version");
                }
                continue;
            }
            let cite = citation(
                dataset,
                &result.document_id,
                &row["reference"],
                &row["source"],
            )?;
            result.metadata["source_reference"] = row["reference"].clone();
            result.metadata["citation"] =
                serde_json::to_value(cite).map_err(|_| QueryError::InvalidProviderResponse)?;
            result.metadata["document_version"] = row["source"]["document_version"].clone();
            result.metadata["page_number"] = row["reference"]["page_number"].clone();
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn citations_bind_all_authority_and_version_fields_without_inventing_geometry() {
        let dataset: Dataset =
            serde_json::from_value(json!({"dataset_id":"d","tenant_id":"t","name":"test"}))
                .unwrap();
        let source = json!({"tenant_id":"t","dataset_id":"d","document_id":"doc","document_version":"v","publication_generation":"4","source_sha256":"sha","parser_profile":"p","execution_id":"e"});
        let reference = json!({"schema":"docir-publication-ref/v2","tenant_id":"t","dataset_id":"d","document_id":"doc","source_sha256":"sha","parser_profile":"p","execution_id":"e","page_number":2,"block_id":"b","region":null,"coordinate_space":"unknown"});
        let c = citation(&dataset, "doc", &reference, &source).unwrap();
        assert_eq!(c.source.document_version, "v");
        assert!(c.region.is_none());
        for key in [
            "tenant_id",
            "dataset_id",
            "document_id",
            "source_sha256",
            "parser_profile",
            "execution_id",
        ] {
            let mut r = reference.clone();
            r[key] = json!("other");
            assert!(citation(&dataset, "doc", &r, &source).is_err(), "{key}");
        }
        let mut r = reference.clone();
        r["region"] = json!({"x0":0.,"y0":0.,"x1":10.,"y1":10.});
        assert!(citation(&dataset, "doc", &r, &source).is_err());
        r["coordinate_space"] = json!("page_points");
        assert!(
            citation(&dataset, "doc", &r, &source)
                .unwrap()
                .region
                .is_some()
        );
    }
}
