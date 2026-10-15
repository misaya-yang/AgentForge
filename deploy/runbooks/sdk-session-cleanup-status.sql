-- Read-only operator diagnostic on the authorized Gateway database. No credentials.
-- Pending is logical deletion awaiting physical worker acknowledgement.
-- Rows are bounded; this is not an exact total count on a large backlog.
SELECT tenant_id,user_id,session_id,service_id,framework,attempts,
       deletion_requested_at,next_attempt_at,
       CASE WHEN attempts > 0 THEN 'retrying_or_previous_unknown' ELSE 'awaiting_delivery' END AS delivery_state
FROM gateway.sdk_session_scopes
WHERE state='pending'
ORDER BY next_attempt_at
LIMIT 100;
