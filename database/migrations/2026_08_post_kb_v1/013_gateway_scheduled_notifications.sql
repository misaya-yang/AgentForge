-- Reuse the Gateway-owned terminal execution receipt as the in-app notification.
-- Only acknowledgement state is new. No duplicate payload, outbox or delivery worker.
-- Additive, forward-only. Existing scheduler binaries leave this nullable field alone.
ALTER TABLE gateway.scheduled_task_executions
    ADD COLUMN notification_read_at timestamptz,
    ADD CONSTRAINT scheduled_execution_notification_terminal
        CHECK (notification_read_at IS NULL OR status IN ('succeeded','failed','cancelled'));

-- Support tenant/owner keyset history and exact unread counts without a global scan.
CREATE INDEX scheduled_notification_history
    ON gateway.scheduled_task_executions
        (tenant_id,user_id,(COALESCE(finished_at,updated_at)) DESC,execution_id DESC)
    WHERE status IN ('succeeded','failed','cancelled');
CREATE INDEX scheduled_notification_unread
    ON gateway.scheduled_task_executions
        (tenant_id,user_id,(COALESCE(finished_at,updated_at)) DESC,execution_id DESC)
    WHERE status IN ('succeeded','failed','cancelled') AND notification_read_at IS NULL;

COMMENT ON COLUMN gateway.scheduled_task_executions.notification_read_at IS
    'Owner acknowledgement of this terminal execution receipt; NULL means unread. It does not attest to external delivery, output availability or execution success.';
