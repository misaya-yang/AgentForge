//! Canonical document batch SQL, unchanged except UUID bind casts.

// document_batches.py:33
pub const CREATE_OPERATION_0: &str = r###"
                    SELECT dataset_id, tenant_id
                    FROM knowledge.datasets
                    WHERE dataset_id = $1 AND is_deleted = FALSE
                    FOR SHARE
                    "###;

// document_batches.py:33
pub const CREATE_OPERATION_1: &str = r###"
                    INSERT INTO knowledge.kb_document_batch_operations (
                        operation_id, tenant_id, dataset_id, operation,
                        created_by, actor_roles
                    ) VALUES ($1::uuid, $2, $3, $4, $5, $6::jsonb)
                    ON CONFLICT (operation_id) DO NOTHING
                    RETURNING TRUE
                    "###;

// document_batches.py:33
pub const CREATE_OPERATION_2: &str = r###"
                    UPDATE knowledge.kb_document_batch_operations
                    SET total_count = $2,
                        status = $3,
                        completed_at = CASE WHEN $2 = 0 THEN NOW() ELSE NULL END,
                        updated_at = NOW()
                    WHERE operation_id = $1::uuid
                    "###;

// document_batches.py:33
pub const CREATE_OPERATION_3: &str = r###"
                    SELECT tenant_id, dataset_id, operation, status
                    FROM knowledge.kb_document_batch_operations
                    WHERE operation_id = $1::uuid
                    FOR SHARE
                    "###;

// document_batches.py:33
pub const CREATE_OPERATION_4: &str = r###"
                        INSERT INTO knowledge.kb_document_batch_items (
                            operation_id, document_id, ordinal
                        )
                        SELECT $1::uuid, document_id,
                               ROW_NUMBER() OVER (ORDER BY created_at, document_id) - 1
                        FROM knowledge.documents
                        WHERE dataset_id = $2
                        ORDER BY created_at, document_id
                        "###;

// document_batches.py:33
pub const CREATE_OPERATION_5: &str = r###"
                        INSERT INTO knowledge.kb_document_batch_items (
                            operation_id, document_id, ordinal
                        ) VALUES ($1::uuid, $2, $3)
                        "###;

// document_batches.py:33
pub const CREATE_OPERATION_6: &str = r###"
                        UPDATE knowledge.kb_document_batch_items
                        SET status = 'pending', claimed_by = NULL,
                            claimed_at = NULL, completed_at = NULL,
                            error_code = NULL, error = NULL
                        WHERE operation_id = $1::uuid
                          AND status = 'failed'
                        "###;

// document_batches.py:33
pub const CREATE_OPERATION_7: &str = r###"
                        SELECT COUNT(*) FILTER (WHERE status = 'queued') AS queued,
                               COUNT(*) FILTER (WHERE status = 'skipped') AS skipped,
                               COUNT(*) FILTER (WHERE status = 'failed') AS failed,
                               COUNT(*) FILTER (WHERE status = 'pending') AS pending
                        FROM knowledge.kb_document_batch_items
                        WHERE operation_id = $1::uuid
                        "###;

// document_batches.py:33
pub const CREATE_OPERATION_8: &str = r###"
                        UPDATE knowledge.kb_document_batch_operations
                        SET status = CASE
                                WHEN $5 > 0 THEN 'pending'
                                WHEN $2 > 0 AND $3 = 0 AND $4 = 0 THEN 'completed'
                                WHEN $2 > 0 OR $3 > 0 THEN 'partial'
                                ELSE 'failed'
                            END,
                            queued_count = $2,
                            skipped_count = $3, failed_count = $4,
                            error = NULL, last_claimed_at = NULL,
                            completed_at = CASE WHEN $5 > 0 THEN NULL ELSE NOW() END,
                            updated_at = NOW()
                        WHERE operation_id = $1::uuid AND status = 'failed'
                        "###;

// document_batches.py:33
pub const CREATE_OPERATION_9: &str = r###"
                        SELECT COUNT(*)
                        FROM knowledge.kb_document_batch_items
                        WHERE operation_id = $1::uuid
                        "###;

// document_batches.py:33
pub const CREATE_OPERATION_10: &str = r###"SELECT COUNT(*) FROM knowledge.kb_document_batch_items WHERE operation_id = $1::uuid AND status = 'claiming'"###;

// document_batches.py:255
pub const GET_OPERATION_ON_CONNECTION_0: &str = r###"
            SELECT operation_id::text, tenant_id, dataset_id, operation,
                   status, total_count, queued_count, skipped_count,
                   failed_count, created_by, error, created_at, updated_at,
                   completed_at
            FROM knowledge.kb_document_batch_operations
            WHERE operation_id = $1::uuid
              AND tenant_id = $2
              AND dataset_id = $3
            "###;

// document_batches.py:255
pub const GET_OPERATION_ON_CONNECTION_1: &str = r###"
            SELECT document_id, status, error_code, error
            FROM knowledge.kb_document_batch_items
            WHERE operation_id = $1::uuid
              AND status IN ('skipped', 'failed')
            ORDER BY ordinal
            LIMIT 1000
            "###;

// document_batches.py:298
pub const CANCEL_OPERATION_0: &str = r###"
                SELECT status, error
                FROM knowledge.kb_document_batch_operations
                WHERE operation_id = $1::uuid
                  AND tenant_id = $2 AND dataset_id = $3
                FOR UPDATE
                "###;

// document_batches.py:298
pub const CANCEL_OPERATION_1: &str = r###"
                    UPDATE knowledge.kb_document_batch_items
                    SET status = 'failed', error_code = 'cancelled',
                        error = 'cancelled by operator', completed_at = NOW()
                    WHERE operation_id = $1::uuid AND status = 'pending'
                    "###;

