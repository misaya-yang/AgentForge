
INSERT INTO usage_daily_aggregates (
    tenant_id,
    user_id,
    model,
    assistant_id,
    service_id,
    date,
    request_count,
    success_count,
    error_count,
    total_input_tokens,
    total_output_tokens,
    total_cost_cents,
    avg_latency_ms,
    avg_first_token_ms
)
SELECT
    tenant_id,
    COALESCE(user_id, '') AS user_id,
    COALESCE(model, '') AS model,
    COALESCE(assistant_id, '') AS assistant_id,
    COALESCE(service_id, '') AS service_id,
    created_at::date AS date,
    COUNT(*)::int AS request_count,
    SUM(CASE WHEN COALESCE(status, 'success') = 'success' THEN 1 ELSE 0 END)::int AS success_count,
    SUM(CASE WHEN COALESCE(status, 'success') = 'success' THEN 0 ELSE 1 END)::int AS error_count,
    COALESCE(SUM(input_tokens), 0)::bigint AS total_input_tokens,
    COALESCE(SUM(output_tokens), 0)::bigint AS total_output_tokens,
    COALESCE(SUM(input_cost_cents + output_cost_cents), 0)::bigint AS total_cost_cents,
    COALESCE(AVG(COALESCE(latency_ms, 0))::int, 0) AS avg_latency_ms,
    COALESCE(AVG(COALESCE(first_token_ms, 0))::int, 0) AS avg_first_token_ms
FROM usage_records
GROUP BY
    tenant_id,
    COALESCE(user_id, ''),
    COALESCE(model, ''),
    COALESCE(assistant_id, ''),
    COALESCE(service_id, ''),
    created_at::date;
