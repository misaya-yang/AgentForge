-- Canonical successor 033. Runs only inside the native authority transaction.
-- Minimal runtime ACL correction for statements the services already execute:
--  * Gateway hourly usage accounting does UPDATE ... SET n=n+$v / ON CONFLICT,
--    which PostgreSQL authorizes only with SELECT on the referenced columns.
--  * The Knowledge worker claims, completes and releases document batch items
--    (SELECT ... FOR UPDATE SKIP LOCKED, UPDATE) but had no privilege at all.
-- No INSERT/DELETE for the worker, no PUBLIC access, no grant option, no membership.
DO $owner$ BEGIN
  IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR EXISTS(SELECT 1 FROM pg_roles WHERE rolname=current_user AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)) THEN RAISE EXCEPTION 'canonical owner required'; END IF;
END $owner$;
-- BEGIN REQUIRED SERVICE ACL CORRECTION
DO $service_acl$
DECLARE
  prefix text := left(current_user,length(current_user)-length('owner'));
BEGIN
  EXECUTE format('GRANT SELECT ON TABLE gateway.usage_hourly_aggregates TO %I', prefix||'gateway');
  EXECUTE format('GRANT SELECT,UPDATE ON TABLE knowledge.kb_document_batch_operations,knowledge.kb_document_batch_items TO %I', prefix||'knowledge_worker');
END $service_acl$;
-- END REQUIRED SERVICE ACL CORRECTION
