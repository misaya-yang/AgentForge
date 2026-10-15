-- 056 — partial index backing `_find_active_command`'s dedup hotpath.
--
-- ADR-004 §Decision: `_find_active_command` in
-- The durable command lifecycle is moving to the single Agent Runtime.
-- from an in-memory dict scan to a single indexed SELECT. The existing
-- 034-era idx_assistant_command_queue_command_key on (command_key, status)
-- is a full-table index — this partial index is filtered to only
-- currently-active rows, so the dedup SELECT scans a much smaller tree.
--
-- Reference: plans/ADR-004-Run-Approval-State-Externalization.md §B
-- (Data model + §Verification GATE-ADR004-2).
--
-- Idempotent: IF NOT EXISTS guards both the index and its drop.

CREATE INDEX IF NOT EXISTS idx_assistant_command_queue_active_by_key
    ON assistant_command_queue (command_key)
    WHERE status IN ('queued', 'running', 'awaiting_approval');

COMMENT ON INDEX idx_assistant_command_queue_active_by_key IS
    'ADR-004: partial index for _find_active_command dedup hotpath. '
    'Keeps the index size O(active commands) rather than O(all rows ever).';
