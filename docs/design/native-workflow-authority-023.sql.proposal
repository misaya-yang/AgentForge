-- Isolated draft only. Do not apply directly or copy into canonical migrations.
-- Candidate canonical successor 023: persist hash-only Local Node v2 bearer
-- credentials and durable cancel intent, grant confirmed Knowledge migration
-- rights to the API and Worker roles, and harden the two existing Assistant
-- runtime functions that own their protected writes.
--
-- Source evidence: the Local Node v2 channel stores only an opaque bearer
-- SHA-256, consumes a one-use pairing challenge, and checks tenant/user/device/
-- channel scope, expiry and revocation on heartbeat. The channel fingerprint
-- remains the verified device public-key fingerprint. The runtime records
-- durable cancel_requested_at before sending controls.
--
-- Knowledge API needs the following table rights for the existing describe,
-- lazy serving initialization, start, enqueue, cancel, cutover, rollback and
-- abort paths. The Knowledge Worker retains its existing rights on the first
-- three relations, receives SELECT for its pending-progress read, and receives
-- only SELECT/UPDATE on the two batch-poller tables plus column-scoped UPDATE
-- on document_versions.change_type and metadata. No broader Knowledge,
-- Runtime, or capability-worker privileges are added. The Assistant changes
-- alter only SECURITY DEFINER/search_path metadata on two exact owner-controlled
-- functions; bodies, owners, EXECUTE ACLs, and Gateway table privileges stay
-- unchanged.
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
        RAISE EXCEPTION 'successor 023 grants require the authority owner role';
    END IF;
    EXECUTE format(
        'GRANT SELECT, INSERT, UPDATE ON TABLE gateway.local_node_device_credentials TO %I',
        prefix || 'gateway'
    );
    EXECUTE format(
        'GRANT SELECT, INSERT, UPDATE ON TABLE knowledge.dataset_collection_bindings TO %I',
        prefix || 'knowledge_api'
    );
    EXECUTE format(
        'GRANT SELECT, INSERT, UPDATE ON TABLE knowledge.embedding_migrations TO %I',
        prefix || 'knowledge_api'
    );
    EXECUTE format(
        'GRANT SELECT, INSERT, UPDATE ON TABLE knowledge.embedding_migration_action_jobs TO %I',
        prefix || 'knowledge_api'
    );
    EXECUTE format(
        'GRANT SELECT, DELETE ON TABLE knowledge.embedding_migration_progress TO %I',
        prefix || 'knowledge_api'
    );
    EXECUTE format(
        'GRANT SELECT ON TABLE knowledge.embedding_migration_progress TO %I',
        prefix || 'knowledge_worker'
    );
    EXECUTE format(
        'GRANT SELECT, UPDATE ON TABLE knowledge.kb_document_batch_operations TO %I',
        prefix || 'knowledge_worker'
    );
    EXECUTE format(
        'GRANT SELECT, UPDATE ON TABLE knowledge.kb_document_batch_items TO %I',
        prefix || 'knowledge_worker'
    );
    EXECUTE format(
        'GRANT UPDATE (change_type, metadata) ON TABLE knowledge.document_versions TO %I',
        prefix || 'knowledge_worker'
    );
END
$permissions$;

-- Retain the frozen 090 function bodies, owners, and EXECUTE ACLs. These two
-- existing invoker functions insert records through the owner-controlled
-- runtime boundary; run only these exact signatures as SECURITY DEFINER with
-- a closed search path and pg_temp last.
ALTER FUNCTION assistant.issue_assistant_runtime_turn(
    UUID, UUID, UUID, UUID, VARCHAR, VARCHAR, VARCHAR, VARCHAR, VARCHAR, JSONB,
    CHAR, BIGINT, VARCHAR, VARCHAR, VARCHAR, VARCHAR, VARCHAR, CHAR, INTEGER,
    BIGINT, BIGINT, BIGINT, TIMESTAMPTZ, TEXT
) SECURITY DEFINER;
ALTER FUNCTION assistant.issue_assistant_runtime_turn(
    UUID, UUID, UUID, UUID, VARCHAR, VARCHAR, VARCHAR, VARCHAR, VARCHAR, JSONB,
    CHAR, BIGINT, VARCHAR, VARCHAR, VARCHAR, VARCHAR, VARCHAR, CHAR, INTEGER,
    BIGINT, BIGINT, BIGINT, TIMESTAMPTZ, TEXT
) SET search_path = pg_catalog, assistant, pg_temp;

ALTER FUNCTION assistant.reserve_assistant_runtime_model_call(
    UUID, UUID, CHAR, BIGINT, BIGINT, BIGINT
) SECURITY DEFINER;
ALTER FUNCTION assistant.reserve_assistant_runtime_model_call(
    UUID, UUID, CHAR, BIGINT, BIGINT, BIGINT
) SET search_path = pg_catalog, assistant, pg_temp;
