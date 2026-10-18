-- Inactive033. Cedar resource audit is already exactly pinned/installed by030; never recreate it.
DO $service_owner$ BEGIN IF (current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'gateway') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'runtime') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_api') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_worker')) IS NOT TRUE THEN RAISE EXCEPTION 'canonical scoped schema owner required'; END IF; END $service_owner$;
-- New ordered native-authority successor after gateway-a2a-task-effects.sql.
-- No frozen epoch changes, startup DDL, resubmission, or budget refunds.
ALTER TABLE gateway.gateway_a2a_outbound_effects
 ADD COLUMN remote_message_id uuid,
 ADD COLUMN remote_task_id text,
 ADD COLUMN card_sha256 text,
 ADD COLUMN card_key_id text,
 ADD COLUMN cancel_state text DEFAULT 'none';
ALTER TABLE gateway.gateway_a2a_outbound_effects ADD CONSTRAINT a2a_outbound_remote_task_bound CHECK(remote_task_id IS NULL OR length(remote_task_id)<=255) NOT VALID;
ALTER TABLE gateway.gateway_a2a_outbound_effects ADD CONSTRAINT a2a_outbound_card_sha256_format CHECK(card_sha256 IS NULL OR card_sha256~'^[0-9a-f]{64}$') NOT VALID;
ALTER TABLE gateway.gateway_a2a_outbound_effects ADD CONSTRAINT a2a_outbound_card_key_bound CHECK(card_key_id IS NULL OR length(card_key_id)<=128) NOT VALID;
ALTER TABLE gateway.gateway_a2a_outbound_effects ADD CONSTRAINT a2a_outbound_cancel_state_domain CHECK(cancel_state IN ('none','unknown','confirmed','rejected')) NOT VALID;
ALTER TABLE gateway.gateway_a2a_outbound_effects ADD CONSTRAINT a2a_outbound_cancel_state_required CHECK(cancel_state IS NOT NULL) NOT VALID;
-- Native source-owned033 explicitly adds only UPDATE(remote_task_id,cancel_state). No DELETE or
-- table-wide UPDATE/grant option is granted. Prior rows without authority fields fail closed for
-- new get/cancel/reconcile; never infer authority or re-send to fill missing data.

DO $service_grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical prefix'; END IF;
 EXECUTE format('GRANT UPDATE(remote_task_id,cancel_state) ON gateway.gateway_a2a_outbound_effects TO %I',prefix||'gateway');
END $service_grants$;
