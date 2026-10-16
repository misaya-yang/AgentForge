//! Catalog SQL retained verbatim from Python bootstrap.
pub const _ROLE_BOOTSTRAP_STATE_SQL: &str = r###"
/* arc03-role-bootstrap-state */
SELECT role.rolname, role.rolcanlogin, role.rolinherit, role.rolsuper,
       role.rolcreatedb, role.rolcreaterole, role.rolreplication,
       role.rolbypassrls, role.rolconfig,
       ARRAY(
           SELECT granted.rolname
           FROM pg_auth_members AS membership
           JOIN pg_roles AS granted ON granted.oid = membership.roleid
           WHERE membership.member = role.oid
           ORDER BY granted.rolname
       ) AS memberships
FROM pg_roles AS role
WHERE role.rolname = ANY($1::text[])
ORDER BY role.rolname
"###;
pub const _SCHEMA_BOOTSTRAP_STATE_SQL: &str = r###"
/* arc03-schema-bootstrap-state */
SELECT namespace.nspname,
       pg_get_userbyid(namespace.nspowner) AS owner,
       EXISTS (
           SELECT 1
           FROM aclexplode(
               COALESCE(namespace.nspacl, acldefault('n', namespace.nspowner))
           ) AS privilege
           WHERE privilege.grantee = 0
             AND privilege.privilege_type = 'CREATE'
       ) AS public_create,
       ARRAY(
           SELECT principal
           FROM unnest($2::text[]) AS principal
           WHERE has_schema_privilege(principal, namespace.nspname, 'CREATE')
           ORDER BY principal
       ) AS create_roles
FROM pg_namespace AS namespace
WHERE namespace.nspname = ANY($1::text[])
ORDER BY namespace.nspname
"###;
pub const _DEFAULT_ACL_BOOTSTRAP_STATE_SQL: &str = r###"
/* arc03-default-acl-bootstrap-state */
WITH owner_role AS (
    SELECT oid FROM pg_roles WHERE rolname = $1
), desired(nspname, objtype) AS (
    SELECT schema_name, object_type
    FROM unnest($2::text[]) AS schema_name
    CROSS JOIN (VALUES ('r'::"char"), ('S'::"char"), ('f'::"char"), ('T'::"char"))
        AS object_types(object_type)
)
SELECT desired.nspname, desired.objtype::text,
       EXISTS (
           SELECT 1
           FROM aclexplode(
               COALESCE(global_acl.defaclacl, acldefault(desired.objtype, owner_role.oid))
           ) AS privilege
           WHERE privilege.grantee = 0
       ) OR EXISTS (
           SELECT 1
           FROM aclexplode(schema_acl.defaclacl) AS privilege
           WHERE privilege.grantee = 0
       ) AS public_has_privilege
FROM desired
CROSS JOIN owner_role
JOIN pg_namespace AS namespace ON namespace.nspname = desired.nspname
LEFT JOIN pg_default_acl AS global_acl
  ON global_acl.defaclrole = owner_role.oid
 AND global_acl.defaclnamespace = 0
 AND global_acl.defaclobjtype = desired.objtype
LEFT JOIN pg_default_acl AS schema_acl
  ON schema_acl.defaclrole = owner_role.oid
 AND schema_acl.defaclnamespace = namespace.oid
 AND schema_acl.defaclobjtype = desired.objtype
ORDER BY desired.nspname, desired.objtype
"###;
pub const _DATABASE_BOOTSTRAP_STATE_SQL: &str = r###"
/* arc03-database-bootstrap-state */
SELECT EXISTS (
           SELECT 1
           FROM pg_database AS database
           CROSS JOIN LATERAL aclexplode(
               COALESCE(database.datacl, acldefault('d', database.datdba))
           ) AS privilege
           WHERE database.datname = current_database()
             AND privilege.grantee = 0
             AND privilege.privilege_type = 'CREATE'
       ) AS public_create,
       ARRAY(
           SELECT principal
           FROM unnest($1::text[]) AS principal
           WHERE has_database_privilege(principal, current_database(), 'CREATE')
           ORDER BY principal
       ) AS create_roles
"###;
pub const _ROUTINE_ACL_HARDENING_SQL: &str = r###"
/* arc03-admin-extension-acl-hardening */
REVOKE EXECUTE ON ALL ROUTINES IN SCHEMA public, gateway, assistant, knowledge FROM PUBLIC
"###;
pub const DATABASE_EMPTY_SQL: &str = r###"
        SELECT sum(object_count)
        FROM (
            SELECT count(*) AS object_count
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND c.relkind IN ('r', 'p', 'm', 'S', 'v', 'f', 'c')
              AND NOT EXISTS (
                  SELECT 1 FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_class'::regclass
                    AND dependency.objid = c.oid
                    AND dependency.deptype = 'e'
              )
            UNION ALL
            SELECT count(*)
            FROM pg_proc AS p
            JOIN pg_namespace AS n ON n.oid = p.pronamespace
            WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND NOT EXISTS (
                  SELECT 1 FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_proc'::regclass
                    AND dependency.objid = p.oid
                    AND dependency.deptype = 'e'
              )
            UNION ALL
            SELECT count(*)
            FROM pg_type AS t
            JOIN pg_namespace AS n ON n.oid = t.typnamespace
            WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND t.typrelid = 0
              AND t.typelem = 0
              AND t.typtype IN ('b', 'c', 'd', 'e', 'm', 'r')
              AND NOT EXISTS (
                  SELECT 1 FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_type'::regclass
                    AND dependency.objid = t.oid
                    AND dependency.deptype = 'e'
              )
            UNION ALL
            SELECT count(*)
            FROM pg_namespace AS n
            WHERE n.nspname NOT IN ('public', 'pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND NOT (n.nspname = ANY($1::text[]))
            UNION ALL
            SELECT count(*)
            FROM pg_extension
            WHERE extname <> ALL($2::text[])
        ) AS user_objects
        "###;
