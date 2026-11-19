-- Install through a NEW native-authority successor; no application startup DDL.
CREATE TABLE gateway.authorization_decisions (
    decision_id uuid PRIMARY KEY,
    capability text NOT NULL CHECK (length(capability) BETWEEN 1 AND 128),
    policy_sha256 text NOT NULL CHECK (policy_sha256 ~ '^[0-9a-f]{64}$'),
    matrix_sha256 text NOT NULL CHECK (matrix_sha256 ~ '^[0-9a-f]{64}$'),
    principal_fingerprint text NOT NULL CHECK (principal_fingerprint ~ '^[0-9a-f]{64}$'),
    mode text NOT NULL CHECK (mode IN ('shadow','enforce')),
    legacy_allowed boolean NOT NULL,
    cedar_allowed boolean NOT NULL,
    enforced_allowed boolean NOT NULL,
    evaluation_error boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
REVOKE ALL ON gateway.authorization_decisions FROM PUBLIC;
-- Append-only process role. Audit readers use a separately governed owner/admin
-- path; possession of Gateway credentials grants no audit SELECT/UPDATE/DELETE.
GRANT INSERT ON gateway.authorization_decisions TO ai_gateway_gateway;
