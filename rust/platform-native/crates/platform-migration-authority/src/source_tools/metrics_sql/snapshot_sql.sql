
WITH dup AS (
    SELECT request_id, COUNT(*) AS cnt
    FROM usage_records
    WHERE request_id IS NOT NULL
    GROUP BY request_id
    HAVING COUNT(*) > 1
),
agg AS (
    SELECT
        (SELECT COUNT(*)::bigint FROM usage_records) AS usage_records,
        (SELECT COUNT(*)::bigint FROM usage_daily_aggregates) AS usage_daily_rows,
        (SELECT COUNT(*)::bigint FROM usage_hourly_aggregates) AS usage_hourly_rows,
        (SELECT COALESCE(SUM(request_count), 0)::bigint FROM usage_daily_aggregates) AS daily_request_sum,
        (SELECT COALESCE(SUM(request_count), 0)::bigint FROM usage_hourly_aggregates) AS hourly_request_sum
)
SELECT
    agg.usage_records,
    agg.usage_daily_rows,
    agg.usage_hourly_rows,
    agg.daily_request_sum,
    agg.hourly_request_sum,
    COALESCE((SELECT COUNT(*)::bigint FROM dup), 0) AS duplicated_request_ids,
    COALESCE((SELECT SUM(cnt - 1)::bigint FROM dup), 0) AS duplicate_extra_rows
FROM agg;
