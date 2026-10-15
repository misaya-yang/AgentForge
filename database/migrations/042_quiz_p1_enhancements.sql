-- 042_quiz_p1_enhancements.sql
-- Quiz P1/P2 hardening: time limits, IP tracking, performance indexes
-- Date: 2026-04-01

SET client_encoding TO 'UTF8';

-- P1: Time limit per share link
ALTER TABLE quiz_shares ADD COLUMN IF NOT EXISTS time_limit_minutes INT;

-- P2: Track client IP on attempts
ALTER TABLE quiz_attempts ADD COLUMN IF NOT EXISTS client_ip VARCHAR(45);

-- Performance: index on share_id for attempt counting
CREATE INDEX IF NOT EXISTS idx_quiz_attempts_share ON quiz_attempts(share_id);

-- Performance: index for duplicate display_name check
CREATE INDEX IF NOT EXISTS idx_quiz_attempts_share_name ON quiz_attempts(share_id, display_name);
