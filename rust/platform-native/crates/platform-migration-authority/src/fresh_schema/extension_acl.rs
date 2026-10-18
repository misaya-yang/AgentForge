//! Fresh-only exact trusted-extension ACL convergence by the local peer owner.
//! This runs in the one schema transaction and never grants a privilege.
pub(super) const HARDEN: &str = r#"
DO $fresh_extension_acl$
DECLARE member record; total integer := 0;
BEGIN
 IF inet_client_addr() IS NOT NULL OR inet_server_addr() IS NOT NULL
    OR current_user <> session_user
    OR NOT (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)
 THEN RAISE EXCEPTION 'fresh extension ACL requires independent local operator'; END IF;
 IF (SELECT array_agg(extname::text ORDER BY extname) FROM pg_catalog.pg_extension WHERE extname <> 'plpgsql')
    IS DISTINCT FROM ARRAY['pg_trgm','pgcrypto','uuid-ossp']::text[]
 THEN RAISE EXCEPTION 'fresh extension allowlist differs'; END IF;
 FOR member IN
   SELECT p.oid,p.prokind,n.nspname,pg_catalog.pg_get_userbyid(p.proowner) AS owner,
          e.extname,pg_catalog.pg_get_userbyid(e.extowner) AS extension_owner,
          pg_catalog.pg_get_function_identity_arguments(p.oid) AS arguments,p.proname
   FROM pg_catalog.pg_proc p
   JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
   JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e'
   JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid
   WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')
   ORDER BY e.extname,p.proname,p.oid
 LOOP
   IF member.nspname <> 'public' OR member.prokind <> 'f' OR member.owner <> current_user
      OR member.extension_owner <> current_setting('agentforge.fresh_owner')
   THEN RAISE EXCEPTION 'fresh trusted extension member provenance differs'; END IF;
   EXECUTE format('REVOKE EXECUTE ON FUNCTION %I.%I(%s) FROM PUBLIC',member.nspname,member.proname,member.arguments);
   total := total + 1;
 END LOOP;
 IF total=0 THEN RAISE EXCEPTION 'fresh extension member inventory empty'; END IF;
 IF EXISTS(
   SELECT 1 FROM pg_catalog.pg_proc p
   JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e'
   JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid
   CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a
   WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp') AND a.grantee=0
 ) THEN RAISE EXCEPTION 'fresh extension PUBLIC ACL remains'; END IF;
END $fresh_extension_acl$;
"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extension_acl_is_exact_local_provenance_bound_and_revoke_only() {
        assert!(HARDEN.contains("ARRAY['pg_trgm','pgcrypto','uuid-ossp']"));
        for requirement in [
            "inet_client_addr() IS NOT NULL",
            "current_user <> session_user",
            "member.owner <> current_user",
            "d.deptype='e'",
            "member.prokind <> 'f'",
            "member.nspname <> 'public'",
            "member.extension_owner",
            "a.grantee=0",
        ] {
            assert!(HARDEN.contains(requirement));
        }
        assert!(!HARDEN.contains("GRANT "));
        assert!(!HARDEN.contains("ALTER ROLE"));
        crate::sql::reject_embedded_transactions(HARDEN, "fresh extension ACL").unwrap();
    }
}
