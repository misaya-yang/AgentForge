//! PostgreSQL statements transcribed from the canonical knowledge-service persistence.
//! Keep placeholder order aligned with the corresponding bind calls.

// datasets.py:261
pub const INSERT_DATASET_ROW_0: &str = r###"
            INSERT INTO knowledge.datasets (
                dataset_id, name, description, tenant_id, visibility,
                embedding_provider, embedding_model, embedding_dimension,
                embedding_config, index_config, collection_name,
                is_deleted, deleted_at, deleted_by, delete_reason,
                created_by
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8,
                $9, $10, $11,
                $12, $13, $14, $15,
                $16
            )
            RETURNING dataset_id
            "###;

// datasets.py:687
pub const CLAIM_DOCUMENT_FOR_ENQUEUE_0: &str = r###"
                UPDATE knowledge.documents
                SET status = 'waiting',
                    progress = 0,
                    error = NULL,
                    process_rule_id = CASE
                        WHEN $5::boolean THEN (
                            SELECT execution.process_rule_id
                            FROM knowledge.document_pipeline_executions AS execution
                            WHERE execution.execution_id = $6
                        )
                        ELSE process_rule_id
                    END,
                    -- This claim opens a new pipeline generation. Stage
                    -- timestamps describe that generation only; retaining a
                    -- prior run makes the UI report ever-growing parsing /
                    -- splitting durations. Cancellation/recovery requeues use
                    -- requeue_cancelled_document_generation instead and keep
                    -- their current-generation stamps.
                    started_at = NULL,
                    completed_at = NULL,
                    parsing_started_at = NULL,
                    splitting_started_at = NULL,
                    indexing_started_at = NULL,
                    updated_at = NOW(),
                    metadata = CASE
                        WHEN $3::jsonb IS NULL THEN
                            COALESCE(metadata, '{}'::jsonb)
                                - '_document_ingest_action'
                                - '_document_recover_stage'
                                - '_document_pipeline_execution_id'
                        ELSE
                            (COALESCE(metadata, '{}'::jsonb)
                                - '_document_recover_stage'
                                - '_document_pipeline_execution_id')
                            || $3::jsonb
                    END
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status IN ('waiting', 'completed', 'error')
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_generation'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_failed'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_confluence_sync_generation'
                  )
                  AND (
                        (
                            COALESCE(enabled, TRUE) = TRUE
                            AND COALESCE(archived, FALSE) = FALSE
                            AND NOT (
                                COALESCE(metadata, '{}'::jsonb)
                                ? '_document_lifecycle_reindex'
                            )
                        )
                        OR metadata -> '_document_lifecycle_reindex'
                            ->> 'status' = 'pending'
                            AND metadata -> '_document_lifecycle_reindex'
                                ->> 'desired_enabled' = 'true'
                            AND metadata -> '_document_lifecycle_reindex'
                                ->> 'desired_archived' = 'false'
                  )
                  -- A row already queued under a verb belongs to that verb:
                  -- only an identical re-claim by the same execution may
                  -- refresh it. A different
                  -- verb (or a plain claim that would strip the marker) is
                  -- rejected so a queued generation can never be silently
                  -- swapped or demoted. Terminal rows are exempt: they never
                  -- legitimately carry a marker, and re-claiming is the
                  -- self-healing path for stale ones.
                  AND (
                        status <> 'waiting'
                        OR NOT (
                            COALESCE(metadata, '{}'::jsonb)
                            ? '_document_ingest_action'
                        )
                        OR COALESCE(metadata ->> '_document_ingest_action', '')
                            = $4
                  )
                  -- A recovery requeue retains the running ledger link even
                  -- while status is waiting. Another request may only reclaim
                  -- the exact same execution, never replace that owner.
                  AND (
                        status <> 'waiting'
                        OR COALESCE(metadata ->> '_document_pipeline_execution_id', '') = ''
                        OR metadata ->> '_document_pipeline_execution_id' = $6
                  )
                  AND (
                        NOT $5::boolean
                        OR EXISTS (
                            SELECT 1 FROM knowledge.document_pipeline_executions AS execution
                            WHERE execution.execution_id = $6
                              AND execution.document_id = $1
                              AND execution.dataset_id = $2
                              AND execution.action = $4
                              AND execution.status = 'running'
                              AND execution.process_rule_id IS NOT NULL
                              AND execution.input_snapshot <> '{}'::jsonb
                        )
                  )
                RETURNING document_id
                "###;

