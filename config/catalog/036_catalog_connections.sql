-- Catalog connections successor 036.
-- Installed beside the closed service-successor definitions 30-35 and beside
-- knowledge spaces 030. The canonical epoch manifest stays at 30. Native
-- authority owns the transaction. No embedded BEGIN/COMMIT. No sequences.
-- Gateway receives SELECT, INSERT, UPDATE only. Revocation is a status update.

-- BEGIN OWNER GUARD
DO $successor_036_owner$
DECLARE prefix text := left(current_user, length(current_user) - length('owner'));
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
       OR (SELECT count(*) = 3 FROM pg_catalog.pg_namespace n
           WHERE n.nspname = ANY (ARRAY['assistant','gateway','knowledge'])
             AND pg_catalog.pg_get_userbyid(n.nspowner) = current_user) IS NOT TRUE
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_roles r
                  WHERE r.rolname = current_user
                    AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls))
    THEN RAISE EXCEPTION 'canonical unprivileged schema owner required'; END IF;
END
$successor_036_owner$;
-- END OWNER GUARD

-- BEGIN BUSINESS DDL
CREATE TABLE gateway.catalog_entries (
    entry_id varchar(128) PRIMARY KEY,
    tenant_id varchar(255) NOT NULL DEFAULT '',
    kind varchar(32) NOT NULL,
    name varchar(200) NOT NULL,
    description varchar(2000) NOT NULL DEFAULT '',
    category varchar(64) NOT NULL DEFAULT '',
    icon varchar(512) NOT NULL DEFAULT '',
    auth_kind varchar(32) NOT NULL DEFAULT 'none',
    manifest jsonb NOT NULL DEFAULT '{}'::jsonb,
    source varchar(32) NOT NULL DEFAULT 'tenant',
    source_sha256 varchar(64) NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_entries_kind_check CHECK (kind IN ('connector', 'mcp_server', 'skill', 'plugin', 'bot_template')),
    CONSTRAINT catalog_entries_auth_check CHECK (auth_kind IN ('none', 'oauth', 'api_key', 'pat')),
    CONSTRAINT catalog_entries_source_check CHECK (source IN ('builtin', 'tenant', 'registry')),
    CONSTRAINT catalog_entries_id_check CHECK (entry_id ~ '^[a-z0-9][a-z0-9._-]{0,127}$'),
    CONSTRAINT catalog_entries_name_check CHECK (length(btrim(name)) BETWEEN 1 AND 200)
);

CREATE TABLE gateway.catalog_connections (
    connection_id varchar(64) PRIMARY KEY,
    tenant_id varchar(255) NOT NULL,
    entry_id varchar(128) NOT NULL,
    owner_kind varchar(16) NOT NULL,
    owner_id varchar(255) NOT NULL,
    status varchar(32) NOT NULL,
    credential_ciphertext text,
    issuer varchar(512) NOT NULL DEFAULT '',
    resource varchar(512) NOT NULL DEFAULT '',
    error_code varchar(64) NOT NULL DEFAULT '',
    created_by varchar(255) NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_connections_owner_check CHECK (owner_kind IN ('user', 'tenant', 'space')),
    CONSTRAINT catalog_connections_status_check CHECK (status IN ('available', 'connecting', 'connected', 'needs_reauth', 'error', 'disabled_by_policy', 'revoked')),
    CONSTRAINT catalog_connections_ciphertext_check CHECK (credential_ciphertext IS NULL OR credential_ciphertext ~ '^enc:'),
    CONSTRAINT catalog_connections_owner_id_check CHECK (length(btrim(owner_id)) > 0)
);

CREATE UNIQUE INDEX catalog_connections_active
    ON gateway.catalog_connections (tenant_id, entry_id, owner_kind, owner_id)
    WHERE status <> 'revoked';

CREATE TABLE gateway.catalog_oauth_states (
    state varchar(128) PRIMARY KEY,
    tenant_id varchar(255) NOT NULL,
    user_id varchar(255) NOT NULL,
    entry_id varchar(128) NOT NULL,
    connection_id varchar(64) NOT NULL,
    code_verifier_ciphertext text NOT NULL,
    issuer varchar(512) NOT NULL,
    resource varchar(512) NOT NULL DEFAULT '',
    redirect_uri varchar(1024) NOT NULL,
    client_id varchar(1024) NOT NULL DEFAULT '',
    token_url varchar(1024) NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    CONSTRAINT catalog_oauth_states_verifier_check CHECK (code_verifier_ciphertext ~ '^enc:'),
    CONSTRAINT catalog_oauth_states_state_check CHECK (length(state) BETWEEN 16 AND 128)
);

REVOKE ALL ON TABLE gateway.catalog_entries FROM PUBLIC;
REVOKE ALL ON TABLE gateway.catalog_connections FROM PUBLIC;
REVOKE ALL ON TABLE gateway.catalog_oauth_states FROM PUBLIC;
-- END BUSINESS DDL

-- BEGIN GRANTS
DO $successor_036_grants$
DECLARE prefix text := left(current_user, length(current_user) - length('owner'));
        gateway text;
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
    THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;
    gateway := prefix || 'gateway';
    EXECUTE format('GRANT SELECT, INSERT, UPDATE ON TABLE gateway.catalog_entries, gateway.catalog_connections, gateway.catalog_oauth_states TO %I', gateway);
END
$successor_036_grants$;
-- END GRANTS
