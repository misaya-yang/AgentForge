
SELECT jsonb_build_object('unread_count', count(*) FILTER (WHERE e.notification_read_at IS NULL),
    'total_count', count(*)) FROM gateway.scheduled_task_executions e
WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.status IN ('succeeded','failed','cancelled')
