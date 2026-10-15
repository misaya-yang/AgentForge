-- Migration: 076_agent_knowledge_content_revision.sql
-- Goal: authoritative, transactional live-content revisions for Agent
-- Knowledge provenance without storing or replaying historical content.

BEGIN;

ALTER TABLE datasets
    ADD COLUMN IF NOT EXISTS content_revision BIGINT NOT NULL DEFAULT 0;

CREATE OR REPLACE FUNCTION agent_knowledge_bump_content_revision()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE datasets
        SET content_revision = content_revision + 1
        WHERE dataset_id IN (
            SELECT DISTINCT dataset_id
            FROM agent_knowledge_new_rows
        );
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE datasets
        SET content_revision = content_revision + 1
        WHERE dataset_id IN (
            SELECT DISTINCT dataset_id
            FROM agent_knowledge_old_rows
        );
    ELSIF TG_TABLE_NAME = 'segments' THEN
        -- Compare an explicit retrieval-effective projection. Token/word/image
        -- counts, FTS/content hashes, audit timestamps and hit accounting are
        -- derived or operational telemetry and must not move content provenance.
        UPDATE datasets
        SET content_revision = content_revision + 1
        WHERE dataset_id IN (
            WITH changed AS (
                SELECT new_row.dataset_id AS new_dataset_id,
                       old_row.dataset_id AS old_dataset_id
                FROM agent_knowledge_new_rows AS new_row
                FULL JOIN agent_knowledge_old_rows AS old_row
                  ON old_row.segment_id = new_row.segment_id
                WHERE new_row IS NULL
                   OR old_row IS NULL
                   OR jsonb_build_object(
                       'dataset_id', new_row.dataset_id,
                       'document_id', new_row.document_id,
                       'position', new_row.position,
                       'text', new_row.text,
                       'vector_id', new_row.vector_id,
                       'metadata', new_row.metadata,
                       'content_type', new_row.content_type,
                       'image_url', new_row.image_url,
                       'image_attachment_id', new_row.image_attachment_id,
                       'image_filename', new_row.image_filename,
                       'image_media_type', new_row.image_media_type,
                       'has_images', new_row.has_images,
                       'vlm_description', new_row.vlm_description,
                       'source_type', new_row.source_type,
                       'source_reference', new_row.source_reference,
                       'citation_text', new_row.citation_text,
                       'page_number', new_row.page_number,
                       'section_header', new_row.section_header,
                       'language', new_row.language,
                       'contextual_prefix', new_row.contextual_prefix,
                       'enabled', new_row.enabled,
                       'status', new_row.status,
                       'keywords', new_row.keywords,
                       'answer', new_row.answer,
                       'index_node_id', new_row.index_node_id,
                       'level', new_row.level,
                       'parent_segment_id', new_row.parent_segment_id,
                       'summary', new_row.summary,
                       'page_start', new_row.page_start,
                       'page_end', new_row.page_end
                   ) IS DISTINCT FROM jsonb_build_object(
                       'dataset_id', old_row.dataset_id,
                       'document_id', old_row.document_id,
                       'position', old_row.position,
                       'text', old_row.text,
                       'vector_id', old_row.vector_id,
                       'metadata', old_row.metadata,
                       'content_type', old_row.content_type,
                       'image_url', old_row.image_url,
                       'image_attachment_id', old_row.image_attachment_id,
                       'image_filename', old_row.image_filename,
                       'image_media_type', old_row.image_media_type,
                       'has_images', old_row.has_images,
                       'vlm_description', old_row.vlm_description,
                       'source_type', old_row.source_type,
                       'source_reference', old_row.source_reference,
                       'citation_text', old_row.citation_text,
                       'page_number', old_row.page_number,
                       'section_header', old_row.section_header,
                       'language', old_row.language,
                       'contextual_prefix', old_row.contextual_prefix,
                       'enabled', old_row.enabled,
                       'status', old_row.status,
                       'keywords', old_row.keywords,
                       'answer', old_row.answer,
                       'index_node_id', old_row.index_node_id,
                       'level', old_row.level,
                       'parent_segment_id', old_row.parent_segment_id,
                       'summary', old_row.summary,
                       'page_start', old_row.page_start,
                       'page_end', old_row.page_end
                   )
            )
            SELECT new_dataset_id FROM changed WHERE new_dataset_id IS NOT NULL
            UNION
            SELECT old_dataset_id FROM changed WHERE old_dataset_id IS NOT NULL
        );
    ELSE
        -- Document progress/error/timing and derived counts are ingestion
        -- telemetry. Keep content, availability, archive and source identity
        -- in the authoritative projection without hashing operator/audit data.
        UPDATE datasets
        SET content_revision = content_revision + 1
        WHERE dataset_id IN (
            WITH changed AS (
                SELECT new_row.dataset_id AS new_dataset_id,
                       old_row.dataset_id AS old_dataset_id
                FROM agent_knowledge_new_rows AS new_row
                FULL JOIN agent_knowledge_old_rows AS old_row
                  ON old_row.document_id = new_row.document_id
                WHERE new_row IS NULL
                   OR old_row IS NULL
                   OR jsonb_build_object(
                       'dataset_id', new_row.dataset_id,
                       'title', new_row.title,
                       'source_type', new_row.source_type,
                       'source_uri', new_row.source_uri,
                       'mime_type', new_row.mime_type,
                       'status', new_row.status,
                       'content', new_row.content,
                       'metadata', new_row.metadata,
                       'enabled', new_row.enabled,
                       'archived', new_row.archived,
                       'process_rule_id', new_row.process_rule_id,
                       'batch', new_row.batch,
                       'doc_type', new_row.doc_type,
                       'doc_form', new_row.doc_form,
                       'doc_language', new_row.doc_language,
                       'confluence_page_id', new_row.confluence_page_id,
                       'confluence_binding_id', new_row.confluence_binding_id,
                       'confluence_version', new_row.confluence_version,
                       'confluence_web_url', new_row.confluence_web_url,
                       'current_version', new_row.current_version
                   ) IS DISTINCT FROM jsonb_build_object(
                       'dataset_id', old_row.dataset_id,
                       'title', old_row.title,
                       'source_type', old_row.source_type,
                       'source_uri', old_row.source_uri,
                       'mime_type', old_row.mime_type,
                       'status', old_row.status,
                       'content', old_row.content,
                       'metadata', old_row.metadata,
                       'enabled', old_row.enabled,
                       'archived', old_row.archived,
                       'process_rule_id', old_row.process_rule_id,
                       'batch', old_row.batch,
                       'doc_type', old_row.doc_type,
                       'doc_form', old_row.doc_form,
                       'doc_language', old_row.doc_language,
                       'confluence_page_id', old_row.confluence_page_id,
                       'confluence_binding_id', old_row.confluence_binding_id,
                       'confluence_version', old_row.confluence_version,
                       'confluence_web_url', old_row.confluence_web_url,
                       'current_version', old_row.current_version
                   )
            )
            SELECT new_dataset_id FROM changed WHERE new_dataset_id IS NOT NULL
            UNION
            SELECT old_dataset_id FROM changed WHERE old_dataset_id IS NOT NULL
        );
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_knowledge_documents_content_revision ON documents;
DROP TRIGGER IF EXISTS agent_knowledge_documents_revision_insert ON documents;
DROP TRIGGER IF EXISTS agent_knowledge_documents_revision_update ON documents;
DROP TRIGGER IF EXISTS agent_knowledge_documents_revision_delete ON documents;
CREATE TRIGGER agent_knowledge_documents_revision_insert
    AFTER INSERT ON documents
    REFERENCING NEW TABLE AS agent_knowledge_new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION agent_knowledge_bump_content_revision();
