-- Knowledge spaces successor candidate 030.
-- Installed migration. The frozen epoch-29 lock, baselines, and fingerprints stay
-- unchanged; their bytes are archived beside this chain. Sequences 030-035 in the
-- service-successor catalog remain that closed preview and are not this file.
-- Native authority owns the transaction. No embedded BEGIN/COMMIT.

-- BEGIN OWNER GUARD
DO $successor_030_owner$
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
$successor_030_owner$;
-- END OWNER GUARD

-- BEGIN BUSINESS DDL
CREATE TABLE knowledge.spaces (
    space_id varchar(255) PRIMARY KEY,
    tenant_id varchar(255) NOT NULL,
    name varchar(200) NOT NULL,
    icon varchar(32) NOT NULL DEFAULT '',
    kind varchar(16) NOT NULL,
    visibility varchar(16) NOT NULL,
    dataset_id varchar(255) NOT NULL REFERENCES knowledge.datasets (dataset_id),
    created_by varchar(255) NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    archived_at timestamptz,
    CONSTRAINT spaces_name_check CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    CONSTRAINT spaces_kind_check CHECK (kind IN ('personal', 'team', 'tenant')),
    CONSTRAINT spaces_visibility_check CHECK (visibility IN ('private', 'members', 'tenant')),
    CONSTRAINT spaces_icon_check CHECK (length(icon) <= 32)
);

CREATE TABLE knowledge.space_items (
    item_id varchar(255) PRIMARY KEY,
    tenant_id varchar(255) NOT NULL,
    space_id varchar(255) NOT NULL REFERENCES knowledge.spaces (space_id),
    parent_id varchar(255) REFERENCES knowledge.space_items (item_id),
    kind varchar(16) NOT NULL,
    title varchar(500) NOT NULL,
    icon varchar(32) NOT NULL DEFAULT '',
    position varchar(64) NOT NULL,
    source varchar(96) NOT NULL,
    external_ref jsonb NOT NULL DEFAULT '{}'::jsonb,
    document_id varchar(255) REFERENCES knowledge.documents (document_id),
    content_format varchar(32) NOT NULL,
    current_revision bigint NOT NULL DEFAULT 0,
    created_by varchar(255) NOT NULL,
    updated_by varchar(255) NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    last_activity_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    title_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', title)) STORED,
    CONSTRAINT space_items_kind_check CHECK (kind IN ('page', 'folder', 'file', 'link', 'slide', 'site', 'image', 'external')),
    CONSTRAINT space_items_title_check CHECK (length(btrim(title)) BETWEEN 1 AND 500),
    CONSTRAINT space_items_icon_check CHECK (length(icon) <= 32),
    CONSTRAINT space_items_position_check CHECK (position ~ '^[0-9A-Za-z]{1,64}$'),
    CONSTRAINT space_items_source_check CHECK (source ~ '^(native|upload|connector:[A-Za-z0-9_.:-]{1,80})$'),
    CONSTRAINT space_items_format_check CHECK (content_format IN ('blocknote_json', 'markdown', 'binary')),
    CONSTRAINT space_items_revision_check CHECK (current_revision >= 0),
    CONSTRAINT space_items_tenant_space_check CHECK (length(btrim(tenant_id)) > 0)
);

CREATE TABLE knowledge.space_item_revisions (
    item_id varchar(255) NOT NULL REFERENCES knowledge.space_items (item_id),
    revision bigint NOT NULL,
    blocks jsonb,
    markdown text,
    content_sha256 char(64) NOT NULL,
    author varchar(255) NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    ydoc_state bytea,
    PRIMARY KEY (item_id, revision),
    CONSTRAINT space_item_revisions_revision_check CHECK (revision > 0),
    CONSTRAINT space_item_revisions_sha_check CHECK (content_sha256 ~ '^[0-9a-f]{64}$')
);

CREATE TABLE knowledge.space_members (
    space_id varchar(255) NOT NULL REFERENCES knowledge.spaces (space_id),
    principal_type varchar(16) NOT NULL,
    principal_id varchar(255) NOT NULL,
    role varchar(16) NOT NULL,
    revoked_at timestamptz,
    PRIMARY KEY (space_id, principal_type, principal_id),
    CONSTRAINT space_members_principal_check CHECK (principal_type IN ('user', 'group', 'role')),
    CONSTRAINT space_members_role_check CHECK (role IN ('owner', 'editor', 'commenter', 'viewer')),
    CONSTRAINT space_members_principal_id_check CHECK (length(btrim(principal_id)) > 0)
);

