-- Migration: 036_assistant_queue_lanes.sql
-- Goal: Add lane + queue mode metadata for assistant command queue

BEGIN;

ALTER TABLE assistant_command_queue
    ADD COLUMN IF NOT EXISTS lane VARCHAR(32) NOT NULL DEFAULT 'main',
    ADD COLUMN IF NOT EXISTS queue_mode VARCHAR(32) NOT NULL DEFAULT 'collect',
    ADD COLUMN IF NOT EXISTS priority INTEGER NOT NULL DEFAULT 100,
    ADD COLUMN IF NOT EXISTS steer_payload JSONB;

CREATE INDEX IF NOT EXISTS idx_assistant_command_queue_lane_status
    ON assistant_command_queue(lane, status, priority, created_at);

CREATE INDEX IF NOT EXISTS idx_assistant_command_queue_queue_mode
    ON assistant_command_queue(queue_mode, created_at DESC);

COMMIT;
