-- Migration 049: Session list performance — composite index for sidebar query
-- Root cause: SELECT * fetches 6.9MB history JSONB for admin user (466 sessions)
-- This index covers the exact WHERE + ORDER BY pattern used by list_session_summaries()

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_sessions_user_tenant_status_updated
ON sessions (user_id, tenant_id, status, updated_at DESC);