// datasets.py:912
pub const LIST_QUEUED_DOCUMENTS_0: &str = r###"
                WITH eligible AS (
                    SELECT
                        ds.tenant_id,
                        d.dataset_id,
                        d.document_id,
                        d.updated_at,
                        CASE
                            WHEN COALESCE(d.metadata, '{}'::jsonb)
                                ->> '_document_ingest_action'
                                = ANY($4::text[])
                            THEN 'interactive'
                            ELSE 'bulk'
                        END AS dispatch_lane,
                        d.updated_at - (
                            CASE
                                WHEN COALESCE(d.metadata, '{}'::jsonb)
                                    ->> '_document_ingest_action'
                                    = ANY($4::text[])
                                THEN $3::double precision
                                ELSE 0
                            END * INTERVAL '1 second'
                        ) AS dispatch_order_at
                    FROM knowledge.documents AS d
                    JOIN knowledge.datasets AS ds ON ds.dataset_id = d.dataset_id
                    WHERE d.status = 'waiting'
                      AND ds.is_deleted = FALSE
                      AND NOT (
                            COALESCE(d.metadata, '{}'::jsonb)
                            ? '_document_upload_generation'
                      )
                      AND NOT (
                            COALESCE(d.metadata, '{}'::jsonb)
                            ? '_document_upload_failed'
                      )
                      AND NOT (
                            COALESCE(d.metadata, '{}'::jsonb)
                            ? '_confluence_sync_generation'
                      )
                      AND NOT COALESCE(
                            COALESCE(ds.index_config, '{}'::jsonb)
                                -> 'retrieval' ? '_index_deletion_fence',
                            FALSE
                      )
                ), tenant_ranked AS (
                    SELECT
                        eligible.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY tenant_id
                            ORDER BY dispatch_order_at, updated_at, document_id
                        ) AS tenant_round
                    FROM eligible
                )
                SELECT
                    tenant_id,
                    dataset_id,
                    document_id,
                    dispatch_lane
                FROM tenant_ranked
                ORDER BY
                    tenant_round,
                    CASE
                        WHEN $2::text = '' OR tenant_id > $2::text THEN 0
                        ELSE 1
                    END,
                    tenant_id,
                    dispatch_order_at,
                    updated_at,
                    document_id
                LIMIT $1
                "###;

// datasets.py:1054
pub const CLAIM_QUEUED_DOCUMENT_FOR_PROCESSING_0: &str = r###"
                UPDATE knowledge.documents
                SET status = 'parsing',
                    progress = 0,
                    error = NULL,
                    updated_at = NOW(),
                    started_at = COALESCE(started_at, NOW()),
                    parsing_started_at = COALESCE(parsing_started_at, NOW())
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status = 'waiting'
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_generation'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_failed'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_confluence_sync_generation'
                  )
                  AND (
                        (
                            COALESCE(enabled, TRUE) = TRUE
                            AND COALESCE(archived, FALSE) = FALSE
                            AND NOT (
                                COALESCE(metadata, '{}'::jsonb)
                                ? '_document_lifecycle_reindex'
                            )
                        )
                        OR (
                            metadata -> '_document_lifecycle_reindex'
                                ->> 'status' = 'pending'
                            AND metadata -> '_document_lifecycle_reindex'
                                ->> 'desired_enabled' = 'true'
                            AND metadata -> '_document_lifecycle_reindex'
                                ->> 'desired_archived' = 'false'
                        )
                  )
                RETURNING document_id
                "###;

// datasets.py:1123
pub const REQUEUE_CANCELLED_DOCUMENT_GENERATION_0: &str = r###"
            UPDATE knowledge.documents
            SET status = 'waiting',
                progress = 0,
                error = NULL,
                updated_at = NOW()
            WHERE document_id = $1
              AND dataset_id = $2
              AND status IN (
                    'parsing',
                    'splitting',
                    'indexing'
              )
            RETURNING document_id
            "###;

