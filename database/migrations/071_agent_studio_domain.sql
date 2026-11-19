-- Migration: 071_agent_studio_domain.sql
-- Goal: additive tenant-safe Agent Studio identity, Draft, Version, ACL,
-- Publication, audit-event and token primitives. Existing Assistant rows and
-- service_id semantics are intentionally untouched.

BEGIN;

-- Knowledge bindings require a tenant-qualified candidate key so child rows
-- cannot point at a Dataset owned by another tenant.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'datasets_tenant_dataset_key'
          AND conrelid = 'datasets'::regclass
    ) THEN
        ALTER TABLE datasets
            ADD CONSTRAINT datasets_tenant_dataset_key
            UNIQUE (tenant_id, dataset_id);
    END IF;
END;
$$;

-- ACL user principals must resolve inside the same tenant. The existing
-- globally unique user_id remains untouched; this candidate key permits
-- tenant-qualified foreign keys for Agent ownership and membership.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'users_tenant_user_key'
          AND conrelid = 'users'::regclass
    ) THEN
        ALTER TABLE users
            ADD CONSTRAINT users_tenant_user_key
            UNIQUE (tenant_id, user_id);
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS agents (
    tenant_id VARCHAR(255) NOT NULL,
    agent_id UUID NOT NULL DEFAULT gen_random_uuid(),
    slug VARCHAR(128) NOT NULL,
    name VARCHAR(255) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    owner_id VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'archived', 'deleted')),
    current_draft_id UUID,
    archived_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    created_by VARCHAR(255) NOT NULL,
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, agent_id),
    CONSTRAINT agents_tenant_agent_key UNIQUE (tenant_id, agent_id),
    CONSTRAINT agents_owner_user_fk
        FOREIGN KEY (tenant_id, owner_id)
        REFERENCES users(tenant_id, user_id)
        ON DELETE RESTRICT
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agents_owner_user_fk'
          AND conrelid = 'agents'::regclass
    ) THEN
        ALTER TABLE agents
            ADD CONSTRAINT agents_owner_user_fk
            FOREIGN KEY (tenant_id, owner_id)
            REFERENCES users(tenant_id, user_id)
            ON DELETE RESTRICT;
    END IF;
END;
$$;

CREATE UNIQUE INDEX IF NOT EXISTS uq_agents_tenant_slug_live
    ON agents(tenant_id, slug)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_agents_tenant_status_updated
    ON agents(tenant_id, status, updated_at DESC)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_agents_tenant_owner_updated
    ON agents(tenant_id, owner_id, updated_at DESC)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS agent_members (
    tenant_id VARCHAR(255) NOT NULL,
    agent_id UUID NOT NULL,
    principal_type VARCHAR(32) NOT NULL DEFAULT 'user'
        CHECK (principal_type IN ('user', 'group')),
    principal_id VARCHAR(255) NOT NULL,
    user_principal_id VARCHAR(255)
        GENERATED ALWAYS AS (
            CASE WHEN principal_type = 'user' THEN principal_id ELSE NULL END
        ) STORED,
    role VARCHAR(16) NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, agent_id, principal_type, principal_id),
    CONSTRAINT agent_members_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_members_user_fk
        FOREIGN KEY (tenant_id, user_principal_id)
        REFERENCES users(tenant_id, user_id)
        ON DELETE RESTRICT,
    -- No authoritative tenant group directory exists in AS-01. Keep the
    -- discriminator for forward compatibility, but reject group grants until
    -- a later additive migration can attach them to a real tenant registry.
    CONSTRAINT agent_members_group_fail_closed
        CHECK (principal_type = 'user')
);

ALTER TABLE agent_members
    ADD COLUMN IF NOT EXISTS user_principal_id VARCHAR(255)
    GENERATED ALWAYS AS (
        CASE WHEN principal_type = 'user' THEN principal_id ELSE NULL END
    ) STORED;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_members_user_fk'
          AND conrelid = 'agent_members'::regclass
    ) THEN
        ALTER TABLE agent_members
            ADD CONSTRAINT agent_members_user_fk
            FOREIGN KEY (tenant_id, user_principal_id)
            REFERENCES users(tenant_id, user_id)
            ON DELETE RESTRICT;
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agent_members_group_fail_closed'
          AND conrelid = 'agent_members'::regclass
    ) THEN
        ALTER TABLE agent_members
            ADD CONSTRAINT agent_members_group_fail_closed
            CHECK (principal_type = 'user');
    END IF;
