
jsonb_build_object(
    'notification_id', e.execution_id,
    'occurred_at', COALESCE(e.finished_at, e.updated_at),
    'read_at', e.notification_read_at,
    'task', jsonb_build_object('task_id', t.task_id, 'title', t.title, 'status', t.status),
    'execution', to_jsonb(e) - 'notification_read_at' - 'tenant_id' - 'user_id'
)