// Ordinary publication admission contention only. The caller still holds the
// document owner lease; never reset a terminal, replaced, or special generation.
pub const REQUEUE_UNPUBLISHED_DOCUMENT_GENERATION_0: &str = r###"
            UPDATE knowledge.documents AS document
            SET status = 'waiting', progress = 0, error = NULL, updated_at = NOW()
            WHERE document.document_id = $1
              AND document.dataset_id = $2
              AND document.status = 'indexing'
              AND document.metadata ->> '_document_pipeline_execution_id' = $3
              AND NOT (COALESCE(document.metadata, '{}'::jsonb)
                       ? '_special_publication_generation_id')
              AND EXISTS (
                    SELECT 1 FROM knowledge.document_pipeline_executions AS execution
                    WHERE execution.execution_id = $3
                      AND execution.document_id = document.document_id
                      AND execution.dataset_id = document.dataset_id
                      AND execution.status = 'running'
                      AND execution.process_rule_id = document.process_rule_id
                      AND NOT (COALESCE(execution.manifest, '{}'::jsonb)
                               ? 'special_publication')
              )
              AND EXISTS (
                    SELECT 1 FROM knowledge.datasets AS dataset
                    WHERE dataset.dataset_id = document.dataset_id
                      AND dataset.is_deleted = FALSE
                      AND dataset.is_archived = FALSE
                      AND NOT COALESCE(dataset.index_config -> 'retrieval'
                                       ? '_index_deletion_fence', FALSE)
              )
              AND (
                    (COALESCE(document.enabled, TRUE) = TRUE
                     AND COALESCE(document.archived, FALSE) = FALSE
                     AND NOT (COALESCE(document.metadata, '{}'::jsonb)
                              ? '_document_lifecycle_reindex'))
                    OR (document.metadata -> '_document_lifecycle_reindex'
                            ->> 'status' = 'pending'
                        AND document.metadata -> '_document_lifecycle_reindex'
                            ->> 'desired_enabled' = 'true'
                        AND document.metadata -> '_document_lifecycle_reindex'
                            ->> 'desired_archived' = 'false')
              )
            RETURNING document.document_id
            "###;

// datasets.py:1245
pub const RECORD_PIPELINE_EXECUTION_0: &str = r###"
                INSERT INTO knowledge.document_pipeline_executions (
                    execution_id,
                    document_id,
                    dataset_id,
                    action,
                    trigger_source,
                    triggered_by,
                    process_rule_id,
                    input_snapshot,
                    manifest,
                    status
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, '{}'::jsonb, 'running')
                "###;

// datasets.py:1310
pub const LINK_PIPELINE_EXECUTION_0: &str = r###"
                UPDATE knowledge.documents
                SET metadata = jsonb_set(
                        COALESCE(metadata, '{}'::jsonb),
                        '{_document_pipeline_execution_id}',
                        to_jsonb($2::text)
                    )
                WHERE document_id = $1
                RETURNING document_id
                "###;

// datasets.py:1423
pub const COMPLETE_PIPELINE_EXECUTION_0: &str = r###"
                    UPDATE knowledge.document_pipeline_executions
                    SET status = $2,
                        error = $3,
                        completed_at = NOW()
                    WHERE execution_id = $1 AND status = 'running'
                    RETURNING execution_id
                    "###;

// datasets.py:1423
pub const COMPLETE_PIPELINE_EXECUTION_1: &str = r###"
                    UPDATE knowledge.document_pipeline_executions
                    SET status = $2,
                        error = $3,
                        manifest = CASE
                            WHEN jsonb_typeof($4::jsonb) = 'object' THEN
                                COALESCE(manifest, '{}'::jsonb) || $4::jsonb
                            ELSE $4::jsonb
                        END,
                        completed_at = NOW()
                    WHERE execution_id = $1 AND status = 'running'
                    RETURNING execution_id
                    "###;

// datasets.py:2221
pub const RECORD_PROCESS_RULE_0: &str = r###"
                SELECT id
                FROM knowledge.dataset_process_rules
                WHERE dataset_id = $1 AND mode = $2 AND rules = $3::jsonb
                ORDER BY created_at ASC, id ASC
                LIMIT 1
                "###;

