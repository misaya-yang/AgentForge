-- Migration: 038_assistant_scheduler_audit.sql
-- Goal: Add scheduler jobs + audit event tables

BEGIN;

CREATE TABLE IF NOT EXISTS assistant_scheduler_jobs (
    job_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    lane VARCHAR(32) NOT NULL DEFAULT 'maintenance',
    job_type VARCHAR(64) NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(32) NOT NULL DEFAULT 'queued',
    retries INTEGER NOT NULL DEFAULT 0,
    max_retries INTEGER NOT NULL DEFAULT 5,
    run_at TIMESTAMPTZ NOT NULL,
    lease_expires_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_scheduler_jobs_lane_status
    ON assistant_scheduler_jobs(lane, status, run_at);

CREATE TABLE IF NOT EXISTS assistant_audit_events (
    event_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64),
    event_type VARCHAR(64) NOT NULL,
    actor_id VARCHAR(64),
    resource_type VARCHAR(64),
    resource_id VARCHAR(128),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_assistant_audit_events_tenant_created
    ON assistant_audit_events(tenant_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_assistant_audit_events_resource
    ON assistant_audit_events(resource_type, resource_id, created_at DESC);

COMMIT;
