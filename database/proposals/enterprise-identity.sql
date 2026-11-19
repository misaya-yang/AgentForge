-- UNNUMBERED, UNACTIVATED canonical proposal for U-GW-04.
-- Metadata augments gateway.users/user_roles and the existing Redis sessions;
-- no parallel account, role, permission, or authenticated-session authority.
ALTER TABLE gateway.users ADD COLUMN auth_epoch bigint NOT NULL DEFAULT 0 CHECK(auth_epoch>=0);
CREATE TABLE gateway.enterprise_tenant_policy (
 tenant_id varchar(255) PRIMARY KEY REFERENCES gateway.tenants(tenant_id),
 require_mfa boolean NOT NULL DEFAULT false
);
CREATE TABLE gateway.enterprise_identity_links (
 tenant_id varchar(255) NOT NULL, provider_id varchar(255) NOT NULL,
 issuer text NOT NULL, subject text NOT NULL CHECK(length(subject) BETWEEN 1 AND 2048),
 user_id varchar(255) NOT NULL, created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,provider_id,issuer,subject),
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE TABLE gateway.enterprise_scim_credentials (
 credential_id uuid PRIMARY KEY,tenant_id varchar(255) NOT NULL REFERENCES gateway.tenants(tenant_id),
 token_hash char(64) UNIQUE NOT NULL CHECK(token_hash ~ '^[0-9a-f]{64}$'),
 scopes text[] NOT NULL CHECK(cardinality(scopes)>0), created_by varchar(255) NOT NULL,
 expires_at timestamptz NOT NULL,revoked_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE gateway.enterprise_scim_users (
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 external_id varchar(255) NOT NULL,version bigint NOT NULL DEFAULT 1 CHECK(version>0),
 deleted boolean NOT NULL DEFAULT false,
 PRIMARY KEY(tenant_id,user_id),UNIQUE(tenant_id,external_id),
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE TABLE gateway.enterprise_scim_groups (
 tenant_id varchar(255) NOT NULL REFERENCES gateway.tenants(tenant_id),group_id uuid NOT NULL,
 external_id varchar(255) NOT NULL,display_name varchar(255) NOT NULL,
 version bigint NOT NULL DEFAULT 1 CHECK(version>0),PRIMARY KEY(tenant_id,group_id),UNIQUE(tenant_id,external_id)
);
CREATE TABLE gateway.enterprise_scim_members (
 tenant_id varchar(255) NOT NULL,group_id uuid NOT NULL,user_id varchar(255) NOT NULL,
 PRIMARY KEY(tenant_id,group_id,user_id),
 FOREIGN KEY(tenant_id,group_id) REFERENCES gateway.enterprise_scim_groups(tenant_id,group_id) ON DELETE CASCADE,
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.enterprise_scim_users(tenant_id,user_id) ON DELETE RESTRICT
);
-- Operator-reviewed mappings only. SCIM displayName never creates a role grant.
CREATE TABLE gateway.enterprise_scim_role_map (
 tenant_id varchar(255) NOT NULL REFERENCES gateway.tenants(tenant_id),external_group_id varchar(255) NOT NULL,
 role_name varchar(100) NOT NULL REFERENCES gateway.rbac_roles(role_name) CHECK(lower(role_name) NOT IN ('admin','owner','superadmin','super_admin','platform_admin')),PRIMARY KEY(tenant_id,external_group_id,role_name)
);
CREATE TABLE gateway.enterprise_security_state (
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,user_handle uuid UNIQUE NOT NULL,
 mfa_required boolean NOT NULL DEFAULT false,recovery_epoch bigint NOT NULL DEFAULT 0,
 PRIMARY KEY(tenant_id,user_id),FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE TABLE gateway.enterprise_passkeys (
 credential_id text PRIMARY KEY,tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 passkey jsonb NOT NULL,counter bigint NOT NULL CHECK(counter BETWEEN 0 AND 4294967295),
 revision bigint NOT NULL DEFAULT 1 CHECK(revision>0),created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
CREATE INDEX enterprise_passkeys_owner ON gateway.enterprise_passkeys(tenant_id,user_id);
CREATE TABLE gateway.enterprise_recovery_codes (
 tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,code_hash char(64) NOT NULL,
 consumed_at timestamptz,created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(tenant_id,user_id,code_hash),
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
-- Assurance is evidence attached to an existing allowlist JTI, never standalone
-- session authority. Current user/tenant/revision and Redis revocation still win.
CREATE TABLE gateway.enterprise_session_assurance (
 jti uuid PRIMARY KEY,tenant_id varchar(255) NOT NULL,user_id varchar(255) NOT NULL,
 method varchar(32) NOT NULL CHECK(method IN ('oidc','oidc_mfa','passkey','recovery')),
 verified_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,expires_at timestamptz NOT NULL,
 FOREIGN KEY(tenant_id,user_id) REFERENCES gateway.users(tenant_id,user_id) ON DELETE RESTRICT
);
REVOKE ALL ON gateway.enterprise_tenant_policy,gateway.enterprise_identity_links,gateway.enterprise_scim_credentials,
 gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_scim_members,
 gateway.enterprise_scim_role_map,gateway.enterprise_security_state,gateway.enterprise_passkeys,
 gateway.enterprise_recovery_codes,gateway.enterprise_session_assurance FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));principal text;
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical role prefix';END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT SELECT,INSERT ON gateway.enterprise_identity_links,gateway.enterprise_scim_credentials,gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_scim_members,gateway.enterprise_security_state,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes,gateway.enterprise_session_assurance TO %I',principal);
 EXECUTE format('GRANT UPDATE ON gateway.enterprise_scim_credentials,gateway.enterprise_scim_users,gateway.enterprise_scim_groups,gateway.enterprise_security_state,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes TO %I',principal);
 EXECUTE format('GRANT DELETE ON gateway.enterprise_scim_members,gateway.enterprise_scim_groups,gateway.enterprise_passkeys,gateway.enterprise_recovery_codes TO %I',principal);
 EXECUTE format('GRANT SELECT ON gateway.enterprise_scim_role_map,gateway.enterprise_tenant_policy TO %I',principal);
END $grants$;
-- Epoch never stands in for password_changed_at. Actual password-revision,
-- status and legacy grant mutations advance it; bcrypt representation-only
-- rehash leaves password_changed_at unchanged and does not revoke sessions.
CREATE FUNCTION gateway.enterprise_user_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF NEW.auth_epoch<OLD.auth_epoch THEN RAISE EXCEPTION 'authentication epoch cannot decrease';END IF;
 IF NEW.password_changed_at IS DISTINCT FROM OLD.password_changed_at
 OR NEW.status IS DISTINCT FROM OLD.status OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
 OR NEW.roles IS DISTINCT FROM OLD.roles OR NEW.permissions IS DISTINCT FROM OLD.permissions
 THEN NEW.auth_epoch:=GREATEST(NEW.auth_epoch,OLD.auth_epoch+1);END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER enterprise_user_epoch BEFORE UPDATE ON gateway.users FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_user_epoch();
CREATE FUNCTION gateway.enterprise_assignment_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id=OLD.user_id;END IF;
 IF TG_OP='INSERT' OR (TG_OP='UPDATE' AND NEW.user_id IS DISTINCT FROM OLD.user_id)
 THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id=NEW.user_id;END IF;
 IF TG_OP='DELETE' THEN RETURN OLD;ELSE RETURN NEW;END IF;
END $$;
CREATE TRIGGER enterprise_role_assignment_epoch AFTER INSERT OR UPDATE OR DELETE ON gateway.user_roles FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_assignment_epoch();
CREATE TRIGGER enterprise_direct_permission_epoch AFTER INSERT OR UPDATE OR DELETE ON gateway.user_permissions FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_assignment_epoch();
CREATE FUNCTION gateway.enterprise_role_policy_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id IN(SELECT user_id FROM gateway.user_roles WHERE role_name=OLD.role_name);END IF;
 IF TG_OP='INSERT' OR (TG_OP='UPDATE' AND NEW.role_name IS DISTINCT FROM OLD.role_name)
 THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE user_id IN(SELECT user_id FROM gateway.user_roles WHERE role_name=NEW.role_name);END IF;
 IF TG_OP='DELETE' THEN RETURN OLD;ELSE RETURN NEW;END IF;
END $$;
CREATE TRIGGER enterprise_role_permission_epoch AFTER INSERT OR UPDATE OR DELETE ON gateway.role_permissions FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_role_policy_epoch();
CREATE TRIGGER enterprise_role_definition_epoch AFTER UPDATE OF permissions ON gateway.rbac_roles FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_role_policy_epoch();
CREATE FUNCTION gateway.enterprise_tenant_mfa_epoch() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,gateway,pg_temp AS $$
BEGIN
 IF TG_OP='INSERT' OR NEW.require_mfa IS DISTINCT FROM OLD.require_mfa
 THEN UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE tenant_id=NEW.tenant_id;END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER enterprise_tenant_mfa_epoch AFTER INSERT OR UPDATE ON gateway.enterprise_tenant_policy FOR EACH ROW EXECUTE FUNCTION gateway.enterprise_tenant_mfa_epoch();
REVOKE ALL ON FUNCTION gateway.enterprise_user_epoch(),gateway.enterprise_assignment_epoch(),gateway.enterprise_role_policy_epoch(),gateway.enterprise_tenant_mfa_epoch() FROM PUBLIC;
