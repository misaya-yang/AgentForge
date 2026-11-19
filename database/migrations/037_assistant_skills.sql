-- Migration: 037_assistant_skills.sql
-- Goal: Add dynamic skills registry/version/run tables

BEGIN;

CREATE TABLE IF NOT EXISTS assistant_skills (
    skill_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    name VARCHAR(128) NOT NULL,
    title VARCHAR(256) NOT NULL,
    description TEXT NOT NULL,
    tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    permissions JSONB NOT NULL DEFAULT '[]'::jsonb,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    created_by VARCHAR(64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, name)
);

CREATE INDEX IF NOT EXISTS idx_assistant_skills_tenant_user
    ON assistant_skills(tenant_id, user_id, status, updated_at DESC);

CREATE TABLE IF NOT EXISTS assistant_skill_versions (
    version_id UUID PRIMARY KEY,
    skill_id UUID NOT NULL REFERENCES assistant_skills(skill_id) ON DELETE CASCADE,
    version VARCHAR(32) NOT NULL,
    manifest JSONB NOT NULL,
    entrypoint TEXT NOT NULL,
    content TEXT,
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    approved_by VARCHAR(64),
    approved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (skill_id, version)
);

CREATE INDEX IF NOT EXISTS idx_assistant_skill_versions_skill
    ON assistant_skill_versions(skill_id, created_at DESC);

CREATE TABLE IF NOT EXISTS assistant_skill_runs (
    run_id UUID PRIMARY KEY,
    skill_id UUID NOT NULL REFERENCES assistant_skills(skill_id) ON DELETE CASCADE,
    version_id UUID REFERENCES assistant_skill_versions(version_id) ON DELETE SET NULL,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100),
    status VARCHAR(32) NOT NULL,
    input JSONB NOT NULL DEFAULT '{}'::jsonb,
    output JSONB,
    error TEXT,
    latency_ms INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_skill_runs_tenant_user
    ON assistant_skill_runs(tenant_id, user_id, created_at DESC);

COMMIT;
