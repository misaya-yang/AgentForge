
WITH ranked AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY
                tenant_id,
                COALESCE(user_id, ''),
                COALESCE(request_id, ''),
                COALESCE(service_id, ''),
                COALESCE(assistant_id, ''),
                model,
                COALESCE(provider, ''),
                input_tokens,
                output_tokens,
                input_cost_cents,
                output_cost_cents,
                COALESCE(latency_ms, 0),
                COALESCE(first_token_ms, 0),
                COALESCE(status, ''),
                COALESCE(request_type, ''),
                COALESCE(metadata::text, '{}'),
                created_at
            ORDER BY id
        ) AS rn
    FROM usage_records
),
deleted AS (
    DELETE FROM usage_records u
    USING ranked r
    WHERE u.id = r.id
      AND r.rn > 1
    RETURNING 1
)
SELECT COUNT(*)::bigint FROM deleted;
