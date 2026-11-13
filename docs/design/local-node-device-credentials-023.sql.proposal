-- Isolated draft only. Do not apply directly or copy into canonical migrations.
-- Candidate canonical successor 023: persist hash-only Local Node v2 bearer
-- credentials and durable per-execution cancel intent.
--
-- Source evidence: the Local Node v2 channel issues an opaque lnv2 bearer,
-- stores only its SHA-256 digest, consumes a one-use pairing challenge, and
-- checks tenant/user/device/channel scope, expiry and revocation on heartbeat.
-- The channel fingerprint remains the verified device public-key fingerprint.
-- The runtime also records durable cancel_requested_at before sending controls.
--
-- The Gateway role receives only SELECT, INSERT and UPDATE on the new table.
-- It already has the required UPDATE edge on local_node_executions. No grant is
-- added to Knowledge, Runtime or capability-worker roles.
CREATE TABLE gateway.local_node_device_credentials (
    credential_id uuid NOT NULL,
    credential_sha256 character(64) NOT NULL,
    tenant_id character varying(128) NOT NULL,
    user_id character varying(128) NOT NULL,
    device_id character varying(255) NOT NULL,
    channel_id character varying(255) NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    accepted_through_sequence bigint DEFAULT 0 NOT NULL,
    last_seen_at timestamp with time zone,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT local_node_device_credentials_pkey PRIMARY KEY (credential_id),
    CONSTRAINT local_node_device_credentials_credential_sha256_key UNIQUE (credential_sha256),
    CONSTRAINT local_node_device_credentials_credential_sha256_check
        CHECK (credential_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT local_node_device_credentials_sequence_check
        CHECK (accepted_through_sequence >= 0),
    CONSTRAINT local_node_device_credentials_device_scope_fkey
        FOREIGN KEY (device_id, tenant_id, user_id)
        REFERENCES gateway.local_node_devices(device_id, tenant_id, user_id)
        ON DELETE RESTRICT,
    CONSTRAINT local_node_device_credentials_channel_id_fkey
        FOREIGN KEY (channel_id)
        REFERENCES gateway.local_node_channels(channel_id)
        ON DELETE RESTRICT
);

CREATE UNIQUE INDEX local_node_device_credentials_active_channel
    ON gateway.local_node_device_credentials(channel_id)
    WHERE revoked_at IS NULL;

CREATE INDEX local_node_device_credentials_owner_device
    ON gateway.local_node_device_credentials(tenant_id, user_id, device_id, expires_at)
    WHERE revoked_at IS NULL;

ALTER TABLE gateway.local_node_executions
    ADD COLUMN cancel_requested_at timestamp with time zone;

DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Local Node credential grants require the authority owner role';
    END IF;
    EXECUTE format(
        'GRANT SELECT, INSERT, UPDATE ON TABLE gateway.local_node_device_credentials TO %I',
        prefix || 'gateway'
    );
END
$permissions$;
