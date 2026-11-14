-- Canonical successor 032. Runs only inside the native authority transaction.
DO $owner$ BEGIN
  IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR EXISTS(SELECT 1 FROM pg_roles WHERE rolname=current_user AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)) THEN RAISE EXCEPTION 'canonical owner required'; END IF;
END $owner$;
ALTER TABLE gateway.scheduled_tasks ADD COLUMN cron_expression text, ADD COLUMN timezone text NOT NULL DEFAULT 'UTC';
ALTER TABLE gateway.scheduled_tasks ADD CONSTRAINT scheduled_tasks_cron_interval_exclusive CHECK (cron_expression IS NULL OR interval_seconds IS NULL);
CREATE TABLE gateway.automation_rules (
  automation_id uuid PRIMARY KEY,
  tenant_id varchar(255) NOT NULL,
  user_id varchar(255) NOT NULL,
  title text NOT NULL CHECK (length(title) BETWEEN 1 AND 120),
  definition jsonb NOT NULL CHECK (jsonb_typeof(definition)='object'),
  revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
  status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','active','paused','cancelled')),
  webhook_secret_ciphertext text CHECK (webhook_secret_ciphertext IS NULL OR webhook_secret_ciphertext LIKE 'enc:%'),
  next_run_at timestamptz,
  watermark text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX automation_rules_owner ON gateway.automation_rules(tenant_id,user_id,created_at DESC,automation_id);
CREATE INDEX automation_rules_due ON gateway.automation_rules(next_run_at) WHERE status='active';
CREATE TABLE gateway.automation_deliveries (
  delivery_id uuid PRIMARY KEY,
  automation_id uuid NOT NULL REFERENCES gateway.automation_rules(automation_id),
  tenant_id varchar(255) NOT NULL,
  user_id varchar(255) NOT NULL,
  event_key text NOT NULL CHECK (length(event_key) BETWEEN 1 AND 200),
  action_index integer NOT NULL CHECK (action_index >= 0),
  action jsonb NOT NULL,
  status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','dispatching','running','succeeded','failed','uncertain')),
  result jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE(automation_id,event_key,action_index)
);
CREATE INDEX automation_delivery_claim ON gateway.automation_deliveries(created_at,delivery_id) WHERE status='queued';
CREATE TABLE gateway.automation_notifications (
  notification_id uuid PRIMARY KEY,
  tenant_id varchar(255) NOT NULL,
  user_id varchar(255) NOT NULL,
  message text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
REVOKE ALL ON gateway.automation_rules,gateway.automation_deliveries,gateway.automation_notifications FROM PUBLIC;
DO $grants$ DECLARE gateway_role text := left(current_user,length(current_user)-length('owner'))||'gateway'; BEGIN
  EXECUTE format('GRANT SELECT,INSERT,UPDATE ON gateway.automation_rules,gateway.automation_deliveries,gateway.automation_notifications TO %I', gateway_role);
END $grants$;