// document_batches.py:298
pub const CANCEL_OPERATION_2: &str = r###"
                    SELECT COUNT(*) FILTER (WHERE status = 'queued') AS queued,
                           COUNT(*) FILTER (WHERE status = 'skipped') AS skipped,
                           COUNT(*) FILTER (WHERE status = 'failed') AS failed
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                    "###;

// document_batches.py:298
pub const CANCEL_OPERATION_3: &str = r###"
                    UPDATE knowledge.kb_document_batch_operations
                    SET status = 'failed', queued_count = $2,
                        skipped_count = $3, failed_count = $4,
                        error = 'cancelled by operator',
                        completed_at = NOW(), updated_at = NOW()
                    WHERE operation_id = $1::uuid
                    "###;

// document_batches.py:369
pub const CLAIM_NEXT_ITEM_0: &str = r###"
                    SELECT op.*
                    FROM knowledge.kb_document_batch_operations AS op
                    WHERE op.status IN ('pending', 'running')
                      AND EXISTS (
                          SELECT 1
                          FROM knowledge.kb_document_batch_items AS item
                          WHERE item.operation_id = op.operation_id
                            AND (
                                item.status = 'pending'
                                OR (
                                    item.status = 'claiming'
                                    AND item.claimed_at < NOW()
                                        - make_interval(secs => $1)
                                )
                            )
                      )
                    ORDER BY op.last_claimed_at NULLS FIRST, op.created_at,
                             op.operation_id
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
                    "###;

// document_batches.py:369
pub const CLAIM_NEXT_ITEM_1: &str = r###"
                    SELECT document_id
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                      AND (
                          status = 'pending'
                          OR (
                              status = 'claiming'
                              AND claimed_at < NOW()
                                  - make_interval(secs => $2)
                          )
                      )
                    ORDER BY ordinal
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
                    "###;

// document_batches.py:369
pub const CLAIM_NEXT_ITEM_2: &str = r###"
                    UPDATE knowledge.kb_document_batch_items
                    SET status = 'claiming', claimed_by = $3,
                        claimed_at = NOW(), error_code = NULL, error = NULL
                    WHERE operation_id = $1::uuid AND document_id = $2
                    "###;

// document_batches.py:369
pub const CLAIM_NEXT_ITEM_3: &str = r###"
                    UPDATE knowledge.kb_document_batch_operations
                    SET status = 'running', last_claimed_at = NOW(),
                        updated_at = NOW()
                    WHERE operation_id = $1::uuid
                    "###;

// document_batches.py:470
pub const COMPLETE_ITEM_0: &str = r###"
                    UPDATE knowledge.kb_document_batch_items
                    SET status = $3, error_code = $4, error = $5,
                        completed_at = NOW()
                    WHERE operation_id = $1::uuid
                      AND document_id = $2
                      AND status = 'claiming'
                    RETURNING 1
                    "###;

// document_batches.py:470
pub const COMPLETE_ITEM_1: &str = r###"
                    SELECT COUNT(*) FILTER (WHERE status = 'queued') AS queued,
                           COUNT(*) FILTER (WHERE status = 'skipped') AS skipped,
                           COUNT(*) FILTER (WHERE status = 'failed') AS failed,
                           COUNT(*) FILTER (
                               WHERE status IN ('pending', 'claiming')
                           ) AS remaining
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                    "###;

// document_batches.py:470
pub const COMPLETE_ITEM_2: &str = r###"
                    UPDATE knowledge.kb_document_batch_operations
                    SET status = CASE
                            WHEN error = 'cancelled by operator' THEN 'failed'
                            ELSE $2
                        END,
                        queued_count = $3, skipped_count = $4,
                        failed_count = $5, updated_at = NOW(),
                        completed_at = CASE
                            WHEN error = 'cancelled by operator'
                                THEN COALESCE(completed_at, NOW())
                            WHEN $6 = 0 THEN NOW()
                            ELSE NULL
                        END
                    WHERE operation_id = $1::uuid
                    "###;

// document_batches.py:551
pub const RELEASE_ITEM_0: &str = r###"
                UPDATE knowledge.kb_document_batch_items AS item
                SET status = CASE
                        WHEN operation.status = 'failed' THEN 'failed'
                        ELSE 'pending'
                    END,
                    claimed_by = NULL, claimed_at = NULL,
                    error_code = CASE
                        WHEN operation.status = 'failed' THEN 'cancelled'
                        ELSE NULL
                    END,
                    error = CASE
                        WHEN operation.status = 'failed' THEN operation.error
                        ELSE NULL
                    END,
                    completed_at = CASE
                        WHEN operation.status = 'failed' THEN NOW()
                        ELSE NULL
                    END
                FROM knowledge.kb_document_batch_operations AS operation
                WHERE item.operation_id = $1::uuid
                  AND item.document_id = $2
                  AND item.status = 'claiming'
                  AND operation.operation_id = item.operation_id
                "###;

// document_batches.py:551
pub const RELEASE_ITEM_1: &str = r###"
                UPDATE knowledge.kb_document_batch_operations AS operation
                SET failed_count = counts.failed, updated_at = NOW()
                FROM (
                    SELECT COUNT(*) FILTER (WHERE status = 'failed') AS failed
                    FROM knowledge.kb_document_batch_items
                    WHERE operation_id = $1::uuid
                ) AS counts
                WHERE operation.operation_id = $1::uuid
                  AND operation.status = 'failed'
                "###;
