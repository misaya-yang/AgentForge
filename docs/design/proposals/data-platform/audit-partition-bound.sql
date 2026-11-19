-- SEPARATE short authority transaction after the cutover month is approved/frozen.
-- Commit this NOT VALID addition before the online validation phase, so its
-- ACCESS EXCLUSIVE lock is not held across a table scan.
SET LOCAL lock_timeout='2s';
SET LOCAL statement_timeout='60s';
SET LOCAL TIME ZONE 'UTC';
DO $bound$
DECLARE boundary timestamptz := current_setting('agentforge.audit_partition_cutover')::timestamptz;
BEGIN
  IF boundary<>date_trunc('month',boundary) THEN RAISE EXCEPTION 'UTC month boundary required'; END IF;
  EXECUTE format('ALTER TABLE gateway.audit_logs ADD CONSTRAINT audit_logs_legacy_bound CHECK(created_at<%L::timestamptz) NOT VALID',boundary);
END $bound$;
