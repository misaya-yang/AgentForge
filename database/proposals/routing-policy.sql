-- Inactive authoring proposal. Native canonical authority assigns the successor,
-- measures raw fingerprints/ACL deltas and activates; do not execute out of band.
-- Versioned policy routing adds no provider/catalog/budget/accounting authority.
CREATE TABLE gateway.routing_policy_versions (
 tenant_id varchar(64) NOT NULL REFERENCES gateway.tenants(tenant_id),
 policy_id varchar(128) NOT NULL,
 version varchar(128) NOT NULL,
 pin char(64) NOT NULL CHECK(pin ~ '^[0-9a-f]{64}$'),
 payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 created_by varchar(64) NOT NULL,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,policy_id,version),
 UNIQUE(tenant_id,policy_id,pin)
);
CREATE TABLE gateway.routing_policy_active (
 tenant_id varchar(64) NOT NULL,
 policy_id varchar(128) NOT NULL,
 pin char(64) NOT NULL,
 PRIMARY KEY(tenant_id,policy_id),
 FOREIGN KEY(tenant_id,policy_id,pin) REFERENCES gateway.routing_policy_versions(tenant_id,policy_id,pin)
);
CREATE TABLE gateway.routing_decisions (
 tenant_id varchar(64) NOT NULL,
 actor_id varchar(64) NOT NULL,
 invocation_id varchar(512) NOT NULL,
 decision_id char(64) NOT NULL CHECK(decision_id ~ '^[0-9a-f]{64}$'),
 policy_id varchar(128) NOT NULL,
 policy_pin char(64) NOT NULL,
 payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,decision_id),
 UNIQUE(tenant_id,actor_id,invocation_id),
 FOREIGN KEY(tenant_id,policy_id,policy_pin) REFERENCES gateway.routing_policy_versions(tenant_id,policy_id,pin)
);
-- Append-only versions/decisions: Gateway gets neither UPDATE nor DELETE.
REVOKE ALL ON gateway.routing_policy_versions,gateway.routing_policy_active,gateway.routing_decisions FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.routing_policy_versions,gateway.routing_decisions TO %I',principal);
 EXECUTE format('GRANT SELECT,INSERT,UPDATE ON gateway.routing_policy_active TO %I',principal);
END $grants$;
-- No Runtime/Worker/Knowledge grants and no SECURITY DEFINER authority.
