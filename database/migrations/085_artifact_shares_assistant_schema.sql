-- 085_artifact_shares_assistant_schema.sql
-- make migrate applies only database/migrations/*.sql. 083 created an
-- unqualified artifact_shares table (gateway/public depending on search_path)
-- while ArtifactShareManager writes assistant.artifact_shares. Adopt any
-- leftover table into the assistant schema and create it if still missing.

CREATE SCHEMA IF NOT EXISTS assistant;

DO $migrate$
BEGIN
    IF to_regclass('assistant.artifact_shares') IS NULL THEN
        IF to_regclass('public.artifact_shares') IS NOT NULL THEN
            ALTER TABLE public.artifact_shares SET SCHEMA assistant;
        ELSIF to_regclass('gateway.artifact_shares') IS NOT NULL THEN
            ALTER TABLE gateway.artifact_shares SET SCHEMA assistant;
        END IF;
    END IF;
END
$migrate$;

DO $migrate$
BEGIN
    IF to_regclass('assistant.artifact_share_submitters') IS NULL THEN
        IF to_regclass('public.artifact_share_submitters') IS NOT NULL THEN
            ALTER TABLE public.artifact_share_submitters SET SCHEMA assistant;
        ELSIF to_regclass('gateway.artifact_share_submitters') IS NOT NULL THEN
            ALTER TABLE gateway.artifact_share_submitters SET SCHEMA assistant;
        END IF;
    END IF;
END
$migrate$;

CREATE TABLE IF NOT EXISTS assistant.artifact_shares (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    share_code        VARCHAR(20) NOT NULL UNIQUE,
    kind              VARCHAR(32) NOT NULL DEFAULT 'quiz',
    title             TEXT NOT NULL DEFAULT '',
    payload           JSONB NOT NULL DEFAULT '{}'::jsonb,
    answer_keys       JSONB,
    tenant_id         VARCHAR(64),
    created_by        VARCHAR(128),
    is_active         BOOLEAN NOT NULL DEFAULT TRUE,
    max_attempts      INT,
    expires_at        TIMESTAMPTZ,
    require_name      BOOLEAN NOT NULL DEFAULT TRUE,
    time_limit_minutes INT,
    attempt_count     INT NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at        TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_artifact_shares_tenant ON assistant.artifact_shares (tenant_id);
CREATE INDEX IF NOT EXISTS idx_artifact_shares_kind ON assistant.artifact_shares (kind);
