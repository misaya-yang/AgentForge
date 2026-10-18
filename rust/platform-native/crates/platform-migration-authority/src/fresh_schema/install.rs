use super::execution::{self, stage};
use super::*;
use crate::{
    bootstrap, credentials, ledger,
    runner::{AuthorityPaths, MigrationAuthority, set_role},
};
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeSet, time::Duration};
pub(super) fn target(expected: &str) -> Result<()> {
    if expected != DATABASE {
        return Err(refused(
            "only the dedicated gateway_dev_fresh database is authorized",
        ));
    }
    Ok(())
}
pub(super) fn exact_acl(actual: &[String]) -> Result<()> {
    let policy: Value = serde_json::from_str(include_str!("acl_expected.json"))
        .map_err(|_| refused("compiled final ACL policy malformed"))?;
    let expected: Vec<String> = serde_json::from_value(policy["raw_acl_lines"].clone())
        .map_err(|_| refused("compiled final ACL lines malformed"))?;
    if policy["schema_epoch"] != EPOCH
        || policy["database_owner"] != "owner"
        || actual.len() != expected.len()
        || actual.iter().collect::<BTreeSet<_>>() != expected.iter().collect::<BTreeSet<_>>()
        || expected.iter().collect::<BTreeSet<_>>().len() != expected.len()
    {
        return Err(refused(
            "complete final owner/role/object/column ACL matrix differs from source",
        ));
    }
    Ok(())
}
/// Exact post-install additions: remove only these literal ACL records before
/// checking the unchanged complete business matrix. Unknown grants stay visible.
pub(super) fn metadata_acl() -> Vec<String> {
    let mut lines=vec!["owner:public.platform_fresh_schema_state:owner".to_owned(),"acl:public.platform_fresh_schema_state:owner:DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE:grantopt=0".to_owned()];
    for (schema, name, roles) in [
        ("gateway", "platform_fresh_catalog_v1", &["owner"][..]),
        (
            "gateway",
            "platform_fresh_schema_snapshot_v1",
            &[
                "owner",
                "migrator",
                "gateway",
                "knowledge_api",
                "knowledge_worker",
            ][..],
        ),
        (
            "assistant",
            "platform_fresh_schema_snapshot_v1",
            &["owner", "migrator", "runtime", "capability_worker"][..],
        ),
    ] {
        lines.push(format!("function_owner:{schema}.{name}():owner"));
        for role in roles {
            lines.push(format!(
                "function_acl:{schema}.{name}():{role}:EXECUTE:grantopt=0"
            ));
        }
    }
    lines
}
pub(super) fn exact_installed_acl(actual: &[String]) -> Result<()> {
    let metadata = metadata_acl();
    if metadata
        .iter()
        .any(|line| actual.iter().filter(|v| *v == line).count() != 1)
    {
        return Err(refused(
            "fresh metadata owner/reader ACL differs from its exact matrix",
        ));
    }
    let business = actual
        .iter()
        .filter(|line| !metadata.contains(line))
        .cloned()
        .collect::<Vec<_>>();
    exact_acl(&business)
}
pub(super) async fn login(
    conn: &mut PgConnection,
    prefix: &str,
    principal: &str,
    require_tls: bool,
) -> Result<()> {
    let row=sqlx::query("SELECT current_database()::text AS database,current_user::text AS identity,session_user::text AS session,r.rolcanlogin,r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls OR r.rolinherit AS privileged,has_table_privilege(current_user,'pg_catalog.pg_authid','SELECT') AS secret_catalog FROM pg_catalog.pg_roles r WHERE r.rolname=current_user")
        .fetch_one(&mut *conn).await?;
    if row.try_get::<String, _>("database")? != DATABASE
        || row.try_get::<String, _>("identity")? != format!("{prefix}{principal}")
        || row.try_get::<String, _>("session")? != format!("{prefix}{principal}")
        || !row.try_get::<bool, _>("rolcanlogin")?
        || row.try_get::<bool, _>("privileged")?
        || row.try_get::<bool, _>("secret_catalog")?
    {
        return Err(refused(
            "dedicated unprivileged principal or target mismatch",
        ));
    }
    if require_tls {
        let tls:bool=sqlx::query_scalar("SELECT COALESCE(bool_and(ssl AND client_dn IS NOT NULL AND length(client_dn)>0 AND client_serial IS NOT NULL),false) FROM pg_catalog.pg_stat_ssl WHERE pid=pg_backend_pid()")
            .fetch_one(&mut *conn).await?;
        if !tls {
            return Err(refused(
                "file credentials require authenticated private mTLS",
            ));
        }
    }
    Ok(())
}
fn authority(prefix: &str) -> Result<MigrationAuthority> {
    MigrationAuthority::from_principal(
        "migrator",
        AuthorityPaths {
            database_dir: crate::commands::default_paths().database_dir,
        },
        prefix.into(),
    )
}
async fn principals(prefix: &str, admit: bool) -> Result<()> {
    let mut keys = BTreeSet::new();
    let mut passwords = BTreeSet::new();
    let mut dsns = BTreeSet::new();
    let mut tls_count = 0;
    for principal in
        std::iter::once("migrator").chain(credentials::APPLICATION_PRINCIPALS.iter().copied())
    {
        let config = credentials::connection_for_principal(principal)?;
        if !dsns.insert(config.dsn.clone())
            || config
                .password_identity
                .as_ref()
                .is_some_and(|p| !passwords.insert(p.clone()))
        {
            return Err(refused("principals share a DSN or password"));
        }
        if let Some(key) = &config.tls_key_identity {
            tls_count += 1;
            if !keys.insert(key.clone()) {
                return Err(refused("principals share an mTLS key"));
            }
        }
        let mut conn = stage("principal.connect", Duration::from_secs(10), async {
            PgConnection::connect_with(&config.options)
                .await
                .map_err(|_| refused("principal connection failed; values withheld"))
        })
        .await?;
        stage(
            "principal.identity",
            execution::SQL_LIMIT,
            login(
                &mut conn,
                prefix,
                principal,
                config.tls_key_identity.is_some(),
            ),
        )
        .await?;
        if admit && principal != "migrator" {
            let p = match principal {
                "gateway" => crate::application_guard::ApplicationPrincipal::GatewayDurable,
                "runtime" => crate::application_guard::ApplicationPrincipal::Runtime,
                "capability_worker" => {
                    crate::application_guard::ApplicationPrincipal::CapabilityWorker
                }
                "knowledge_api" => crate::application_guard::ApplicationPrincipal::KnowledgeApi,
                "knowledge_worker" => {
                    crate::application_guard::ApplicationPrincipal::KnowledgeWorker
                }
                _ => return Err(refused("unknown fresh principal")),
            };
            if !stage(
                "principal.current-schema-admission",
                execution::SQL_LIMIT,
                admit_if_fresh(&mut conn, p),
            )
            .await?
            {
                return Err(refused("fresh authority marker unavailable"));
            }
        }
        stage("principal.close", Duration::from_secs(3), async {
            Ok(conn.close().await?)
        })
        .await?;
    }
    if tls_count != 0 && tls_count != 6 {
        return Err(refused("cannot mix file mTLS and legacy local principals"));
    }
    Ok(())
}
async fn execute(conn: &mut PgConnection, sql: &str) -> Result<()> {
    execution::execute(conn, sql).await
}
async fn conditions(conn: &mut PgConnection, conditions: &[String]) -> Result<()> {
    for (index, condition) in conditions.iter().enumerate() {
        let rows = sqlx::raw_sql(condition).fetch_all(&mut *conn).await?;
        if rows.len() != 1 || rows[0].try_get::<bool, _>(0).ok() != Some(true) {
            eprintln!("fresh-schema condition_index={} state=refused", index + 1);
            return Err(refused("complete source condition refused"));
        }
    }
    Ok(())
}
async fn install(
    authority: &MigrationAuthority,
    conn: &mut PgConnection,
    defs: &source::Definitions,
    local_operator: bool,
) -> Result<Value> {
    let mut tx = conn.begin().await?;
    execute(&mut tx,"SET LOCAL statement_timeout='600s';SET LOCAL lock_timeout='5s';SET LOCAL standard_conforming_strings=on;SET LOCAL timezone='UTC';SET LOCAL idle_in_transaction_session_timeout='20s';SET LOCAL client_connection_check_interval='1s'").await?;
    let owner = format!("{}owner", authority.role_prefix);
    let db_owner:String=sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(datdba)::text FROM pg_catalog.pg_database WHERE datname=current_database()")
        .fetch_one(&mut *tx).await?;
    if db_owner != owner {
        return Err(refused(
            "fresh database must belong to the existing NOLOGIN owner",
        ));
    }
    if !bootstrap::database_empty(
        &mut tx,
        &[
            "gateway",
            "assistant",
            "knowledge",
            "public",
            "partition_ops",
        ],
    )
    .await?
    {
        return Err(refused(
            "initialization requires an entirely empty database; use verify for an installed fresh database",
        ));
    }
    set_role(&mut tx, &owner).await?;
    stage(
        "owner.local-schema-setup",
        execution::SQL_LIMIT,
        execute(
            &mut tx,
            &bootstrap::render_role_sql(&defs.owner_setup, &authority.role_prefix)?,
        ),
    )
    .await?;
    stage(
        "owner.role-policy",
        execution::SQL_LIMIT,
        bootstrap::bootstrap_roles(&mut tx, &authority.role_prefix),
    )
    .await?;
    stage(
        "baseline.extensions",
        execution::SQL_LIMIT,
        execute(&mut tx, &defs.extensions),
    )
    .await?;
    if local_operator {
        // Trusted extension members belong to the bootstrap superuser. Only the
        // independent Unix peer operator can revoke their bootstrap PUBLIC ACL;
        // this remains inside the same transaction as every platform definition.
        stage(
            "baseline.extension-private-acl",
            execution::SQL_LIMIT,
            async {
                sqlx::query("SELECT set_config('agentforge.fresh_owner',$1,true)")
                    .bind(&owner)
                    .execute(&mut *tx)
                    .await?;
                execute(&mut tx, "RESET ROLE").await?;
                execute(&mut tx, super::extension_acl::HARDEN).await?;
                set_role(&mut tx, &owner).await?;
                let actual: String = sqlx::query_scalar("SELECT current_user::text")
                    .fetch_one(&mut *tx)
                    .await?;
                if actual != owner {
                    return Err(refused(
                        "extension ACL convergence did not restore NOLOGIN owner",
                    ));
                }
                Ok(())
            },
        )
        .await?;
    }
    for (index, sql) in defs.baseline_sql.iter().enumerate() {
        let sql = if index == 2 {
            bootstrap::render_role_sql(sql, &authority.role_prefix)?
        } else {
            sql.clone()
        };
        let phase = match index {
            0 => "baseline.ddl",
            1 => "baseline.reference-data",
            _ => "baseline.grants",
        };
        stage(phase, Duration::from_secs(60), execute(&mut tx, &sql)).await?;
    }
    let verify_sql = bootstrap::render_role_sql(&defs.verify_sql, &authority.role_prefix)?;
    // Static complete SELECT: use the simple-query API, not prepared
    // Parse/Describe/Sync followed by Bind after a large GRANT result stream.
    let rows = stage("baseline.verify", execution::SQL_LIMIT, async {
        Ok(sqlx::raw_sql(&verify_sql).fetch_all(&mut *tx).await?)
    })
    .await?;
    let mut check_names = BTreeSet::new();
    if rows.is_empty() {
        return Err(refused("initial schema verification returned no checks"));
    }
    for row in rows {
        let name: String = row.try_get("check_name")?;
        let valid_name = !name.is_empty()
            && name.len() <= 160
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_:.-".contains(&b));
        if !valid_name
            || !check_names.insert(name.clone())
            || row.try_get::<bool, _>("ok").ok() != Some(true)
        {
            if valid_name {
                // Names are source-owned check identifiers, never SQL or values.
                eprintln!("fresh-schema baseline_check={name} state=refused");
            }
            return Err(refused(
                "initial schema ownership/privilege/location check failed",
            ));
        }
    }
    stage(
        "authority.empty-ledger-ddl",
        execution::SQL_LIMIT,
        execute(&mut tx, ledger::LEDGER_DDL),
    )
    .await?;
    stage(
        "authority.ledger-acl",
        execution::SQL_LIMIT,
        crate::ledger_acl::install(&mut tx, &authority.role_prefix),
    )
    .await?;
    set_role(&mut tx, &owner).await?;
    for (spec, sql) in &defs.changes {
        sqlx::query(
            "SELECT set_config('statement_timeout',$1,true),set_config('lock_timeout',$2,true)",
        )
        .bind(format!("{}ms", spec.timeout_seconds * 1000))
        .bind(format!("{}ms", spec.lock_budget_seconds * 1000))
        .execute(&mut *tx)
        .await?;
        eprintln!(
            "fresh-schema phase=definition sequence={} state=started",
            spec.sequence
        );
        stage(
            "definition.preconditions",
            execution::SQL_LIMIT,
            conditions(&mut tx, &spec.preconditions),
        )
        .await?;
        stage(
            "definition.ddl",
            execution::SQL_LIMIT,
            execute(&mut tx, sql),
        )
        .await?;
        stage(
            "definition.postconditions",
            execution::SQL_LIMIT,
            conditions(&mut tx, &spec.postconditions),
        )
        .await?;
        eprintln!(
            "fresh-schema phase=definition sequence={} state=complete",
            spec.sequence
        );
    }
    stage(
        "final.builtin-hash-closure",
        execution::SQL_LIMIT,
        super::forward::install_hash_closure(&mut tx, defs),
    )
    .await?;
    // This is an explicit source-owned permission policy, not a measured hash
    // borrowed from an upgrade. No historical ledger rows/receipts are fabricated.
    let acl = stage(
        "final.exact-business-acl",
        execution::SQL_LIMIT,
        crate::fingerprint::acl_lines(&mut tx, &authority.role_prefix),
    )
    .await?;
    exact_acl(&acl)?;
    let partition = stage(
        "final.partition-catalog",
        execution::SQL_LIMIT,
        crate::source_tools::service_successors::partition_catalog::capture(
            &mut tx,
            &authority.role_prefix,
        ),
    )
    .await?;
    stage(
        "final.metadata-readers",
        execution::SQL_LIMIT,
        execute(&mut tx, &catalog::install_sql(&authority.role_prefix)?),
    )
    .await?;
    let observed = stage(
        "final.complete-catalog",
        execution::SQL_LIMIT,
        catalog::capture(&mut tx),
    )
    .await?;
    let fingerprints = stage(
        "final.four-fingerprints",
        execution::SQL_LIMIT,
        crate::fingerprint::compute_fingerprints(
            &mut tx,
            &authority.role_prefix,
            &defs.baseline.reference_data,
        ),
    )
    .await?;
    stage("final.store-real-measurement",execution::SQL_LIMIT,async {
        sqlx::query(&format!("INSERT INTO public.{TABLE}(singleton,source_sha256,catalog,fingerprints,partition_catalog) VALUES(true,$1,$2,$3,$4)"))
            .bind(source_sha256()).bind(&observed).bind(json!(&fingerprints)).bind(&partition).execute(&mut *tx).await?;
        Ok(())
    }).await?;
    let snapshot: Value = stage("final.reader-self-check", execution::SQL_LIMIT, async {
        Ok(
            sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()")
                .fetch_one(&mut *tx)
                .await?,
        )
    })
    .await?;
    validate_snapshot(&snapshot)?;
    stage("final.atomic-commit", execution::SQL_LIMIT, async {
        Ok(tx.commit().await?)
    })
    .await?;
    Ok(
        json!({"status":"initialized_actual_fresh_database","database":DATABASE,"schema_epoch":EPOCH,
        "source_contract_sha256":source_sha256(),"fingerprints":fingerprints,
        "catalog_sha256":sha(&serde_json::to_vec(&observed).map_err(|_|refused("catalog encoding failed"))?),
        "partition_catalog":partition,"definitions_installed":defs.changes.len(),
        "historical_upgrade_acceptance":false,"one_atomic_schema_transaction":true}),
    )
}
pub async fn initialize(expected_database: &str) -> Result<Value> {
    target(expected_database)?;
    stage(
        "fresh.initialize",
        execution::INIT_LIMIT,
        initialize_inner(expected_database, None),
    )
    .await
}
pub(super) async fn initialize_with_operator_connection(
    expected_database: &str,
    options: &sqlx::postgres::PgConnectOptions,
) -> Result<Value> {
    stage(
        "fresh.initialize",
        execution::INIT_LIMIT,
        initialize_inner(expected_database, Some(options)),
    )
    .await
}
async fn initialize_inner(
    expected_database: &str,
    operator: Option<&sqlx::postgres::PgConnectOptions>,
) -> Result<Value> {
    target(expected_database)?;
    let defs = source::compiled_definitions()?;
    let prefix = credentials::role_prefix_from_env()?;
    principals(&prefix, false).await?;
    let authority = authority(&prefix)?;
    let mut conn = if let Some(options) = operator {
        let mut conn = stage("operator.schema-connect", Duration::from_secs(10), async {
            Ok(PgConnection::connect_with(options).await?)
        })
        .await?;
        let valid: bool = sqlx::query_scalar("SELECT current_database()=$1 AND current_user=session_user AND current_user=$2 AND inet_client_addr() IS NULL AND inet_server_addr() IS NULL AND (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)")
            .bind(DATABASE).bind(options.get_username()).fetch_one(&mut conn).await?;
        if !valid {
            return Err(refused(
                "fresh schema operator requires the independent local Unix peer identity",
            ));
        }
        conn
    } else {
        let mut conn = authority.connect(false).await?;
        login(
            &mut conn,
            &prefix,
            "migrator",
            credentials::connection_for_principal("migrator")?
                .tls_key_identity
                .is_some(),
        )
        .await?;
        conn
    };
    authority.acquire_lock(&mut conn).await?;
    if admission::present(&mut conn).await? {
        let mut read = conn.begin().await?;
        set_role(&mut read, &format!("{prefix}owner")).await?;
        let old_source: String = sqlx::query_scalar(
            "SELECT source_sha256 FROM public.platform_fresh_schema_state WHERE singleton",
        )
        .fetch_one(&mut *read)
        .await?;
        read.rollback().await?;
        if old_source != source_sha256() {
            if operator.is_none() {
                return Err(refused(
                    "source-bound fresh repair requires the independent local operator",
                ));
            }
            stage(
                "fresh.exact-d6-forward",
                execution::SQL_LIMIT,
                super::forward::repair(&mut conn, &prefix, &defs),
            )
            .await?;
        }
        authority.release_lock(conn).await?;
        let verified = verify(expected_database).await?;
        return Ok(
            json!({"status":"already_initialized_exact_fresh_database_verified",
            "database":DATABASE,"schema_epoch":EPOCH,"source_contract_sha256":source_sha256(),
            "schema_reexecuted":false,"verification":verified,"historical_upgrade_acceptance":false}),
        );
    }
    let result = install(&authority, &mut conn, &defs, operator.is_some()).await;
    let mut value = match result {
        Ok(value) => value,
        Err(error) => {
            execution::discard(conn).await;
            return Err(error);
        }
    };
    stage(
        "authority.release-connection",
        Duration::from_secs(3),
        authority.release_lock(conn),
    )
    .await?;
    // Actual service-login admission runs only after the committed all-schema
    // transaction; its failure is a real failure and never creates a success receipt.
    let verified = verify(expected_database).await?;
    value["verification"] = verified;
    Ok(value)
}
pub async fn verify(expected_database: &str) -> Result<Value> {
    target(expected_database)?;
    stage(
        "fresh.verify",
        execution::VERIFY_LIMIT,
        verify_inner(expected_database),
    )
    .await
}
async fn verify_inner(expected_database: &str) -> Result<Value> {
    target(expected_database)?;
    let defs = source::compiled_definitions()?;
    let prefix = credentials::role_prefix_from_env()?;
    let authority = authority(&prefix)?;
    let mut conn = authority.connect(true).await?;
    login(
        &mut conn,
        &prefix,
        "migrator",
        credentials::connection_for_principal("migrator")?
            .tls_key_identity
            .is_some(),
    )
    .await?;
    let mut tx = conn.begin().await?;
    set_role(&mut tx, &format!("{prefix}owner")).await?;
    bootstrap::bootstrap_roles(&mut tx, &prefix).await?;
    let stored:Value=sqlx::query_scalar(&format!("SELECT jsonb_build_object('catalog',catalog,'fingerprints',fingerprints,'partition_catalog',partition_catalog,'source_sha256',source_sha256) FROM public.{TABLE} WHERE singleton"))
        .fetch_one(&mut *tx).await?;
    exact_installed_acl(&crate::fingerprint::acl_lines(&mut tx, &prefix).await?)?;
    let observed = catalog::capture(&mut tx).await?;
    let fingerprints =
        crate::fingerprint::compute_fingerprints(&mut tx, &prefix, &defs.baseline.reference_data)
            .await?;
    let partition =
        crate::source_tools::service_successors::partition_catalog::capture(&mut tx, &prefix)
            .await?;
    if stored["source_sha256"] != source_sha256()
        || stored["catalog"] != observed
        || stored["fingerprints"] != json!(&fingerprints)
        || stored["partition_catalog"] != partition
    {
        return Err(refused(
            "actual final catalog, RLS, source fingerprint or permissions drifted",
        ));
    }
    let snapshot: Value = sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()")
        .fetch_one(&mut *tx)
        .await?;
    validate_snapshot(&snapshot)?;
    tx.rollback().await?;
    stage("verify.close-connection", Duration::from_secs(3), async {
        Ok(conn.close().await?)
    })
    .await?;
    principals(&prefix, true).await?;
    Ok(
        json!({"status":"verified_actual_fresh_catalog_and_five_service_logins","database":DATABASE,
        "schema_epoch":EPOCH,"source_contract_sha256":source_sha256(),"fingerprints":fingerprints,
        "all_22_families":true,"catalog_matches":true,"historical_upgrade_acceptance":false,"database_mutations":false}),
    )
}
