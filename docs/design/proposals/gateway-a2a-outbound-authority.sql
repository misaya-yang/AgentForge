-- New ordered native-authority successor after gateway-a2a-task-effects.sql.
-- No frozen epoch changes, startup DDL, resubmission, or budget refunds.
ALTER TABLE gateway.gateway_a2a_outbound_effects
 ADD COLUMN remote_message_id uuid,
 ADD COLUMN remote_task_id text CHECK(remote_task_id IS NULL OR length(remote_task_id)<=255),
 ADD COLUMN card_sha256 text CHECK(card_sha256 IS NULL OR card_sha256~'^[0-9a-f]{64}$'),
 ADD COLUMN card_key_id text CHECK(card_key_id IS NULL OR length(card_key_id)<=128),
 ADD COLUMN cancel_state text NOT NULL DEFAULT 'none' CHECK(cancel_state IN ('none','unknown','confirmed','rejected'));
-- Existing app SELECT/INSERT/UPDATE grants remain sufficient. No DELETE or
-- privilege escalation. Prior rows without authority fields fail closed for
-- new get/cancel/reconcile; never infer authority or re-send to fill missing data.
