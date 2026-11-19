-- Separate NONTRANSACTIONAL authority-owned online preparation phase.
-- Never concatenate this file into the normal transactional successor runner.
SET lock_timeout = '2s';
SET statement_timeout = '30min';
CREATE UNIQUE INDEX CONCURRENTLY audit_logs_partition_ready_id_time
  ON gateway.audit_logs(id,created_at);
CREATE INDEX CONCURRENTLY audit_logs_partition_ready_created_brin ON gateway.audit_logs USING brin(created_at);
CREATE INDEX CONCURRENTLY audit_logs_partition_ready_tenant_time ON gateway.audit_logs(tenant_id,created_at DESC,id DESC);
CREATE INDEX CONCURRENTLY audit_logs_partition_ready_user_time ON gateway.audit_logs(user_id,created_at DESC);
RESET statement_timeout;
RESET lock_timeout;
-- A failed concurrent index may remain INVALID. Authority inspects pg_index;
-- it must not use IF NOT EXISTS to convert an invalid prior attempt into success.
