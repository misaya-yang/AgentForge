//! One exact, measured fresh-engineering predecessor repair; no upgrade history.
use super::*;
use sqlx::{Connection, PgConnection, Row};
const D6: &str = "d6a6a3a3e1c4d3a43af7cbca3c8597e24f0e1e8adf73cc5a9a85d087ee28733a";
const D6_CATALOG: &str = "e13623d50d31128ea9f8aa0a457697f2a3e9fc8389b45f8550ad1e3a6d67c512";
const FUNCTION: &str = "assistant.import_assistant_legacy_session(uuid,character varying,character varying,character varying)";
const PATTERNS: [(&str, &str, usize); 3] = [
    (
        "digest(v_history::text, 'sha256')",
        "pg_catalog.sha256(pg_catalog.convert_to(v_history::text, 'UTF8'))",
        1,
    ),
    (
        "digest(v_payload::text, 'sha256')",
        "pg_catalog.sha256(pg_catalog.convert_to(v_payload::text, 'UTF8'))",
        4,
    ),
    (
        "digest(COALESCE(v_approval->'arguments', '{}'::jsonb)::text, 'sha256')",
        "pg_catalog.sha256(pg_catalog.convert_to(COALESCE(v_approval->'arguments', '{}'::jsonb)::text, 'UTF8'))",
        1,
    ),
];
fn transformed(body: &str) -> Result<String> {
    let mut body = body.to_owned();
    for (old, new, count) in PATTERNS {
        if body.matches(old).count() != count {
            return Err(refused("exact six-site legacy hash source differs"));
        }
        body = body.replace(old, new);
    }
    if body.contains("digest(") {
        return Err(refused("unbound legacy digest call remains"));
    }
    Ok(body)
}
const ENSURE: &str = "assistant.ensure_assistant_runtime_thread(uuid,character varying,character varying,character varying,character varying)";
const APPEND: &str = "assistant.append_assistant_runtime_item(uuid,uuid,character varying,character varying,character varying,uuid,character varying,character varying,character varying,character varying,character varying,character varying,jsonb,character)";
const SAFE_PATH: &str = "pg_catalog, assistant, pg_temp";
fn original_body<'a>(
    defs: &'a source::Definitions,
    name: &str,
    delimiter: &str,
) -> Result<&'a str> {
    defs.baseline_sql[0]
        .split_once(&format!("CREATE FUNCTION assistant.{name}("))
        .and_then(|(_, s)| s.split_once(&format!("AS {delimiter}")))
        .and_then(|(_, s)| s.split_once(&format!("{delimiter};")))
        .map(|(body, _)| body)
        .ok_or_else(|| refused("original pinned routine body absent"))
}
fn baseline_body(defs: &source::Definitions) -> Result<&str> {
    original_body(defs, "import_assistant_legacy_session", "$_$")
}
fn once(body: String, old: &str, new: &str) -> Result<String> {
    if body.matches(old).count() != 1 {
        return Err(refused("exact fresh routine source site differs"));
    }
    Ok(body.replacen(old, new, 1))
}
fn scoped_import(body: &str) -> Result<String> {
    let mut body = transformed(body)?;
    for (variable, table) in [
        ("v_sessions_table", "sessions"),
        ("v_runs_table", "assistant_runs"),
        ("v_commands_table", "assistant_command_queue"),
        ("v_approvals_table", "assistant_tool_approvals"),
    ] {
        let old = format!(
            "{variable} := CASE WHEN current_schema() = 'gateway'\n        THEN COALESCE(to_regclass('assistant.{table}'), to_regclass('{table}'))\n        ELSE to_regclass('{table}') END;"
        );
        body = once(
            body,
            &old,
            &format!("{variable} := pg_catalog.to_regclass('assistant.{table}');"),
        )?;
    }
    body = once(
        body,
        "PERFORM ensure_assistant_runtime_thread(",
        "PERFORM assistant.ensure_assistant_runtime_thread(",
    )?;
    if body
        .matches("PERFORM append_assistant_runtime_item(")
        .count()
        != 4
    {
        return Err(refused("exact four append call sites differ"));
    }
    body = body.replace(
        "PERFORM append_assistant_runtime_item(",
        "PERFORM assistant.append_assistant_runtime_item(",
    );
    if body.contains("current_schema()") {
        return Err(refused(
            "fresh import still has caller-dependent schema routing",
        ));
    }
    Ok(body)
}
fn scoped_ensure(body: &str) -> Result<String> {
    once(
        body.to_owned(),
        "BEGIN\n    IF p_source_kind",
        r#"BEGIN
    PERFORM 1 FROM assistant.sessions AS owned
      WHERE owned.session_id=p_session_id AND owned.tenant_id=p_tenant_id
        AND owned.user_id=p_user_id FOR UPDATE;
    IF NOT FOUND THEN
      RAISE EXCEPTION 'ASSISTANT_RUNTIME_SESSION_NOT_FOUND' USING ERRCODE='42501';
    END IF;
    IF EXISTS(SELECT 1 FROM assistant.assistant_runtime_threads AS existing
      WHERE existing.runtime_thread_id=p_runtime_thread_id
        AND ROW(existing.tenant_id,existing.user_id,existing.session_id,existing.kernel_owner)
          IS DISTINCT FROM ROW(p_tenant_id,p_user_id,p_session_id,'agent_runtime'::varchar)) THEN
      RAISE EXCEPTION 'ASSISTANT_RUNTIME_THREAD_SCOPE_MISMATCH' USING ERRCODE='42501';
    END IF;
    IF p_source_kind"#,
    )
}
async fn install_scoped(
    conn: &mut PgConnection,
    signature: &str,
    old: &str,
    new: &str,
) -> Result<()> {
    let row=sqlx::query("SELECT p.prosrc,p.prosecdef,pg_catalog.pg_get_functiondef(p.oid) AS definition FROM pg_catalog.pg_proc p WHERE p.oid=$1::regprocedure").bind(signature).fetch_one(&mut *conn).await?;
    let body: String = row.try_get("prosrc")?;
    if body == old && !row.try_get::<bool, _>("prosecdef")? {
        let definition: String = row.try_get("definition")?;
        if definition.matches(old).count() != 1 {
            return Err(refused("canonical routine body not unique"));
        }
        execution::execute(conn, &definition.replacen(old, new, 1)).await?;
        execution::execute(conn,&format!("ALTER FUNCTION {signature} SECURITY DEFINER;ALTER FUNCTION {signature} SET search_path=pg_catalog,assistant,pg_temp")).await?;
    } else if body != new || !row.try_get::<bool, _>("prosecdef")? {
        return Err(refused(
            "routine is neither exact original nor fresh scoped definer",
        ));
    }
    let paths: Vec<String> =
        sqlx::query_scalar("SELECT proconfig FROM pg_catalog.pg_proc WHERE oid=$1::regprocedure")
            .bind(signature)
            .fetch_one(&mut *conn)
            .await?;
    if paths != [format!("search_path={SAFE_PATH}")] {
        return Err(refused("scoped definer search path differs"));
    }
    Ok(())
}
pub(super) async fn install_hash_closure(
    conn: &mut PgConnection,
    defs: &source::Definitions,
) -> Result<()> {
    let utf8: bool = sqlx::query_scalar("SELECT current_setting('server_encoding')='UTF8'")
        .fetch_one(&mut *conn)
        .await?;
    if !utf8 {
        return Err(refused("fresh builtin hash equivalence requires UTF8"));
    }
    let ensure = original_body(defs, "ensure_assistant_runtime_thread", "$$")?;
    install_scoped(conn, ENSURE, ensure, &scoped_ensure(ensure)?).await?;
    let import = baseline_body(defs)?;
    install_scoped(conn, FUNCTION, import, &scoped_import(import)?).await?;
    // The existing definer append callee also needs explicit pg_temp-last; no
    // function/body/EXECUTE/table privilege changes are made to this callee.
    let append_safe: bool =
        sqlx::query_scalar("SELECT prosecdef FROM pg_catalog.pg_proc WHERE oid=$1::regprocedure")
            .bind(APPEND)
            .fetch_one(&mut *conn)
            .await?;
    if !append_safe {
        return Err(refused("existing append owner definer absent"));
    }
    execution::execute(
        conn,
        &format!("ALTER FUNCTION {APPEND} SET search_path=pg_catalog,assistant,pg_temp"),
    )
    .await?;
    let closure:bool=sqlx::query_scalar(r#"SELECT NOT EXISTS(
      SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
      JOIN (SELECT DISTINCT member.proname FROM pg_catalog.pg_proc member
        JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_proc'::regclass AND d.objid=member.oid AND d.deptype='e'
        JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid AND d.refclassid='pg_catalog.pg_extension'::regclass
        WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')
        AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_proc builtin JOIN pg_catalog.pg_namespace bn ON bn.oid=builtin.pronamespace WHERE bn.nspname='pg_catalog' AND builtin.proname=member.proname)) extension
      ON p.prosrc ~ ('(^|[^A-Za-z0-9_])' || extension.proname || '[[:space:]]*\(')
      WHERE n.nspname IN ('gateway','assistant','knowledge','partition_ops'))"#)
        .fetch_one(&mut *conn).await?;
    if !closure {
        return Err(refused(
            "platform routine has unresolved trusted-extension executable dependency",
        ));
    }
    extension_private(conn).await?;
    Ok(())
}
async fn extension_private(conn: &mut PgConnection) -> Result<()> {
    let exact:bool=sqlx::query_scalar(r#"SELECT count(*)>0 AND bool_and(n.nspname='public' AND p.prokind='f' AND NOT p.prosecdef AND a.grantor=p.proowner AND a.grantee=p.proowner AND a.privilege_type='EXECUTE' AND NOT a.is_grantable)
      FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
      JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e'
      JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid
      CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a
      WHERE e.extname IN ('pg_trgm','pgcrypto','uuid-ossp')"#).fetch_one(conn).await?;
    if !exact {
        return Err(refused(
            "trusted extension exact private member ACL differs",
        ));
    }
    Ok(())
}
pub(super) async fn repair(
    conn: &mut PgConnection,
    prefix: &str,
    defs: &source::Definitions,
) -> Result<()> {
    let mut tx = conn.begin().await?;
    execution::execute(&mut tx,"SET LOCAL statement_timeout='30s';SET LOCAL lock_timeout='5s';SET LOCAL idle_in_transaction_session_timeout='20s';SET LOCAL client_connection_check_interval='1s'").await?;
    let operator:bool=sqlx::query_scalar("SELECT current_database()='gateway_dev_fresh' AND inet_client_addr() IS NULL AND inet_server_addr() IS NULL AND current_user=session_user AND (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)").fetch_one(&mut *tx).await?;
    if !operator {
        return Err(refused(
            "fresh exact repair requires independent local peer operator",
        ));
    }
    crate::runner::set_role(&mut tx, &format!("{prefix}owner")).await?;
    crate::bootstrap::bootstrap_roles(&mut tx, prefix).await?;
    let stored:Value=sqlx::query_scalar("SELECT jsonb_build_object('source',source_sha256,'catalog',catalog,'fingerprints',fingerprints,'partition',partition_catalog) FROM public.platform_fresh_schema_state WHERE singleton").fetch_one(&mut *tx).await?;
    let observed = catalog::capture(&mut tx).await?;
    let fingerprints =
        crate::fingerprint::compute_fingerprints(&mut tx, prefix, &defs.baseline.reference_data)
            .await?;
    let measured = json!({"acl":"5a486838bf1e0a36dafa339183507b8fa88a1ed1faadf6d24cacd2f95758961a","extensions":"fd20c5b99a0fbe847de60c75290f3135f3a9513fe2ad07b65529ca859973376a","reference_data":"c71c1fd656e4e2559f4c6b09f8cbd1d0b2d57de6e5991bcd2c10d1b068c02318","structural":"61e481600b6aadf09687dc42290d166d45ded92f6b9d1bc48271c49dcd4ea712"});
    let partition =
        crate::source_tools::service_successors::partition_catalog::capture(&mut tx, prefix)
            .await?;
    if stored["source"] != D6
        || observed != stored["catalog"]
        || sha(&serde_json::to_vec(&observed).map_err(|_| refused("catalog encoding"))?)
            != D6_CATALOG
        || json!(&fingerprints) != measured
        || stored["fingerprints"] != measured
        || stored["partition"] != partition
    {
        return Err(refused(
            "only the exact measured d6 fresh predecessor without drift can be repaired",
        ));
    }
    install::exact_installed_acl(&crate::fingerprint::acl_lines(&mut tx, prefix).await?)?;
    extension_private(&mut tx).await?;
    execution::execute(&mut tx, "RESET ROLE").await?;
    let crypto_owner:bool=sqlx::query_scalar(r#"SELECT p.prosrc='pg_digest' AND p.probin='$libdir/pgcrypto' AND l.lanname='c' AND p.prokind='f' AND NOT p.prosecdef AND pg_catalog.pg_get_userbyid(p.proowner)=current_user AND EXISTS(SELECT 1 FROM pg_catalog.pg_depend d JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid WHERE d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.objsubid=0 AND d.refclassid='pg_catalog.pg_extension'::regclass AND d.deptype='e' AND e.extname='pgcrypto') FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_language l ON l.oid=p.prolang WHERE p.oid='public.digest(text,text)'::regprocedure"#).fetch_one(&mut *tx).await?;
    if !crypto_owner {
        return Err(refused(
            "actual extension hash function implementation/owner differs",
        ));
    }
    // Cross-check builtin hashing against the actual extension owner on bytes,
    // including multibyte JSON and empty text. This does not grant any privilege.
    let hashes:bool=sqlx::query_scalar("SELECT bool_and(public.digest(v,'sha256')=pg_catalog.sha256(pg_catalog.convert_to(v,'UTF8'))) FROM unnest(ARRAY['','abc','{\"tool\":\"工具 🔑\",\"n\":1}','[null,true,42]']::text[]) v").fetch_one(&mut *tx).await?;
    if !hashes {
        return Err(refused(
            "actual extension/builtin UTF8 hash equivalence failed",
        ));
    }
    crate::runner::set_role(&mut tx, &format!("{prefix}owner")).await?;
    // Exercise the real extension opclass support closure as the normal index
    // DDL owner, without any extension EXECUTE grant or permanent fixture.
    execution::execute(&mut tx,"CREATE TEMP TABLE fresh_extension_index_probe(value text) ON COMMIT DROP;INSERT INTO fresh_extension_index_probe VALUES('authority'),('工具');CREATE INDEX fresh_extension_index_probe_idx ON fresh_extension_index_probe USING gin(value public.gin_trgm_ops);DROP TABLE fresh_extension_index_probe").await?;
    install_hash_closure(&mut tx, defs).await?;
    crate::runner::set_role(&mut tx, &format!("{prefix}gateway")).await?;
    // Exercise the exact application privilege context before committing a new
    // source, with every business fixture rolled back to a nested savepoint.
    business::exercise(&mut tx, prefix, true).await?;
    crate::runner::set_role(&mut tx, &format!("{prefix}owner")).await?;
    let ddl = catalog::catalog_sql(prefix)?;
    let signature = "CREATE FUNCTION gateway.platform_fresh_catalog_v1()";
    if ddl.matches(signature).count() != 1 {
        return Err(refused("fresh catalog replacement signature differs"));
    }
    execution::execute(
        &mut tx,
        &ddl.replacen(
            signature,
            "CREATE OR REPLACE FUNCTION gateway.platform_fresh_catalog_v1()",
            1,
        ),
    )
    .await?;
    install::exact_installed_acl(&crate::fingerprint::acl_lines(&mut tx, prefix).await?)?;
    let observed = catalog::capture(&mut tx).await?;
    let fingerprints =
        crate::fingerprint::compute_fingerprints(&mut tx, prefix, &defs.baseline.reference_data)
            .await?;
    sqlx::query("UPDATE public.platform_fresh_schema_state SET source_sha256=$1,catalog=$2,fingerprints=$3,partition_catalog=$4 WHERE singleton")
        .bind(source_sha256()).bind(observed).bind(json!(&fingerprints)).bind(partition).execute(&mut *tx).await?;
    let snapshot: Value = sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()")
        .fetch_one(&mut *tx)
        .await?;
    validate_snapshot(&snapshot)?;
    tx.commit().await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exactly_six_frozen_text_sha_sites_change_without_privilege_or_semantic_changes() {
        let defs = source::compiled_definitions().unwrap();
        let old = baseline_body(&defs).unwrap();
        let new = transformed(old).unwrap();
        assert_eq!(old.matches("digest(").count(), 6);
        assert_eq!(
            new.matches("pg_catalog.sha256(pg_catalog.convert_to(")
                .count(),
            6
        );
        assert_eq!(
            new.replace(
                "pg_catalog.sha256(pg_catalog.convert_to(v_history::text, 'UTF8'))",
                PATTERNS[0].0
            )
            .replace(
                "pg_catalog.sha256(pg_catalog.convert_to(v_payload::text, 'UTF8'))",
                PATTERNS[1].0
            )
            .replace(PATTERNS[2].1, PATTERNS[2].0),
            old
        );
        for bad in [
            old.replacen(PATTERNS[0].0, "digest(v_history::text, 'sha512')", 1),
            format!("{old}\ndigest(v_history::text, 'sha256')"),
        ] {
            assert!(transformed(&bad).is_err());
        }
        assert!(transformed(&new).is_err());
    }
}

#[cfg(test)]
mod scoped_tests {
    use super::*;
    #[test]
    fn source_owned_definers_require_exact_session_scope_and_fixed_routes() {
        let defs = source::compiled_definitions().unwrap();
        let old = baseline_body(&defs).unwrap();
        let body = scoped_import(old).unwrap();
        assert!(!body.contains("current_schema()"));
        assert_eq!(body.matches("pg_catalog.sha256(").count(), 6);
        for table in [
            "sessions",
            "assistant_runs",
            "assistant_command_queue",
            "assistant_tool_approvals",
        ] {
            assert!(body.contains(&format!("pg_catalog.to_regclass('assistant.{table}')")));
        }
        assert_eq!(
            body.matches("PERFORM assistant.append_assistant_runtime_item(")
                .count(),
            4
        );
        assert!(
            scoped_import(&old.replacen(
                "current_schema() = 'gateway'",
                "current_schema() = 'public'",
                1
            ))
            .is_err()
        );
        let old = original_body(&defs, "ensure_assistant_runtime_thread", "$$").unwrap();
        let body = scoped_ensure(old).unwrap();
        for check in [
            "owned.session_id=p_session_id",
            "owned.tenant_id=p_tenant_id",
            "owned.user_id=p_user_id FOR UPDATE",
            "existing.kernel_owner",
            "IS DISTINCT FROM",
            "ASSISTANT_RUNTIME_SESSION_NOT_FOUND",
        ] {
            assert!(body.contains(check));
        }
        assert!(
            scoped_ensure(&old.replacen(
                "BEGIN\n    IF p_source_kind",
                "BEGIN\n    IF malicious",
                1
            ))
            .is_err()
        );
        assert_eq!(SAFE_PATH, "pg_catalog, assistant, pg_temp");
    }
}
