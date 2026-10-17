-- UNACTIVATED PROPOSAL: no successor number assigned. Never apply directly.
-- Promote only through the canonical native migration authority after complete
-- predecessor and fingerprint verification.
-- This metadata-only change retains the existing function body, signature,
-- owner and EXECUTE ACL. It grants no table privileges or other role rights.
ALTER FUNCTION assistant.issue_assistant_runtime_turn(
    uuid, uuid, uuid, uuid,
    character varying, character varying, character varying, character varying,
    character varying, jsonb, character, bigint,
    character varying, character varying, character varying, character varying,
    character varying, character, integer, bigint, bigint, bigint,
    timestamp with time zone, text
)
SECURITY DEFINER;

ALTER FUNCTION assistant.issue_assistant_runtime_turn(
    uuid, uuid, uuid, uuid,
    character varying, character varying, character varying, character varying,
    character varying, jsonb, character, bigint,
    character varying, character varying, character varying, character varying,
    character varying, character, integer, bigint, bigint, bigint,
    timestamp with time zone, text
)
SET search_path = pg_catalog, assistant, gateway, pg_temp;