END;
$$;

CREATE INDEX IF NOT EXISTS idx_agent_members_principal
    ON agent_members(tenant_id, principal_type, principal_id, role, agent_id);

CREATE TABLE IF NOT EXISTS agent_drafts (
    tenant_id VARCHAR(255) NOT NULL,
    draft_id UUID NOT NULL DEFAULT gen_random_uuid(),
    agent_id UUID NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    schema_version VARCHAR(64) NOT NULL DEFAULT 'agent-spec/v1',
    spec JSONB NOT NULL DEFAULT '{}'::jsonb,
    spec_hash CHAR(64) NOT NULL CHECK (spec_hash ~ '^[0-9a-f]{64}$'),
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, draft_id),
    CONSTRAINT agent_drafts_tenant_agent_unique UNIQUE (tenant_id, agent_id),
    CONSTRAINT agent_drafts_tenant_agent_draft_key
        UNIQUE (tenant_id, agent_id, draft_id),
    CONSTRAINT agent_drafts_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'agents_current_draft_fk'
          AND conrelid = 'agents'::regclass
    ) THEN
        ALTER TABLE agents
            ADD CONSTRAINT agents_current_draft_fk
            FOREIGN KEY (tenant_id, agent_id, current_draft_id)
            REFERENCES agent_drafts(tenant_id, agent_id, draft_id)
            DEFERRABLE INITIALLY DEFERRED;
    END IF;
END;
$$;

CREATE TABLE IF NOT EXISTS agent_draft_knowledge_bindings (
    tenant_id VARCHAR(255) NOT NULL,
    draft_id UUID NOT NULL,
    dataset_id VARCHAR(255) NOT NULL,
    retrieval_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, draft_id, dataset_id),
    CONSTRAINT agent_draft_knowledge_draft_fk
        FOREIGN KEY (tenant_id, draft_id)
        REFERENCES agent_drafts(tenant_id, draft_id)
        ON DELETE CASCADE,
    CONSTRAINT agent_draft_knowledge_dataset_fk
        FOREIGN KEY (tenant_id, dataset_id)
        REFERENCES datasets(tenant_id, dataset_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_draft_knowledge_dataset
    ON agent_draft_knowledge_bindings(tenant_id, dataset_id, draft_id);

CREATE TABLE IF NOT EXISTS agent_versions (
    tenant_id VARCHAR(255) NOT NULL,
    agent_version_id UUID NOT NULL DEFAULT gen_random_uuid(),
    agent_id UUID NOT NULL,
    version_number INTEGER NOT NULL CHECK (version_number >= 1),
    schema_version VARCHAR(64) NOT NULL DEFAULT 'agent-spec/v1',
    resolved_spec JSONB NOT NULL,
    spec_hash CHAR(64) NOT NULL CHECK (spec_hash ~ '^[0-9a-f]{64}$'),
    source_draft_id UUID NOT NULL,
    source_draft_revision INTEGER NOT NULL CHECK (source_draft_revision >= 1),
    bindings_sealed BOOLEAN NOT NULL DEFAULT FALSE,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, agent_version_id),
    CONSTRAINT agent_versions_tenant_agent_number_unique
        UNIQUE (tenant_id, agent_id, version_number),
    CONSTRAINT agent_versions_tenant_agent_version_key
        UNIQUE (tenant_id, agent_id, agent_version_id),
    CONSTRAINT agent_versions_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_versions_source_draft_fk
        FOREIGN KEY (tenant_id, agent_id, source_draft_id)
        REFERENCES agent_drafts(tenant_id, agent_id, draft_id)
        ON DELETE RESTRICT
);

-- Existing Version rows from an earlier AS-01 draft are treated as sealed;
-- newly inserted rows start open only inside their creating transaction.
ALTER TABLE agent_versions
    ADD COLUMN IF NOT EXISTS bindings_sealed BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE agent_versions
    ALTER COLUMN bindings_sealed SET DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_agent_versions_agent_created
    ON agent_versions(tenant_id, agent_id, version_number DESC);

