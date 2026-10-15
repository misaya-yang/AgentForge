-- Gateway-owned tenant admission policy only. No package files or runtime state.
-- Additive schema, but old Gateway binaries do not enforce these restrictions:
-- drain old readers before writes; rollback must retain a compatible Gateway.
CREATE TABLE gateway.tenant_plugin_bindings (
    tenant_id varchar(255) NOT NULL CHECK (length(btrim(tenant_id)) > 0),
    plugin_id varchar(128) NOT NULL CHECK (plugin_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    selected_version varchar(128) NOT NULL CHECK (selected_version ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    enabled boolean NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    selected_catalog_revision varchar(20) NOT NULL CHECK (selected_catalog_revision ~ '^[1-9][0-9]{0,19}$'),
    selection_hash varchar(71) NOT NULL CHECK (selection_hash ~ '^sha256:[0-9a-f]{64}$'),
    capability_names text[] NOT NULL CHECK (cardinality(capability_names) BETWEEN 1 AND 1024),
    updated_by varchar(255) NOT NULL CHECK (length(btrim(updated_by)) > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, plugin_id)
);
COMMENT ON TABLE gateway.tenant_plugin_bindings IS
    'Gateway admission preferences and catalog fingerprints. Not executable installation or permission grants.';
REVOKE ALL ON gateway.tenant_plugin_bindings FROM PUBLIC;
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Plugin lifecycle migration requires the authority owner role';
    END IF;
    EXECUTE format('REVOKE ALL ON gateway.tenant_plugin_bindings FROM %I', prefix || 'gateway');
    EXECUTE format('GRANT SELECT, INSERT, UPDATE ON gateway.tenant_plugin_bindings TO %I', prefix || 'gateway');
END
$permissions$;
