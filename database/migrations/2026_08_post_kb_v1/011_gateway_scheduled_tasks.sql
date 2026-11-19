-- Gateway owns scheduling policy and dispatch receipts; Runtime owns Agent execution.
-- Additive, forward-only. No legacy orphaned scheduler or frozen baseline changes.
CREATE TABLE gateway.scheduled_tasks (
    task_id uuid PRIMARY KEY,
    tenant_id varchar(255) NOT NULL CHECK (length(btrim(tenant_id)) > 0),
    user_id varchar(255) NOT NULL CHECK (length(btrim(user_id)) > 0),
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 120),
    message text NOT NULL CHECK (length(message) BETWEEN 1 AND 200000),
    model_id varchar(255) NOT NULL,
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active','paused','completed','cancelled')),
    next_run_at timestamptz NOT NULL,
    interval_seconds bigint CHECK (interval_seconds BETWEEN 60 AND 31536000),
    max_retries integer NOT NULL DEFAULT 0 CHECK (max_retries BETWEEN 0 AND 3),
    retry_count integer NOT NULL DEFAULT 0 CHECK (retry_count BETWEEN 0 AND 4),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (task_id,tenant_id,user_id)
);
CREATE INDEX scheduled_tasks_due ON gateway.scheduled_tasks(next_run_at) WHERE status='active';
CREATE INDEX scheduled_tasks_owner ON gateway.scheduled_tasks(tenant_id,user_id,created_at DESC);
CREATE TABLE gateway.scheduled_task_executions (
    execution_id uuid PRIMARY KEY,
    task_id uuid NOT NULL,
    tenant_id varchar(255) NOT NULL,
    user_id varchar(255) NOT NULL,
    session_id varchar(255) NOT NULL UNIQUE,
    run_id uuid,
    runtime_thread_id uuid,
    scheduled_at timestamptz NOT NULL,
    attempt integer NOT NULL CHECK (attempt BETWEEN 0 AND 3),
    status text NOT NULL CHECK (status IN ('preparing','dispatching','running','uncertain','succeeded','failed','cancelled')),
    after_sequence bigint NOT NULL DEFAULT 0 CHECK (after_sequence >= 0),
    cancel_requested boolean NOT NULL DEFAULT false,
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    FOREIGN KEY(task_id,tenant_id,user_id) REFERENCES gateway.scheduled_tasks(task_id,tenant_id,user_id)
);
CREATE INDEX scheduled_execution_history ON gateway.scheduled_task_executions(task_id,created_at DESC);
CREATE UNIQUE INDEX scheduled_execution_one_active ON gateway.scheduled_task_executions(task_id)
    WHERE status IN ('preparing','dispatching','running','uncertain');
REVOKE ALL ON gateway.scheduled_tasks, gateway.scheduled_task_executions FROM PUBLIC;
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Scheduled tasks migration requires the authority owner role';
    END IF;
    EXECUTE format('GRANT SELECT, INSERT, UPDATE ON gateway.scheduled_tasks, gateway.scheduled_task_executions TO %I', prefix || 'gateway');
END
$permissions$;
