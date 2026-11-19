
SELECT jsonb_build_object(
    'unread_count', (SELECT count(*) FROM gateway.scheduled_task_executions e
        WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.status IN ('succeeded','failed','cancelled') AND e.notification_read_at IS NULL),
    'notifications', COALESCE((SELECT jsonb_agg(p.document ORDER BY p.occurred_at DESC, p.execution_id DESC)
        FROM (SELECT e.execution_id, COALESCE(e.finished_at,e.updated_at) AS occurred_at,
            
jsonb_build_object(
    'notification_id', e.execution_id,
    'occurred_at', COALESCE(e.finished_at, e.updated_at),
    'read_at', e.notification_read_at,
    'task', jsonb_build_object('task_id', t.task_id, 'title', t.title, 'status', t.status),
    'execution', to_jsonb(e) - 'notification_read_at' - 'tenant_id' - 'user_id'
)
 AS document FROM gateway.scheduled_task_executions e JOIN gateway.scheduled_tasks t ON t.task_id=e.task_id AND t.tenant_id=e.tenant_id AND t.user_id=e.user_id
            WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.status IN ('succeeded','failed','cancelled')
                AND (NOT $3::boolean OR e.notification_read_at IS NULL)
                AND ($4::timestamptz IS NULL OR
                    (COALESCE(e.finished_at,e.updated_at),e.execution_id) < ($4::timestamptz,$5::uuid))
            ORDER BY COALESCE(e.finished_at,e.updated_at) DESC,e.execution_id DESC LIMIT $6
        ) p), '[]'::jsonb)
)
