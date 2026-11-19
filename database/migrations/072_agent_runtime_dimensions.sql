-- 072 — Agent Studio runtime identity, pinning, and trace dimensions.
--
-- Additive only. Existing built-in Assistant rows retain NULL dimensions.
-- Agent Version content stays immutable; operational revocation is represented
-- by a separate append-only row rather than an UPDATE to agent_versions.

BEGIN;

CREATE TABLE IF NOT EXISTS agent_version_revocations (
    tenant_id VARCHAR(255) NOT NULL,
    agent_version_id UUID NOT NULL,
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_by VARCHAR(255) NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (tenant_id, agent_version_id),
    CONSTRAINT agent_version_revocations_version_fk
        FOREIGN KEY (tenant_id, agent_version_id)
        REFERENCES agent_versions(tenant_id, agent_version_id)
        ON DELETE RESTRICT
);

CREATE OR REPLACE FUNCTION agent_runtime_reject_revocation_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'AGENT_VERSION_REVOCATION_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_version_revocations_immutable
    ON agent_version_revocations;
CREATE TRIGGER agent_version_revocations_immutable
    BEFORE UPDATE OR DELETE ON agent_version_revocations
    FOR EACH ROW EXECUTE FUNCTION agent_runtime_reject_revocation_mutation();

ALTER TABLE sessions
    ADD COLUMN IF NOT EXISTS agent_id UUID,
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS agent_draft_revision INTEGER,
    ADD COLUMN IF NOT EXISTS publication_id UUID,
    ADD COLUMN IF NOT EXISTS channel VARCHAR(16),
    ADD COLUMN IF NOT EXISTS runtime_fingerprint TEXT,
    ADD COLUMN IF NOT EXISTS agent_spec_hash TEXT;

ALTER TABLE assistant_runs
    ADD COLUMN IF NOT EXISTS agent_id UUID,
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS agent_draft_revision INTEGER,
    ADD COLUMN IF NOT EXISTS publication_id UUID,
    ADD COLUMN IF NOT EXISTS channel VARCHAR(16),
    ADD COLUMN IF NOT EXISTS runtime_fingerprint TEXT,
    ADD COLUMN IF NOT EXISTS agent_spec_hash TEXT;

ALTER TABLE assistant_run_checkpoints
    ADD COLUMN IF NOT EXISTS agent_id UUID,
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS agent_draft_revision INTEGER,
    ADD COLUMN IF NOT EXISTS publication_id UUID,
    ADD COLUMN IF NOT EXISTS channel VARCHAR(16),
    ADD COLUMN IF NOT EXISTS runtime_fingerprint TEXT,
    ADD COLUMN IF NOT EXISTS agent_spec_hash TEXT;

ALTER TABLE agent_traces
    ADD COLUMN IF NOT EXISTS agent_id UUID,
    ADD COLUMN IF NOT EXISTS agent_version_id UUID,
    ADD COLUMN IF NOT EXISTS agent_draft_revision INTEGER,
    ADD COLUMN IF NOT EXISTS publication_id UUID,
    ADD COLUMN IF NOT EXISTS channel VARCHAR(16),
    ADD COLUMN IF NOT EXISTS runtime_fingerprint TEXT,
    ADD COLUMN IF NOT EXISTS agent_spec_hash TEXT;

DO $$
DECLARE
    target_table TEXT;
    target_name TEXT;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
        'sessions',
        'assistant_runs',
        'assistant_run_checkpoints',
        'agent_traces'
    ]
    LOOP
        target_name := target_table || '_agent_runtime_channel_check';
        IF NOT EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = target_name
              AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I CHECK '
                || '(channel IS NULL OR channel IN '
                || '(''preview'', ''hosted'', ''embed'', ''api'', ''builtin''))',
                target_table,
                target_name
            );
        END IF;

        target_name := target_table || '_agent_runtime_shape_check';
        IF NOT EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = target_name
              AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I CHECK ('
                || '(agent_id IS NULL AND agent_version_id IS NULL '
                || 'AND agent_draft_revision IS NULL AND publication_id IS NULL '
                || 'AND channel IS NULL AND runtime_fingerprint IS NULL '
                || 'AND agent_spec_hash IS NULL) OR '
                || '(agent_id IS NOT NULL AND channel IS NOT NULL '
                || 'AND runtime_fingerprint IS NOT NULL AND agent_spec_hash IS NOT NULL '
                || 'AND ((channel = ''preview'' AND agent_draft_revision IS NOT NULL '
                || 'AND agent_version_id IS NULL AND publication_id IS NULL) '
                || 'OR (channel <> ''preview'' AND agent_version_id IS NOT NULL))))',
                target_table,
                target_name
            );
        END IF;
    END LOOP;
END;
$$;

DO $$
DECLARE
    target_table TEXT;
    target_name TEXT;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
        'sessions',
        'assistant_runs',
        'assistant_run_checkpoints',
        'agent_traces'
    ]
    LOOP
        target_name := target_table || '_runtime_agent_fk';
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = target_name AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I '
                || 'FOREIGN KEY (tenant_id, agent_id) '
                || 'REFERENCES agents(tenant_id, agent_id) ON DELETE RESTRICT',
                target_table,
                target_name
            );
        END IF;

        target_name := target_table || '_runtime_version_fk';
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = target_name AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I '
                || 'FOREIGN KEY (tenant_id, agent_version_id) '
                || 'REFERENCES agent_versions(tenant_id, agent_version_id) ON DELETE RESTRICT',
                target_table,
                target_name
            );
        END IF;

        target_name := target_table || '_runtime_publication_fk';
        IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = target_name AND conrelid = target_table::regclass
        ) THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I '
                || 'FOREIGN KEY (tenant_id, publication_id) '
                || 'REFERENCES agent_publications(tenant_id, publication_id) ON DELETE RESTRICT',
                target_table,
                target_name
            );
        END IF;
    END LOOP;
END;
$$;

CREATE INDEX IF NOT EXISTS idx_sessions_agent_runtime
    ON sessions(tenant_id, agent_id, agent_version_id, channel, updated_at DESC)
    WHERE agent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_assistant_runs_agent_runtime
    ON assistant_runs(tenant_id, agent_id, agent_version_id, channel, created_at DESC)
    WHERE agent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_assistant_checkpoints_agent_runtime
    ON assistant_run_checkpoints(
        tenant_id, agent_id, agent_version_id, channel, created_at DESC
    ) WHERE agent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agent_traces_agent_runtime
    ON agent_traces(tenant_id, agent_id, agent_version_id, channel, created_at DESC)
    WHERE agent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agent_traces_runtime_fingerprint
    ON agent_traces(tenant_id, runtime_fingerprint, created_at DESC)
    WHERE runtime_fingerprint IS NOT NULL;

COMMIT;
