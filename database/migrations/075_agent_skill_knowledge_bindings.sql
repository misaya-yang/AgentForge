-- Migration: 075_agent_skill_knowledge_bindings.sql
-- Goal: immutable tenant instruction Skill versions, exact Agent Skill
-- bindings, and explicit live-content Knowledge authorization metadata.

BEGIN;

ALTER TABLE assistant_skills
    ADD COLUMN IF NOT EXISTS artifact_type VARCHAR(32) NOT NULL DEFAULT 'legacy',
    ADD COLUMN IF NOT EXISTS disabled_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

-- The legacy key prevented two users in one tenant from owning same-named
-- private Skills.  Authorization is tenant + user, so the candidate key must
-- carry both dimensions.
ALTER TABLE assistant_skills
    DROP CONSTRAINT IF EXISTS assistant_skills_tenant_id_name_key;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skills_tenant_user_name_key'
          AND conrelid = 'assistant_skills'::regclass
    ) THEN
        ALTER TABLE assistant_skills
            ADD CONSTRAINT assistant_skills_tenant_user_name_key
            UNIQUE (tenant_id, user_id, name);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skills_tenant_skill_key'
          AND conrelid = 'assistant_skills'::regclass
    ) THEN
        ALTER TABLE assistant_skills
            ADD CONSTRAINT assistant_skills_tenant_skill_key
            UNIQUE (tenant_id, skill_id);
    END IF;
END;
$$;

ALTER TABLE assistant_skill_versions
    ADD COLUMN IF NOT EXISTS tenant_id VARCHAR(64),
    ADD COLUMN IF NOT EXISTS user_id VARCHAR(64),
    ADD COLUMN IF NOT EXISTS revision INTEGER,
    ADD COLUMN IF NOT EXISTS content_hash CHAR(64),
    ADD COLUMN IF NOT EXISTS artifact_type VARCHAR(32) NOT NULL DEFAULT 'legacy',
    ADD COLUMN IF NOT EXISTS created_by VARCHAR(64);

UPDATE assistant_skill_versions AS version
SET tenant_id = skill.tenant_id,
    user_id = skill.user_id
FROM assistant_skills AS skill
WHERE version.skill_id = skill.skill_id
  AND (version.tenant_id IS NULL OR version.user_id IS NULL);

WITH ranked AS (
    SELECT version_id,
           ROW_NUMBER() OVER (PARTITION BY skill_id ORDER BY created_at, version_id) AS value
    FROM assistant_skill_versions
)
UPDATE assistant_skill_versions AS version
SET revision = ranked.value
FROM ranked
WHERE version.version_id = ranked.version_id
  AND version.revision IS NULL;

UPDATE assistant_skill_versions
SET content_hash = ENCODE(DIGEST(COALESCE(content, ''), 'sha256'), 'hex')
WHERE content_hash IS NULL;

ALTER TABLE assistant_skill_versions
    ALTER COLUMN tenant_id SET NOT NULL,
    ALTER COLUMN user_id SET NOT NULL,
    ALTER COLUMN revision SET NOT NULL,
    ALTER COLUMN content_hash SET NOT NULL;

ALTER TABLE assistant_skill_versions
    DROP CONSTRAINT IF EXISTS assistant_skill_versions_skill_id_version_key;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skill_versions_skill_revision_key'
          AND conrelid = 'assistant_skill_versions'::regclass
    ) THEN
        ALTER TABLE assistant_skill_versions
            ADD CONSTRAINT assistant_skill_versions_skill_revision_key
            UNIQUE (skill_id, revision);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skill_versions_tenant_version_key'
          AND conrelid = 'assistant_skill_versions'::regclass
    ) THEN
        ALTER TABLE assistant_skill_versions
            ADD CONSTRAINT assistant_skill_versions_tenant_version_key
            UNIQUE (tenant_id, version_id);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skill_versions_tenant_skill_version_key'
          AND conrelid = 'assistant_skill_versions'::regclass
    ) THEN
        ALTER TABLE assistant_skill_versions
            ADD CONSTRAINT assistant_skill_versions_tenant_skill_version_key
            UNIQUE (tenant_id, skill_id, version_id);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skill_versions_tenant_skill_fk'
          AND conrelid = 'assistant_skill_versions'::regclass
    ) THEN
        ALTER TABLE assistant_skill_versions
            ADD CONSTRAINT assistant_skill_versions_tenant_skill_fk
            FOREIGN KEY (tenant_id, skill_id)
            REFERENCES assistant_skills(tenant_id, skill_id)
            ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skill_versions_content_hash_check'
          AND conrelid = 'assistant_skill_versions'::regclass
    ) THEN
        ALTER TABLE assistant_skill_versions
            ADD CONSTRAINT assistant_skill_versions_content_hash_check
            CHECK (content_hash ~ '^[0-9a-f]{64}$');
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'assistant_skill_versions_tenant_entrypoint_check'
          AND conrelid = 'assistant_skill_versions'::regclass
    ) THEN
        ALTER TABLE assistant_skill_versions
            ADD CONSTRAINT assistant_skill_versions_tenant_entrypoint_check
            CHECK (
                artifact_type <> 'tenant_instruction'
                OR entrypoint = 'db://' || skill_id::text || '/' || version_id::text
            );
    END IF;
