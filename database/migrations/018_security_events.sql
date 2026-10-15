-- Migration: 018_security_events.sql
-- Description: Add daily aggregates for auth failures and rate limit events
-- Date: 2026-01-20

CREATE TABLE IF NOT EXISTS security_event_daily_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64),
    service_id VARCHAR(64),
    event_type VARCHAR(32) NOT NULL, -- auth_failed, rate_limited
    date DATE NOT NULL,
    event_count INTEGER DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_security_event_daily_unique
ON security_event_daily_aggregates (
    tenant_id,
    COALESCE(user_id, ''),
    COALESCE(service_id, ''),
    event_type,
    date
);

CREATE INDEX IF NOT EXISTS idx_security_event_daily_tenant_date
ON security_event_daily_aggregates (tenant_id, date);

CREATE INDEX IF NOT EXISTS idx_security_event_daily_user_date
ON security_event_daily_aggregates (user_id, date);

CREATE INDEX IF NOT EXISTS idx_security_event_daily_service_date
ON security_event_daily_aggregates (service_id, date);

DROP TRIGGER IF EXISTS update_security_event_daily_aggregates_updated_at ON security_event_daily_aggregates;
CREATE TRIGGER update_security_event_daily_aggregates_updated_at
    BEFORE UPDATE ON security_event_daily_aggregates
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
