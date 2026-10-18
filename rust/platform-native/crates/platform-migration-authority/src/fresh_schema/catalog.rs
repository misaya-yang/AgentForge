use super::*;
use sqlx::PgConnection;
pub(super) fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
const SCOPES: &str = "ARRAY['gateway','assistant','knowledge','public','partition_ops']::text[]";
fn aggregate(query: &str) -> String {
    format!(
        "(SELECT COALESCE(jsonb_agg(to_jsonb(fresh_row) ORDER BY to_jsonb(fresh_row)::text COLLATE \"C\"),'[]'::jsonb) FROM ({query}) fresh_row)"
    )
}
/// Every SQL catalog family is captured, including nested enum/domain/composite
/// definitions, all RLS expressions, column ACLs and exact role memberships.
pub(super) fn catalog_sql(prefix: &str) -> Result<String> {
    crate::credentials::validate_role_prefix(prefix)?;
    let queries: BTreeMap<String, String> =
        serde_json::from_str(include_str!("../fingerprint/catalog_queries.json"))
            .map_err(|_| refused("compiled catalog queries invalid"))?;
    let pattern = literal(&crate::fingerprint::like_prefix_pattern(prefix));
    let mut pairs = Vec::new();
    for (key, query) in queries {
        let selected = if matches!(
            key.as_str(),
            "structural_lines/labels"
                | "structural_lines/domain_constraints"
                | "structural_lines/attributes"
        ) {
            // The retained enum query uses an unqualified oid in its nested
            // type lookup. This new all-type reader always parses that branch;
            // qualify it here without editing the frozen query snapshot.
            let query = query.replace(
                "SELECT oid FROM pg_type AS t2",
                "SELECT t2.oid FROM pg_type AS t2",
            );
            let query = query
                .replace("$1", "type_scope.schema")
                .replace("$2", "type_scope.name");
            format!(
                "SELECT type_scope.schema,type_scope.name,to_jsonb(type_row) AS definition FROM (SELECT n.nspname AS schema,t.typname AS name FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname=ANY({SCOPES})) type_scope CROSS JOIN LATERAL ({query}) type_row"
            )
        } else if matches!(key.as_str(), "acl_lines/roles" | "database_acl_lines/rows") {
            query.replace("$1", &pattern)
        } else if key == "extension_catalog_lines/rows" {
            query
        } else {
            query.replace("$1", SCOPES).replace("$2", &pattern)
        };
        if selected.contains("$1") || selected.contains("$2") {
            return Err(refused("unbound fresh catalog argument"));
        }
        pairs.push(format!("{},{}", literal(&key), aggregate(&selected)));
    }
    for (key, query) in [
        (
            "extension/member_acl",
            "SELECT e.extname,n.nspname,p.proname,pg_catalog.oidvectortypes(p.proargtypes) AS arguments,pg_catalog.pg_get_userbyid(p.proowner) AS owner,pg_catalog.pg_get_userbyid(e.extowner) AS extension_owner,p.prokind,p.prosecdef,pg_catalog.pg_get_functiondef(p.oid) AS definition,pg_catalog.pg_get_userbyid(a.grantor) AS grantor,CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE pg_catalog.pg_get_userbyid(a.grantee) END AS grantee,a.privilege_type,a.is_grantable FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')",
        ),
        (
            "extension/type_members",
            "SELECT e.extname,n.nspname,t.typname,pg_catalog.pg_get_userbyid(t.typowner) AS owner,t.typtype,t.typlen,t.typbyval,t.typalign,t.typstorage,t.typcategory,t.typispreferred,t.typdelim,t.typinput::regproc::text AS input,t.typoutput::regproc::text AS output,t.typreceive::regproc::text AS receive,t.typsend::regproc::text AS send,t.typmodin::regproc::text AS modifier_input,t.typmodout::regproc::text AS modifier_output,t.typanalyze::regproc::text AS analyze,CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE pg_catalog.pg_get_userbyid(a.grantee) END AS grantee,pg_catalog.pg_get_userbyid(a.grantor) AS grantor,a.privilege_type,a.is_grantable FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_type'::regclass AND d.objid=t.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(t.typacl,pg_catalog.acldefault('T',t.typowner))) a WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')",
        ),
        (
            "extension/operator_members",
            "SELECT e.extname,n.nspname,o.oprname,pg_catalog.pg_get_userbyid(o.oprowner) AS owner,o.oprkind,pg_catalog.format_type(o.oprleft,NULL) AS left_type,pg_catalog.format_type(o.oprright,NULL) AS right_type,pg_catalog.format_type(o.oprresult,NULL) AS result_type,o.oprcode::regprocedure::text AS implementation,o.oprrest::regproc::text AS restrict_estimator,o.oprjoin::regproc::text AS join_estimator,o.oprcom::regoperator::text AS commutator,o.oprnegate::regoperator::text AS negator,o.oprcanmerge,o.oprcanhash FROM pg_catalog.pg_operator o JOIN pg_catalog.pg_namespace n ON n.oid=o.oprnamespace JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_operator'::regclass AND d.objid=o.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')",
        ),
        (
            "extension/opclass_members",
            "SELECT e.extname,n.nspname,c.opcname,pg_catalog.pg_get_userbyid(c.opcowner) AS owner,m.amname,pg_catalog.format_type(c.opcintype,NULL) AS input_type,pg_catalog.format_type(c.opckeytype,NULL) AS key_type,c.opcdefault,fn.nspname AS family_schema,f.opfname AS family_name FROM pg_catalog.pg_opclass c JOIN pg_catalog.pg_namespace n ON n.oid=c.opcnamespace JOIN pg_catalog.pg_am m ON m.oid=c.opcmethod JOIN pg_catalog.pg_opfamily f ON f.oid=c.opcfamily JOIN pg_catalog.pg_namespace fn ON fn.oid=f.opfnamespace JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_opclass'::regclass AND d.objid=c.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')",
        ),
        (
            "extension/index_operators",
            "SELECT e.extname,n.nspname,f.opfname,a.amopstrategy,a.amoppurpose,pg_catalog.format_type(a.amoplefttype,NULL) AS left_type,pg_catalog.format_type(a.amoprighttype,NULL) AS right_type,a.amopopr::regoperator::text AS operator,sn.nspname AS sort_family_schema,sf.opfname AS sort_family FROM pg_catalog.pg_opfamily f JOIN pg_catalog.pg_namespace n ON n.oid=f.opfnamespace JOIN pg_catalog.pg_amop a ON a.amopfamily=f.oid LEFT JOIN pg_catalog.pg_opfamily sf ON sf.oid=a.amopsortfamily LEFT JOIN pg_catalog.pg_namespace sn ON sn.oid=sf.opfnamespace JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_opfamily'::regclass AND d.objid=f.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')",
        ),
        (
            "extension/index_support",
            "SELECT e.extname,n.nspname,f.opfname,a.amprocnum,pg_catalog.format_type(a.amproclefttype,NULL) AS left_type,pg_catalog.format_type(a.amprocrighttype,NULL) AS right_type,a.amproc::regprocedure::text AS support_function FROM pg_catalog.pg_opfamily f JOIN pg_catalog.pg_namespace n ON n.oid=f.opfnamespace JOIN pg_catalog.pg_amproc a ON a.amprocfamily=f.oid JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_opfamily'::regclass AND d.objid=f.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')",
        ),
        (
            "reference/permissions",
            "SELECT permission_code,name,description,category,resource,action,is_system FROM gateway.permissions WHERE is_system IS TRUE",
        ),
        (
            "reference/roles",
            "SELECT role_name,description,permissions,is_system FROM gateway.rbac_roles WHERE is_system IS TRUE",
        ),
        (
            "reference/role_permissions",
            "SELECT role_name,permission_code FROM gateway.role_permissions WHERE role_name IN (SELECT role_name FROM gateway.rbac_roles WHERE is_system IS TRUE)",
        ),
        (
            "partition/families",
            "SELECT family,clock_column,cadence,safe_retention_predicate FROM partition_ops.families",
        ),
        (
            "partition/statement_policies",
            "SELECT * FROM partition_ops.statement_policies",
        ),
        (
            "authority/baselines",
            "SELECT * FROM public.platform_schema_baselines",
        ),
        (
            "authority/changes",
            "SELECT * FROM public.platform_schema_changes",
        ),
        (
            "authority/attempts",
            "SELECT * FROM public.platform_schema_change_attempts",
        ),
    ] {
        pairs.push(format!("{},{}", literal(key), aggregate(query)));
    }
    Ok(format!(
        "CREATE FUNCTION gateway.platform_fresh_catalog_v1() RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fresh_catalog$ SELECT jsonb_build_object({}) $fresh_catalog$; REVOKE ALL ON FUNCTION gateway.platform_fresh_catalog_v1() FROM PUBLIC;",
        pairs.join(",")
    ))
}
pub(super) fn install_sql(prefix: &str) -> Result<String> {
    crate::credentials::validate_role_prefix(prefix)?;
    let mut sql = format!(
        "CREATE TABLE public.{TABLE}(singleton boolean PRIMARY KEY CHECK(singleton),source_sha256 text NOT NULL CHECK(source_sha256 ~ '^[0-9a-f]{{64}}$'),catalog jsonb NOT NULL,fingerprints jsonb NOT NULL,partition_catalog jsonb NOT NULL,installed_at timestamptz NOT NULL DEFAULT clock_timestamp()); REVOKE ALL ON public.{TABLE} FROM PUBLIC; {}",
        catalog_sql(prefix)?
    );
    for schema in ["gateway", "assistant"] {
        sql.push_str(&format!("CREATE FUNCTION {schema}.platform_fresh_schema_snapshot_v1() RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fresh_reader$ SELECT jsonb_build_object('schema_version','agentforge/fresh-schema-admission/v1','mode','fresh-development','database',current_database(),'schema_epoch',35,'source_contract_sha256',s.source_sha256,'catalog_matches',s.catalog=gateway.platform_fresh_catalog_v1(),'transactional_initialization',true,'historical_upgrade_acceptance',false,'fingerprints',s.fingerprints) FROM public.{TABLE} s WHERE singleton $fresh_reader$; REVOKE ALL ON FUNCTION {schema}.platform_fresh_schema_snapshot_v1() FROM PUBLIC;"));
        let principals: &[&str] = if schema == "assistant" {
            &["runtime", "capability_worker", "migrator"]
        } else {
            &["gateway", "knowledge_api", "knowledge_worker", "migrator"]
        };
        for principal in principals {
            sql.push_str(&format!("GRANT EXECUTE ON FUNCTION {schema}.platform_fresh_schema_snapshot_v1() TO \"{prefix}{principal}\";"));
        }
    }
    Ok(sql)
}
pub(super) async fn capture(conn: &mut PgConnection) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT gateway.platform_fresh_catalog_v1()")
            .fetch_one(conn)
            .await?,
    )
}