// datasets.py:2221
pub const RECORD_PROCESS_RULE_1: &str = r###"
                INSERT INTO knowledge.dataset_process_rules (
                    id, dataset_id, mode, rules, created_by
                )
                VALUES ($1, $2, $3, $4::jsonb, $5)
                RETURNING id
                "###;

// datasets.py:2317
pub const PIN_DOCUMENT_PROCESS_RULE_0: &str = r###"
                UPDATE knowledge.documents
                SET process_rule_id = $2
                WHERE document_id = $1
                  AND (NOT $3::boolean OR status IN ('completed', 'error'))
                RETURNING document_id
                "###;

// database.py:1652
pub const INSERT_DOCUMENT_ROW_0: &str = r###"
                INSERT INTO knowledge.documents (
                    document_id, dataset_id, title, source_type, source_uri,
                    mime_type, size_bytes, status, progress, error, content,
                    metadata, started_at, completed_at
                ) VALUES (
                    $1, $2, $3, $4, $5,
                    $6, $7, $8, $9, $10, $11,
                    $12, $13, $14
                )
            "###;

// database.py:1750
pub const FINALIZE_DOCUMENT_UPLOAD_0: &str = r###"
                UPDATE knowledge.documents
                SET title = $3,
                    source_type = $4,
                    source_uri = $5,
                    mime_type = $6,
                    size_bytes = $7,
                    status = $8,
                    progress = $9,
                    error = $10,
                    content = $11,
                    metadata = $12::jsonb,
                    started_at = $13,
                    completed_at = $14,
                    updated_at = NOW()
                WHERE document_id = $1
                  AND dataset_id = $2
                  AND status IN (
                        'uploading', 'waiting', 'uploading_images'
                  )
                  AND COALESCE(enabled, TRUE) = TRUE
                  AND COALESCE(archived, FALSE) = FALSE
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_lifecycle_reindex'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_document_upload_failed'
                  )
                  AND NOT (
                        COALESCE(metadata, '{}'::jsonb)
                        ? '_confluence_sync_generation'
                  )
                  AND metadata ->> $15 = $16
                RETURNING document_id
                "###;

// database.py:3480
pub const FINISH_INDEX_PUBLICATION_0: &str = r###"
            UPDATE knowledge.datasets
            SET content_revision = ABS(content_revision) + 1,
                updated_at = NOW()
            WHERE dataset_id = $1
              AND is_deleted = FALSE
              AND content_revision < 0
            RETURNING content_revision
            "###;

// database.py:3720
pub const INSERT_SEGMENTS_0: &str = r###"
                INSERT INTO knowledge.segments (
                    segment_id, dataset_id, document_id, position,
                    level, parent_segment_id, summary, page_start, page_end,
                    text, token_count, vector_id, metadata,
                    enabled, status, word_count, keywords, answer, created_by,
                    content_hash,
                    source_type, source_reference, citation_text,
                    page_number, section_header, language, contextual_prefix,
                    content_type, index_node_id, index_node_hash,
                    image_url, image_attachment_id, image_filename,
                    image_media_type, image_file_size
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                          $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27,
                          $28, $29, $30, $31, $32, $33, $34, $35)
                ON CONFLICT (document_id, content_type, position) DO UPDATE SET
                    segment_id = EXCLUDED.segment_id,
                    dataset_id = EXCLUDED.dataset_id,
                    level = EXCLUDED.level,
                    parent_segment_id = EXCLUDED.parent_segment_id,
                    summary = EXCLUDED.summary,
                    page_start = EXCLUDED.page_start,
                    page_end = EXCLUDED.page_end,
                    text = EXCLUDED.text,
                    token_count = EXCLUDED.token_count,
                    vector_id = EXCLUDED.vector_id,
                    metadata = EXCLUDED.metadata,
                    -- PRD T1 item 6 / §6.3: an operator's segment disable is an
                    -- explicit visibility decision and survives re-ingestion.
                    -- A content change re-stages the row's CONTENT (text, hash,
                    -- vector) but must never revoke the disable: the row keeps
                    -- enabled=FALSE and a non-staging status so the completion
                    -- flip (status='indexing' only) can never promote it. New
                    -- rows (INSERT leg) cannot be operator-disabled.
                    enabled = CASE
                        WHEN segments.disabled_by IS NOT NULL THEN FALSE
                        ELSE EXCLUDED.enabled
                    END,
                    status = CASE
                        WHEN segments.disabled_by IS NOT NULL THEN 'completed'
                        ELSE EXCLUDED.status
                    END,
                    word_count = EXCLUDED.word_count,
                    keywords = EXCLUDED.keywords,
                    answer = EXCLUDED.answer,
                    content_hash = EXCLUDED.content_hash,
                    source_type = EXCLUDED.source_type,
                    source_reference = EXCLUDED.source_reference,
                    citation_text = EXCLUDED.citation_text,
                    page_number = EXCLUDED.page_number,
                    section_header = EXCLUDED.section_header,
                    language = EXCLUDED.language,
                    contextual_prefix = EXCLUDED.contextual_prefix,
                    index_node_id = EXCLUDED.index_node_id,
                    index_node_hash = EXCLUDED.index_node_hash,
                    image_url = EXCLUDED.image_url,
                    image_attachment_id = EXCLUDED.image_attachment_id,
                    image_filename = EXCLUDED.image_filename,
                    image_media_type = EXCLUDED.image_media_type,
                    image_file_size = EXCLUDED.image_file_size,
                    updated_at = NOW()
                "###;

