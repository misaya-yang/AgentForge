-- New successor only; do not alter frozen 029 or its fingerprint.
CREATE TABLE gateway.authorization_resource_decisions (
 decision_id uuid PRIMARY KEY,capability varchar(128) NOT NULL,
 policy_sha256 text NOT NULL CHECK(policy_sha256~'^[0-9a-f]{64}$'),
 matrix_sha256 text NOT NULL CHECK(matrix_sha256~'^[0-9a-f]{64}$'),
 principal_fingerprint text NOT NULL CHECK(principal_fingerprint~'^[0-9a-f]{64}$'),
 mode text NOT NULL CHECK(mode IN ('shadow','enforce')),
 legacy_allowed boolean NOT NULL,cedar_allowed boolean NOT NULL,
 enforced_allowed boolean NOT NULL,evaluation_error boolean NOT NULL,
 resource_context jsonb NOT NULL CHECK(jsonb_typeof(resource_context)='object'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX authorization_resource_decisions_created ON gateway.authorization_resource_decisions(created_at);
CREATE INDEX authorization_resource_decisions_differences ON gateway.authorization_resource_decisions(created_at) WHERE legacy_allowed<>cedar_allowed;
REVOKE ALL ON gateway.authorization_resource_decisions FROM PUBLIC;
-- Native authority renders its gateway app role: GRANT INSERT only. Operator
-- audit read access stays separately authorized; no UPDATE/DELETE app privilege.
