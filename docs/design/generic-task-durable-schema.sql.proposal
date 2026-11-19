-- UNNUMBERED, NON-ACTIVE CANONICAL MIGRATION PROPOSAL. DO NOT EXECUTE DIRECTLY.
-- The consolidated successor plan places this after018 metadata reader and019
-- Knowledge trigger repair, as candidate020. Sequence allocation and canonical
-- fingerprint measurement remain blocked; this proposal is absent from discovery.
-- Executor after explicit approval: the canonical migrator under its owner role.

DO $owner$
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user <> (SELECT pg_get_userbyid(nspowner)
                           FROM pg_namespace WHERE nspname='gateway')
       OR EXISTS (SELECT 1 FROM pg_roles WHERE rolname=current_user
                  AND (rolcanlogin OR rolsuper OR rolcreaterole OR rolcreatedb
                       OR rolreplication OR rolbypassrls)) THEN
        RAISE EXCEPTION 'Task delivery migration requires canonical owner';
    END IF;
END
$owner$;

CREATE TABLE gateway.task_deliveries (
    task_id varchar(255) PRIMARY KEY REFERENCES gateway.tasks(task_id),
    request_id varchar(255) NOT NULL CHECK (request_id <> ''),
    service_id varchar(255) NOT NULL CHECK (service_id <> ''),
    user_id varchar(255) NOT NULL CHECK (user_id <> ''),
    tenant_id varchar(255) NOT NULL CHECK (tenant_id <> ''),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    priority integer NOT NULL,
    state text NOT NULL CHECK (state IN (
        'pending', 'claimed_unknown', 'reconciliation_required',
        'completed', 'failed', 'cancelled'
    )),
    claim_id uuid UNIQUE,
    admitted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    claimed_at timestamptz,
    finished_at timestamptz,
    reconciliation_reason text CHECK (reconciliation_reason IN (
        'invalid_envelope', 'task_projection_conflict'
    )),
    CHECK (task_id <> ''),
    CHECK ((claim_id IS NULL) = (claimed_at IS NULL)),
    CHECK ((state IN ('completed', 'failed', 'cancelled')) = (finished_at IS NOT NULL)),
    CHECK (state NOT IN ('pending', 'reconciliation_required') OR claim_id IS NULL),
    CHECK (state NOT IN ('claimed_unknown', 'completed', 'failed') OR claim_id IS NOT NULL),
    CHECK ((state = 'reconciliation_required') = (reconciliation_reason IS NOT NULL))
);
CREATE INDEX task_deliveries_pending_order
    ON gateway.task_deliveries (priority DESC, admitted_at, task_id)
    WHERE state = 'pending';
COMMENT ON TABLE gateway.task_deliveries IS 'gateway.generic-task-delivery/v1';

REVOKE ALL ON gateway.task_deliveries FROM PUBLIC;
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
DECLARE suffix text;
BEGIN
    -- Remove every application/default privilege before adding exact Gateway
    -- rights. Never grant access through the shared legacy tasks projection.
    FOREACH suffix IN ARRAY ARRAY['gateway','runtime','capability_worker','knowledge_api','knowledge_worker'] LOOP
        EXECUTE format('REVOKE ALL ON gateway.task_deliveries FROM %I', prefix || suffix);
    END LOOP;
    EXECUTE format('GRANT SELECT, INSERT, UPDATE ON gateway.task_deliveries TO %I', prefix || 'gateway');
END
$permissions$;
-- No app DELETE/TRUNCATE/REFERENCES/TRIGGER, sequence, function EXECUTE,
-- membership, default privileges, schema CREATE or API-key changes.
