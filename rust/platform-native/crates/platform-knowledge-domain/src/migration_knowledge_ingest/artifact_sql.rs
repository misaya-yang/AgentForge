//! Tenant-scoped parser artifact SQL transcribed from knowledge_artifacts.py.

pub const LOAD_PARSING_IR_0: &str = r###"
                SELECT ir_id, tenant_id, dataset_id, document_id,
                       generation_key, content_hash, schema_version,
                       parser_bundle, parser_config_hash, cascade_config,
                       ir, stats, created_at, updated_at
                FROM knowledge.kb_parsing_ir
                WHERE tenant_id = $1
                  AND dataset_id = $2
                  AND document_id = $3
                  AND generation_key = $4
                  AND parser_bundle = $5
                  AND parser_config_hash = $6
                "###;

pub const STORE_PARSING_IR_0: &str = r###"
                INSERT INTO knowledge.kb_parsing_ir (
                    tenant_id, dataset_id, document_id, generation_key,
                    content_hash, schema_version, parser_bundle,
                    parser_config_hash, cascade_config, ir, stats
                )
                SELECT $1::varchar, $2::varchar, $3::varchar, $4::varchar,
                       $5::varchar, $6::varchar, $7::varchar, $8::varchar,
                       $9::jsonb, $10::jsonb, $11::jsonb
                FROM knowledge.documents AS d
                JOIN knowledge.datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.document_id = $3::varchar
                  AND d.dataset_id = $2::varchar
                  AND ds.tenant_id = $1::varchar
                  AND ds.is_deleted = FALSE
                ON CONFLICT (
                    tenant_id, dataset_id, document_id, generation_key,
                    parser_bundle, parser_config_hash
                ) DO UPDATE SET
                    content_hash = EXCLUDED.content_hash,
                    schema_version = EXCLUDED.schema_version,
                    cascade_config = EXCLUDED.cascade_config,
                    ir = EXCLUDED.ir,
                    stats = EXCLUDED.stats,
                    updated_at = NOW()
                RETURNING ir_id
                "###;

pub const LOAD_PARSING_PAGE_CACHE_0: &str = r###"
                SELECT page_ir, backend, backend_version, content_hash,
                       page_number, confidence, hard_page
                FROM knowledge.kb_parsing_page_cache
                WHERE tenant_id = $1
                  AND dataset_id = $2
                  AND document_id = $3
                  AND generation_key = $4
                  AND cache_key = $5
                  AND parser_config_hash = $6
                "###;

pub const STORE_PARSING_PAGE_CACHE_0: &str = r###"
                INSERT INTO knowledge.kb_parsing_page_cache (
                    tenant_id, dataset_id, document_id, generation_key,
                    cache_key, content_hash, page_number, backend,
                    backend_version, parser_config_hash, page_ir,
                    confidence, hard_page
                )
                SELECT $1::varchar, $2::varchar, $3::varchar, $4::varchar,
                       $5::varchar, $6::varchar, $7, $8::varchar,
                       $9::varchar, $10::varchar,
                       $11::jsonb, $12, $13
                FROM knowledge.documents AS d
                JOIN knowledge.datasets AS ds ON ds.dataset_id = d.dataset_id
                WHERE d.document_id = $3::varchar
                  AND d.dataset_id = $2::varchar
                  AND ds.tenant_id = $1::varchar
                  AND ds.is_deleted = FALSE
                ON CONFLICT (tenant_id, dataset_id, document_id, cache_key)
                DO UPDATE SET
                    generation_key = EXCLUDED.generation_key,
                    content_hash = EXCLUDED.content_hash,
                    page_number = EXCLUDED.page_number,
                    backend = EXCLUDED.backend,
                    backend_version = EXCLUDED.backend_version,
                    parser_config_hash = EXCLUDED.parser_config_hash,
                    page_ir = EXCLUDED.page_ir,
                    confidence = EXCLUDED.confidence,
                    hard_page = EXCLUDED.hard_page,
                    updated_at = NOW()
                RETURNING cache_key
                "###;

pub const STORE_IMAGE_SEGMENTS: &str = r###"
                    INSERT INTO knowledge.segments (
                        segment_id, dataset_id, document_id, position,
                        text, token_count, vector_id, metadata,
                        content_type, image_url, image_attachment_id,
                        image_filename, image_media_type, image_file_size,
                        enabled, status
                    )
                    SELECT $1::varchar, $2::varchar, $3::varchar, $4,
                           $5::text, $6, $7::varchar, $8::jsonb,
                           'image', $9::text, $10::varchar, $11::varchar,
                           $12::varchar, $13, TRUE, 'completed'
                    FROM knowledge.documents AS d
                    WHERE d.document_id = $3::varchar
                      AND d.dataset_id = $2::varchar
                    ON CONFLICT (segment_id) DO UPDATE SET
                        dataset_id = EXCLUDED.dataset_id,
                        document_id = EXCLUDED.document_id,
                        position = EXCLUDED.position,
                        text = EXCLUDED.text,
                        token_count = EXCLUDED.token_count,
                        vector_id = EXCLUDED.vector_id,
                        metadata = EXCLUDED.metadata,
                        content_type = EXCLUDED.content_type,
                        image_url = EXCLUDED.image_url,
                        image_attachment_id = EXCLUDED.image_attachment_id,
                        image_filename = EXCLUDED.image_filename,
                        image_media_type = EXCLUDED.image_media_type,
                        image_file_size = EXCLUDED.image_file_size,
                        enabled = TRUE,
                        status = 'completed',
                        error = NULL,
                        updated_at = NOW()
                    RETURNING segment_id
                    "###;