END;
$$;

CREATE INDEX IF NOT EXISTS idx_assistant_skill_versions_scope
    ON assistant_skill_versions(tenant_id, user_id, version_id, status);

CREATE OR REPLACE FUNCTION agent_skill_reject_version_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'SKILL_VERSION_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS assistant_skill_versions_immutable
    ON assistant_skill_versions;
CREATE TRIGGER assistant_skill_versions_immutable
    BEFORE UPDATE OR DELETE ON assistant_skill_versions
    FOR EACH ROW EXECUTE FUNCTION agent_skill_reject_version_mutation();

CREATE TABLE IF NOT EXISTS assistant_skill_version_revocations (
    tenant_id VARCHAR(64) NOT NULL,
    version_id UUID NOT NULL,
    revoked_by VARCHAR(64) NOT NULL,
    reason VARCHAR(255) NOT NULL DEFAULT '',
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, version_id),
    CONSTRAINT assistant_skill_version_revocations_version_fk
        FOREIGN KEY (tenant_id, version_id)
        REFERENCES assistant_skill_versions(tenant_id, version_id)
        ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS agent_draft_skill_bindings (
    tenant_id VARCHAR(255) NOT NULL,
    draft_id UUID NOT NULL,
    skill_id UUID NOT NULL,
    skill_version_id UUID NOT NULL,
    skill_name VARCHAR(128) NOT NULL,
    bound_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, draft_id, skill_version_id),
    CONSTRAINT agent_draft_skill_name_key
        UNIQUE (tenant_id, draft_id, skill_name),
    CONSTRAINT agent_draft_skill_draft_fk
        FOREIGN KEY (tenant_id, draft_id)
        REFERENCES agent_drafts(tenant_id, draft_id)
        ON DELETE CASCADE,
    CONSTRAINT agent_draft_skill_skill_fk
        FOREIGN KEY (tenant_id, skill_id)
        REFERENCES assistant_skills(tenant_id, skill_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_draft_skill_version_fk
        FOREIGN KEY (tenant_id, skill_id, skill_version_id)
        REFERENCES assistant_skill_versions(tenant_id, skill_id, version_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_draft_skill_reverse
    ON agent_draft_skill_bindings(tenant_id, skill_version_id, draft_id);

CREATE TABLE IF NOT EXISTS agent_version_skill_bindings (
    tenant_id VARCHAR(255) NOT NULL,
    agent_version_id UUID NOT NULL,
    skill_id UUID NOT NULL,
    skill_version_id UUID NOT NULL,
    skill_name VARCHAR(128) NOT NULL,
    content_hash CHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, agent_version_id, skill_version_id),
    CONSTRAINT agent_version_skill_name_key
        UNIQUE (tenant_id, agent_version_id, skill_name),
    CONSTRAINT agent_version_skill_version_fk
        FOREIGN KEY (tenant_id, agent_version_id)
        REFERENCES agent_versions(tenant_id, agent_version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_version_skill_skill_fk
        FOREIGN KEY (tenant_id, skill_id)
        REFERENCES assistant_skills(tenant_id, skill_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_version_skill_artifact_fk
        FOREIGN KEY (tenant_id, skill_id, skill_version_id)
        REFERENCES assistant_skill_versions(tenant_id, skill_id, version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_version_skill_content_hash_check
        CHECK (content_hash ~ '^[0-9a-f]{64}$')
);

CREATE INDEX IF NOT EXISTS idx_agent_version_skill_reverse
    ON agent_version_skill_bindings(tenant_id, skill_version_id, agent_version_id);

DROP TRIGGER IF EXISTS agent_version_skill_immutable
    ON agent_version_skill_bindings;
CREATE TRIGGER agent_version_skill_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON agent_version_skill_bindings
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_version_binding_mutation();

ALTER TABLE agent_draft_knowledge_bindings
    ADD COLUMN IF NOT EXISTS bound_by VARCHAR(255),
    ADD COLUMN IF NOT EXISTS authorization_checked_at TIMESTAMPTZ;

ALTER TABLE agent_version_knowledge_bindings
    ADD COLUMN IF NOT EXISTS bound_by VARCHAR(255),
    ADD COLUMN IF NOT EXISTS authorization_checked_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS content_mode VARCHAR(32) NOT NULL DEFAULT 'live_latest',
    ADD COLUMN IF NOT EXISTS historical_replayable BOOLEAN NOT NULL DEFAULT FALSE;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'agent_version_knowledge_live_content_check'
          AND conrelid = 'agent_version_knowledge_bindings'::regclass
    ) THEN
        ALTER TABLE agent_version_knowledge_bindings
            ADD CONSTRAINT agent_version_knowledge_live_content_check
            CHECK (content_mode = 'live_latest' AND historical_replayable = FALSE);
    END IF;
END;
$$;

COMMIT;