// database.py:3929

// database.py:3929
pub const ACTIVATE_STAGED_SEGMENTS_1: &str = r###"
                UPDATE knowledge.segments
                SET enabled = TRUE,
                    status = 'completed',
                    updated_at = NOW()
                WHERE document_id = $1
                  AND segment_id = ANY($2::text[])
                  AND status = 'indexing'
                  AND disabled_by IS NULL
                "###;

// database.py:3982

// database.py:3982
pub const DELETE_SEGMENTS_BY_DOCUMENT_1: &str = r###"DELETE FROM knowledge.segments WHERE document_id = $1 AND segment_id != ALL($2::text[]) AND content_type = $3"###;

// database.py:3982
pub const DELETE_SEGMENTS_BY_DOCUMENT_2: &str = r###"DELETE FROM knowledge.segments WHERE document_id = $1 AND segment_id != ALL($2::text[])"###;

// database.py:3982
pub const DELETE_SEGMENTS_BY_DOCUMENT_3: &str =
    r###"DELETE FROM knowledge.segments WHERE document_id = $1 AND content_type = $2"###;

// database.py:3982
pub const DELETE_SEGMENTS_BY_DOCUMENT_4: &str =
    r###"DELETE FROM knowledge.segments WHERE document_id = $1"###;

// database.py:9072
pub const SET_PENDING_RESTORE_VERSION_0: &str = r###"
            UPDATE knowledge.documents
            SET metadata = COALESCE(metadata, '{}'::jsonb)
                    || jsonb_build_object(
                        $3::text, jsonb_build_object(
                            'previous_version', $4::integer,
                            'candidate_version', $5::integer
                        ),
                        $6::text, 'reprocess',
                        $7::text, $8::text
                    ),
                updated_at = NOW()
            WHERE document_id = $1 AND dataset_id = $2 AND status = 'waiting'
              AND NOT (COALESCE(metadata, '{}'::jsonb) ? $3::text)
            RETURNING document_id
            "###;

// database.py:9119
pub const CREATE_DOCUMENT_VERSION_0: &str = r###"SELECT COALESCE(MAX(version_number), 0) + 1 as next_version FROM knowledge.document_versions WHERE document_id = $1"###;

// database.py:9119
pub const CREATE_DOCUMENT_VERSION_1: &str = r###"
                INSERT INTO knowledge.document_versions (
                    version_id, document_id, version_number, content, content_hash,
                    confluence_version, confluence_updated_at, title, metadata, word_count,
                    change_type, change_reason, changed_by, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW())
                "###;

// database.py:9119
pub const CREATE_DOCUMENT_VERSION_2: &str = r###"
                    UPDATE knowledge.documents
                    SET current_version = $1, version_count = COALESCE(version_count, 0) + 1
                    WHERE document_id = $2
                    "###;