CREATE TABLE knowledge.space_item_shares (
    item_id varchar(255) NOT NULL REFERENCES knowledge.space_items (item_id),
    principal_type varchar(16) NOT NULL,
    principal_id varchar(255) NOT NULL,
    role varchar(16) NOT NULL,
    revoked_at timestamptz,
    PRIMARY KEY (item_id, principal_type, principal_id),
    CONSTRAINT space_item_shares_principal_check CHECK (principal_type IN ('user', 'group', 'role')),
    CONSTRAINT space_item_shares_role_check CHECK (role IN ('owner', 'editor', 'commenter', 'viewer')),
    CONSTRAINT space_item_shares_principal_id_check CHECK (length(btrim(principal_id)) > 0)
);

CREATE TABLE knowledge.space_item_stars (
    user_id varchar(255) NOT NULL,
    item_id varchar(255) NOT NULL REFERENCES knowledge.space_items (item_id),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    PRIMARY KEY (user_id, item_id)
);

CREATE TABLE knowledge.space_item_activity (
    activity_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    item_id varchar(255) NOT NULL REFERENCES knowledge.space_items (item_id),
    user_id varchar(255) NOT NULL,
    action varchar(16) NOT NULL,
    at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT space_item_activity_action_check CHECK (action IN ('create', 'open', 'edit', 'rename', 'move', 'share', 'star', 'unstar', 'delete', 'restore'))
);

CREATE INDEX spaces_tenant_active ON knowledge.spaces (tenant_id, updated_at DESC) WHERE archived_at IS NULL;
CREATE INDEX space_items_activity ON knowledge.space_items (tenant_id, last_activity_at DESC, item_id DESC);
CREATE INDEX space_items_space_tree ON knowledge.space_items (space_id, parent_id, position);
CREATE INDEX space_items_title_tsv ON knowledge.space_items USING gin (title_tsv);
CREATE INDEX space_members_principal ON knowledge.space_members (principal_type, principal_id);
CREATE INDEX space_item_shares_principal ON knowledge.space_item_shares (principal_type, principal_id);
CREATE INDEX space_item_stars_user ON knowledge.space_item_stars (user_id);
CREATE INDEX space_item_activity_user ON knowledge.space_item_activity (user_id, at DESC);

REVOKE ALL ON TABLE knowledge.spaces FROM PUBLIC;
REVOKE ALL ON TABLE knowledge.space_items FROM PUBLIC;
REVOKE ALL ON TABLE knowledge.space_item_revisions FROM PUBLIC;
REVOKE ALL ON TABLE knowledge.space_members FROM PUBLIC;
REVOKE ALL ON TABLE knowledge.space_item_shares FROM PUBLIC;
REVOKE ALL ON TABLE knowledge.space_item_stars FROM PUBLIC;
REVOKE ALL ON TABLE knowledge.space_item_activity FROM PUBLIC;
REVOKE ALL ON SEQUENCE knowledge.space_item_activity_activity_id_seq FROM PUBLIC;
-- END BUSINESS DDL

-- BEGIN GRANTS
DO $successor_030_grants$
DECLARE prefix text := left(current_user, length(current_user) - length('owner'));
        api text;
        worker text;
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
    THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;
    api := prefix || 'knowledge_api';
    worker := prefix || 'knowledge_worker';
    EXECUTE format('GRANT SELECT, INSERT, UPDATE ON TABLE knowledge.spaces, knowledge.space_items, knowledge.space_item_revisions, knowledge.space_members, knowledge.space_item_shares, knowledge.space_item_stars, knowledge.space_item_activity TO %I', api);
    EXECUTE format('GRANT USAGE, SELECT ON SEQUENCE knowledge.space_item_activity_activity_id_seq TO %I', api);
    EXECUTE format('GRANT SELECT (space_id, tenant_id, dataset_id, archived_at) ON TABLE knowledge.spaces TO %I', worker);
    EXECUTE format('GRANT SELECT (item_id, tenant_id, space_id, document_id, title, deleted_at, current_revision, content_format) ON TABLE knowledge.space_items TO %I', worker);
    EXECUTE format('GRANT SELECT (item_id, revision, markdown, content_sha256) ON TABLE knowledge.space_item_revisions TO %I', worker);
END
$successor_030_grants$;
-- END GRANTS