CREATE TABLE IF NOT EXISTS agent_version_capabilities (
    tenant_id VARCHAR(255) NOT NULL,
    capability_id UUID NOT NULL DEFAULT gen_random_uuid(),
    agent_version_id UUID NOT NULL,
    capability_type VARCHAR(32) NOT NULL
        CHECK (capability_type IN ('native', 'model_native', 'mcp', 'skill', 'connector', 'knowledge')),
    resource_id VARCHAR(255) NOT NULL DEFAULT '',
    resource_version VARCHAR(255),
    schema_hash CHAR(64),
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, capability_id),
    CONSTRAINT agent_version_capabilities_unique
        UNIQUE (tenant_id, agent_version_id, capability_type, resource_id),
    CONSTRAINT agent_version_capabilities_version_fk
        FOREIGN KEY (tenant_id, agent_version_id)
        REFERENCES agent_versions(tenant_id, agent_version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_version_capabilities_schema_hash_check
        CHECK (schema_hash IS NULL OR schema_hash ~ '^[0-9a-f]{64}$')
);

CREATE INDEX IF NOT EXISTS idx_agent_version_capabilities_resource
    ON agent_version_capabilities(tenant_id, capability_type, resource_id);

CREATE TABLE IF NOT EXISTS agent_version_knowledge_bindings (
    tenant_id VARCHAR(255) NOT NULL,
    agent_version_id UUID NOT NULL,
    dataset_id VARCHAR(255) NOT NULL,
    retrieval_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, agent_version_id, dataset_id),
    CONSTRAINT agent_version_knowledge_version_fk
        FOREIGN KEY (tenant_id, agent_version_id)
        REFERENCES agent_versions(tenant_id, agent_version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_version_knowledge_dataset_fk
        FOREIGN KEY (tenant_id, dataset_id)
        REFERENCES datasets(tenant_id, dataset_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_version_knowledge_dataset
    ON agent_version_knowledge_bindings(tenant_id, dataset_id, agent_version_id);

CREATE TABLE IF NOT EXISTS agent_publications (
    tenant_id VARCHAR(255) NOT NULL,
    publication_id UUID NOT NULL DEFAULT gen_random_uuid(),
    agent_id UUID NOT NULL,
    channel VARCHAR(16) NOT NULL CHECK (channel IN ('hosted', 'embed', 'api')),
    public_id UUID NOT NULL DEFAULT gen_random_uuid(),
    version_id UUID,
    auth_mode VARCHAR(16) NOT NULL DEFAULT 'private'
        CHECK (auth_mode IN ('private', 'tenant', 'public', 'token')),
    policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(16) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'disabled', 'degraded')),
    created_by VARCHAR(255) NOT NULL,
    updated_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, publication_id),
    CONSTRAINT agent_publications_public_id_unique UNIQUE (public_id),
    CONSTRAINT agent_publications_tenant_agent_channel_unique
        UNIQUE (tenant_id, agent_id, channel),
    CONSTRAINT agent_publications_tenant_publication_agent_key
        UNIQUE (tenant_id, publication_id, agent_id),
    CONSTRAINT agent_publications_agent_fk
        FOREIGN KEY (tenant_id, agent_id)
        REFERENCES agents(tenant_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_publications_version_fk
        FOREIGN KEY (tenant_id, agent_id, version_id)
        REFERENCES agent_versions(tenant_id, agent_id, agent_version_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_publications_tenant_status
    ON agent_publications(tenant_id, status, updated_at DESC);

CREATE TABLE IF NOT EXISTS agent_publish_events (
    tenant_id VARCHAR(255) NOT NULL,
    event_id UUID NOT NULL DEFAULT gen_random_uuid(),
    publication_id UUID NOT NULL,
    agent_id UUID NOT NULL,
    from_version_id UUID,
    to_version_id UUID NOT NULL,
    actor_id VARCHAR(255) NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    validation_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, event_id),
    CONSTRAINT agent_publish_events_publication_fk
        FOREIGN KEY (tenant_id, publication_id, agent_id)
        REFERENCES agent_publications(tenant_id, publication_id, agent_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_publish_events_from_version_fk
        FOREIGN KEY (tenant_id, agent_id, from_version_id)
        REFERENCES agent_versions(tenant_id, agent_id, agent_version_id)
        ON DELETE RESTRICT,
    CONSTRAINT agent_publish_events_to_version_fk
        FOREIGN KEY (tenant_id, agent_id, to_version_id)
        REFERENCES agent_versions(tenant_id, agent_id, agent_version_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_publish_events_publication_created
    ON agent_publish_events(tenant_id, publication_id, created_at DESC);

CREATE TABLE IF NOT EXISTS agent_api_tokens (
    tenant_id VARCHAR(255) NOT NULL,
    token_id UUID NOT NULL DEFAULT gen_random_uuid(),
    publication_id UUID NOT NULL,
    token_hash CHAR(64) NOT NULL CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    name VARCHAR(255) NOT NULL,
    scopes TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_by VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, token_id),
    CONSTRAINT agent_api_tokens_hash_unique UNIQUE (token_hash),
    CONSTRAINT agent_api_tokens_publication_fk
        FOREIGN KEY (tenant_id, publication_id)
        REFERENCES agent_publications(tenant_id, publication_id)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_agent_api_tokens_publication_active
    ON agent_api_tokens(tenant_id, publication_id, created_at DESC)
    WHERE revoked_at IS NULL;

-- Version snapshots and their normalized bindings are write-once. Publish
-- events are append-only. Application rollback therefore preserves history.
CREATE OR REPLACE FUNCTION agent_studio_reject_immutable_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'AGENT_VERSION_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION agent_studio_protect_version_mutation()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'UPDATE'
       AND OLD.bindings_sealed = FALSE
       AND NEW.bindings_sealed = TRUE
       AND (TO_JSONB(NEW) - 'bindings_sealed')
           IS NOT DISTINCT FROM (TO_JSONB(OLD) - 'bindings_sealed') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'AGENT_VERSION_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION agent_studio_protect_version_binding_mutation()
RETURNS TRIGGER AS $$
DECLARE
    parent_sealed BOOLEAN;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'AGENT_VERSION_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;

    SELECT bindings_sealed INTO parent_sealed
    FROM agent_versions
    WHERE tenant_id = NEW.tenant_id
      AND agent_version_id = NEW.agent_version_id;

    -- A missing tenant-qualified parent must flow to the composite FK so the
    -- caller receives a foreign-key failure rather than an immutability error.
    IF parent_sealed IS NULL OR parent_sealed = FALSE THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'AGENT_VERSION_IMMUTABLE'
        USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION agent_studio_require_sealed_version()
RETURNS TRIGGER AS $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM agent_versions
        WHERE tenant_id = NEW.tenant_id
          AND agent_version_id = NEW.agent_version_id
          AND bindings_sealed = FALSE
    ) THEN
        RAISE EXCEPTION 'AGENT_VERSION_NOT_SEALED'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_versions_immutable ON agent_versions;
CREATE TRIGGER agent_versions_immutable
    BEFORE UPDATE OR DELETE ON agent_versions
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_version_mutation();

DROP TRIGGER IF EXISTS agent_versions_sealed_at_commit ON agent_versions;
CREATE CONSTRAINT TRIGGER agent_versions_sealed_at_commit
    AFTER INSERT OR UPDATE ON agent_versions
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION agent_studio_require_sealed_version();

DROP TRIGGER IF EXISTS agent_version_capabilities_immutable ON agent_version_capabilities;
CREATE TRIGGER agent_version_capabilities_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON agent_version_capabilities
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_version_binding_mutation();

DROP TRIGGER IF EXISTS agent_version_knowledge_immutable ON agent_version_knowledge_bindings;
CREATE TRIGGER agent_version_knowledge_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON agent_version_knowledge_bindings
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_version_binding_mutation();

DROP TRIGGER IF EXISTS agent_publish_events_immutable ON agent_publish_events;
CREATE TRIGGER agent_publish_events_immutable
    BEFORE UPDATE OR DELETE ON agent_publish_events
    FOR EACH ROW EXECUTE FUNCTION agent_studio_reject_immutable_mutation();

CREATE OR REPLACE FUNCTION agent_studio_protect_member_identity()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
       OR NEW.agent_id IS DISTINCT FROM OLD.agent_id
       OR NEW.principal_type IS DISTINCT FROM OLD.principal_type
       OR NEW.principal_id IS DISTINCT FROM OLD.principal_id THEN
        RAISE EXCEPTION 'AGENT_MEMBER_IDENTITY_IMMUTABLE'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_members_identity_immutable ON agent_members;
CREATE TRIGGER agent_members_identity_immutable
    BEFORE UPDATE ON agent_members
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_member_identity();

CREATE OR REPLACE FUNCTION agent_studio_protect_last_owner()
RETURNS TRIGGER AS $$
DECLARE
    owner_count INTEGER;
    replacement_owner_id VARCHAR(255);
BEGIN
    IF OLD.role <> 'owner' THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' AND NEW.role = 'owner' THEN
        RETURN NEW;
    END IF;

    PERFORM 1
    FROM agents
    WHERE tenant_id = OLD.tenant_id AND agent_id = OLD.agent_id
    FOR UPDATE;

    SELECT COUNT(*) INTO owner_count
    FROM agent_members
    WHERE tenant_id = OLD.tenant_id
      AND agent_id = OLD.agent_id
      AND role = 'owner';

    IF owner_count <= 1 THEN
        RAISE EXCEPTION 'AGENT_LAST_OWNER'
            USING ERRCODE = '23514';
    END IF;
    SELECT principal_id INTO replacement_owner_id
    FROM agent_members
    WHERE tenant_id = OLD.tenant_id
      AND agent_id = OLD.agent_id
      AND role = 'owner'
      AND NOT (
          principal_type = OLD.principal_type
          AND principal_id = OLD.principal_id
      )
    ORDER BY created_at, principal_id
    LIMIT 1;
    UPDATE agents
    SET owner_id = replacement_owner_id
    WHERE tenant_id = OLD.tenant_id
      AND agent_id = OLD.agent_id
      AND owner_id = OLD.principal_id;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_members_last_owner ON agent_members;
CREATE TRIGGER agent_members_last_owner
    BEFORE DELETE OR UPDATE OF role ON agent_members
    FOR EACH ROW EXECUTE FUNCTION agent_studio_protect_last_owner();

CREATE OR REPLACE FUNCTION agent_studio_assert_owner_invariant()
RETURNS TRIGGER AS $$
DECLARE
    target_tenant_id VARCHAR(255);
    target_agent_id UUID;
    current_owner_id VARCHAR(255);
BEGIN
    IF TG_TABLE_NAME = 'agents' THEN
        target_tenant_id := COALESCE(NEW.tenant_id, OLD.tenant_id);
        target_agent_id := COALESCE(NEW.agent_id, OLD.agent_id);
    ELSE
        target_tenant_id := COALESCE(NEW.tenant_id, OLD.tenant_id);
        target_agent_id := COALESCE(NEW.agent_id, OLD.agent_id);
    END IF;

    SELECT owner_id INTO current_owner_id
    FROM agents
    WHERE tenant_id = target_tenant_id
      AND agent_id = target_agent_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM agent_members
        WHERE tenant_id = target_tenant_id
          AND agent_id = target_agent_id
          AND principal_type = 'user'
          AND principal_id = current_owner_id
          AND role = 'owner'
    ) THEN
        RAISE EXCEPTION 'AGENT_OWNER_INVARIANT'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agents_owner_invariant ON agents;
CREATE CONSTRAINT TRIGGER agents_owner_invariant
    AFTER INSERT OR UPDATE OR DELETE ON agents
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION agent_studio_assert_owner_invariant();

DROP TRIGGER IF EXISTS agent_members_owner_invariant ON agent_members;
CREATE CONSTRAINT TRIGGER agent_members_owner_invariant
    AFTER INSERT OR UPDATE OR DELETE ON agent_members
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION agent_studio_assert_owner_invariant();

CREATE OR REPLACE FUNCTION agent_studio_touch_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agents_touch_updated_at ON agents;
CREATE TRIGGER agents_touch_updated_at
    BEFORE UPDATE ON agents
    FOR EACH ROW EXECUTE FUNCTION agent_studio_touch_updated_at();

DROP TRIGGER IF EXISTS agent_drafts_touch_updated_at ON agent_drafts;
CREATE TRIGGER agent_drafts_touch_updated_at
    BEFORE UPDATE ON agent_drafts
    FOR EACH ROW EXECUTE FUNCTION agent_studio_touch_updated_at();

DROP TRIGGER IF EXISTS agent_members_touch_updated_at ON agent_members;
CREATE TRIGGER agent_members_touch_updated_at
    BEFORE UPDATE ON agent_members
    FOR EACH ROW EXECUTE FUNCTION agent_studio_touch_updated_at();

DROP TRIGGER IF EXISTS agent_publications_touch_updated_at ON agent_publications;
CREATE TRIGGER agent_publications_touch_updated_at
    BEFORE UPDATE ON agent_publications
    FOR EACH ROW EXECUTE FUNCTION agent_studio_touch_updated_at();

COMMIT;
