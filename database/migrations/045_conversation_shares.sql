-- 045_conversation_shares.sql
-- Share assistant conversations with artifacts as public read-only snapshots
-- Date: 2026-04-02

SET client_encoding TO 'UTF8';

CREATE TABLE IF NOT EXISTS conversation_shares (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    share_code       VARCHAR(12) UNIQUE NOT NULL,
    session_id       VARCHAR(255) NOT NULL,
    user_id          VARCHAR(255) NOT NULL,
    tenant_id        VARCHAR(255) NOT NULL DEFAULT '',
    title            VARCHAR(500),
    -- Immutable snapshot frozen at share time
    snapshot         JSONB NOT NULL,
    message_count    INT DEFAULT 0,
    artifact_count   INT DEFAULT 0,
    is_active        BOOLEAN DEFAULT TRUE,
    view_count       INT DEFAULT 0,
    expires_at       TIMESTAMPTZ,
    created_at       TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_cs_share_code ON conversation_shares(share_code) WHERE is_active = TRUE;
CREATE INDEX IF NOT EXISTS idx_cs_user ON conversation_shares(user_id, tenant_id);
CREATE INDEX IF NOT EXISTS idx_cs_session ON conversation_shares(session_id);
