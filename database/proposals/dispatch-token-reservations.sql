-- UNNUMBERED / NOT ACTIVATED: U-GW-07 dispatch admission lifecycle only.
-- Requires the canonical price-book-v2 accounting checkpoint proposal activated
-- first (including quota_counters transaction receipts). No pricing arithmetic,
-- copied usage ledger, or fingerprint edits. Canonical numbering/activation and
-- exact scoped-role verification remain the native authority writer's task.
CREATE TABLE gateway.dispatch_token_reservations (
 tenant_id text NOT NULL CHECK(tenant_id<>''),
 user_id text NOT NULL CHECK(user_id<>''),
 request_id text NOT NULL CHECK(request_id<>''),
 service_id text NOT NULL,
 request_type text NOT NULL CHECK(request_type IN ('invoke','stream')),
 member uuid NOT NULL,
 logical_id uuid NOT NULL,
 estimated_tokens bigint NOT NULL CHECK(estimated_tokens>=0),
 state text NOT NULL CHECK(state IN ('prepared','dispatched','awaiting_checkpoint','settled','cancelled','expired','outcome_unknown')),
 prepare_deadline timestamptz NOT NULL,
 candidate_receipt_id text,
 settlement_candidate jsonb,
 settlement_observation jsonb,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,user_id,member),
 CHECK((state='settled')=(settlement_observation IS NOT NULL)),
 CHECK(settlement_observation IS NULL OR (
   settlement_observation->>'tenant_id'=tenant_id AND
   settlement_observation->>'user_id'=user_id AND
   settlement_observation->>'request_id'=request_id AND
   settlement_observation->>'service_id'=service_id AND
   settlement_observation->>'request_type'=request_type AND
   settlement_observation->>'status' IN ('success','error')) IS TRUE)
);
CREATE INDEX dispatch_token_reservations_live ON gateway.dispatch_token_reservations(tenant_id,user_id,created_at)
 WHERE state IN ('prepared','dispatched','awaiting_checkpoint','outcome_unknown');
REVOKE ALL ON gateway.dispatch_token_reservations FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF current_user NOT LIKE '%\_owner' ESCAPE '\' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
 THEN RAISE EXCEPTION 'invalid canonical role prefix'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.dispatch_token_reservations TO %I',principal);
 EXECUTE format('GRANT UPDATE(state,candidate_receipt_id,settlement_candidate,settlement_observation,updated_at) ON gateway.dispatch_token_reservations TO %I',principal);
END
$grants$;
-- GatewayDurable, Knowledge, Runtime/Worker receive no new privileges.