CREATE TRIGGER agent_knowledge_documents_revision_update
    AFTER UPDATE ON documents
    REFERENCING OLD TABLE AS agent_knowledge_old_rows
                NEW TABLE AS agent_knowledge_new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION agent_knowledge_bump_content_revision();
CREATE TRIGGER agent_knowledge_documents_revision_delete
    AFTER DELETE ON documents
    REFERENCING OLD TABLE AS agent_knowledge_old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION agent_knowledge_bump_content_revision();

DROP TRIGGER IF EXISTS agent_knowledge_segments_content_revision ON segments;
DROP TRIGGER IF EXISTS agent_knowledge_segments_revision_insert ON segments;
DROP TRIGGER IF EXISTS agent_knowledge_segments_revision_update ON segments;
DROP TRIGGER IF EXISTS agent_knowledge_segments_revision_delete ON segments;
CREATE TRIGGER agent_knowledge_segments_revision_insert
    AFTER INSERT ON segments
    REFERENCING NEW TABLE AS agent_knowledge_new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION agent_knowledge_bump_content_revision();
CREATE TRIGGER agent_knowledge_segments_revision_update
    AFTER UPDATE ON segments
    REFERENCING OLD TABLE AS agent_knowledge_old_rows
                NEW TABLE AS agent_knowledge_new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION agent_knowledge_bump_content_revision();
CREATE TRIGGER agent_knowledge_segments_revision_delete
    AFTER DELETE ON segments
    REFERENCING OLD TABLE AS agent_knowledge_old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION agent_knowledge_bump_content_revision();

COMMIT;
