-- AUTHORITY-ONLY ONLINE VALIDATION after bounded backfill and before short cutover.
-- SHARE UPDATE EXCLUSIVE validation permits ordinary reads/inserts; record timing.
SET LOCAL lock_timeout='2s';
SET LOCAL statement_timeout='30min';
ALTER TABLE gateway.audit_logs VALIDATE CONSTRAINT audit_logs_partition_ready_identity_fk;
ALTER TABLE gateway.audit_logs VALIDATE CONSTRAINT audit_logs_legacy_bound;
-- last_id=NULL terminates backfill; rows_copied=0 alone does not (concurrent inserts
-- may already have registered an entire backfill batch). Verify each range and FK.
