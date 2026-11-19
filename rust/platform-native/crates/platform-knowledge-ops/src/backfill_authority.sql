SELECT s.vector_id::text AS point_id, s.text,
                           s.enabled AS segment_enabled,
                           s.status AS segment_status,
                           d.enabled AS document_enabled,
                           d.archived AS document_archived,
                           d.status AS document_status,
                           (
                               COALESCE(d.metadata, '{}'::jsonb)
                               ? '_document_lifecycle_reindex'
                           ) AS document_lifecycle_pending
                    FROM segments AS s
                    JOIN documents AS d
                      ON d.document_id = s.document_id
                     AND d.dataset_id = s.dataset_id
                    JOIN datasets AS ds ON ds.dataset_id = s.dataset_id
                    WHERE s.dataset_id = $1
                      AND ds.tenant_id = $2
                      AND ds.collection_name = $3
                      AND ds.is_deleted = FALSE
                      AND NOT (
                          COALESCE(ds.index_config -> 'retrieval', '{}'::jsonb)
                          ? '_index_deletion_fence'
                      )
                      AND s.vector_id IS NOT NULL
                      AND s.vector_id <> ''
                      AND COALESCE(s.enabled, TRUE) = TRUE
                      AND s.status = 'completed'
                      AND COALESCE(s.level, 3) = 3
                      AND COALESCE(s.content_type, 'text') = 'text'
                      AND COALESCE(d.enabled, TRUE) = TRUE
                      AND COALESCE(d.archived, FALSE) = FALSE
                      AND d.status = 'completed'
                      AND NOT (
                          COALESCE(d.metadata, '{}'::jsonb)
                          ? '_document_lifecycle_reindex'
                      )
                    ORDER BY s.vector_id::text
