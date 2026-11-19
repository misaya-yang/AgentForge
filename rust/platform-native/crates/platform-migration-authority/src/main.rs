use ai_platform_migration_authority::{
    AuthorityError, DEFAULT_BASELINE_ID, Result, bootstrap, commands, credential_bootstrap,
    credentials, runner::MigrationAuthority,
};
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("authority: {error}");
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(|| {
        AuthorityError::Refused(
            "usage: database-authority COMMAND [--baseline ID] [--principal NAME]".into(),
        )
    })?;
    if matches!(command.as_str(), "--help" | "-h") {
        println!(
            "usage: database-authority {{fresh-development-init,fresh-development-verify,fresh-development-contract,migrate,init-fresh,status,source-kind,verify,startup-check,fingerprint,provision-roles,provision-extensions,bootstrap-ledger-acl,prepare-cutover-ownership,cutover,verify-role-connections,initialize-local-credentials,deployment-bootstrap,deployment-file-admission}} [--baseline ID] [--principal NAME] [--expected-database NAME] [--expected-admin NAME] [--no-adoption] [--reconciliation-evidence-out PATH] [--phase pre-migration|post-migration]"
        );
        return Ok(());
    }
    if ![
        "fresh-development-init",
        "fresh-development-verify",
        "fresh-development-contract",
        "fresh-development-acceptance",
        "migrate",
        "init-fresh",
        "status",
        "source-kind",
        "verify",
        "startup-check",
        "fingerprint",
        "provision-roles",
        "provision-extensions",
        "bootstrap-ledger-acl",
        "prepare-cutover-ownership",
        "cutover",
        "verify-role-connections",
        "initialize-local-credentials",
        "deployment-bootstrap",
        "deployment-file-admission",
    ]
    .contains(&command.as_str())
    {
        return Err(AuthorityError::Refused("unknown authority command".into()));
    }
    let mut baseline = DEFAULT_BASELINE_ID.to_owned();
    let mut principal = "gateway".to_string();
    let mut expected = None;
    let mut expected_admin = None;
    let mut no_adoption = false;
    let mut evidence = None;
    let mut phase = None;
    let mut principal_selected = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baseline" => {
                baseline = args
                    .next()
                    .ok_or_else(|| AuthorityError::Refused("--baseline requires a value".into()))?
            }
            "--principal" => {
                principal_selected = true;
                principal = args
                    .next()
                    .ok_or_else(|| AuthorityError::Refused("--principal requires a value".into()))?
            }
            "--expected-database" => {
                expected = Some(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--expected-database requires a value".into())
                })?)
            }
            "--expected-admin" => {
                expected_admin = Some(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--expected-admin requires a value".into())
                })?);
            }
            "--phase" => {
                if phase.is_some() {
                    return Err(AuthorityError::Refused("duplicate phase".into()));
                }
                phase = Some(
                    ai_platform_migration_authority::deployment_file_admission::Phase::parse(
                        &args.next().ok_or_else(|| {
                            AuthorityError::Refused("phase value required".into())
                        })?,
                    )?,
                );
            }
            "--no-adoption" => no_adoption = true,
            "--reconciliation-evidence-out" => {
                evidence = Some(std::path::PathBuf::from(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--reconciliation-evidence-out requires a value".into())
                })?))
            }
            _ => return Err(AuthorityError::Refused("unknown authority argument".into())),
        }
    }
    if command.starts_with("fresh-development-") {
        if principal_selected
            || expected_admin.is_some()
            || no_adoption
            || evidence.is_some()
            || phase.is_some()
            || baseline != DEFAULT_BASELINE_ID
        {
            return Err(AuthorityError::Refused(
                "fresh development entry does not accept legacy/adoption/admin-TCP flags".into(),
            ));
        }
        let value = if command == "fresh-development-contract" {
            if expected.is_some() {
                return Err(AuthorityError::Refused(
                    "fresh contract takes no database arguments".into(),
                ));
            }
            ai_platform_migration_authority::fresh_schema::admission_contract()
        } else {
            let expected = expected.as_deref().ok_or_else(|| {
                AuthorityError::Refused(
                    "fresh development entry requires --expected-database gateway_dev_fresh".into(),
                )
            })?;
            if command == "fresh-development-init" {
                ai_platform_migration_authority::fresh_schema::initialize_with_local_operator(
                    expected,
                    &ai_platform_migration_authority::fresh_schema::LocalOperator::from_env()?,
                )
                .await?
            } else if command == "fresh-development-acceptance" {
                ai_platform_migration_authority::fresh_schema::acceptance(expected).await?
            } else {
                ai_platform_migration_authority::fresh_schema::verify(expected).await?
            }
        };
        println!("{value}");
        return Ok(());
    }
    if command == "deployment-file-admission" {
        if expected.as_deref().is_none_or(str::is_empty)
            || expected_admin.is_some()
            || principal_selected
            || no_adoption
            || evidence.is_some()
            || std::env::var("AI_PLATFORM_LOCAL_DEV_CREDENTIALS").as_deref() == Ok("true")
        {
            return Err(AuthorityError::Refused("deployment-file-admission requires expected-database, optional phase, six file/mTLS roles and no admin/local/migration flags".into()));
        }
        let prefix = credentials::role_prefix_from_env()?;
        let paths = commands::default_paths();
        commands::verify_source(&paths, &baseline)?;
        println!(
            "{}",
            ai_platform_migration_authority::deployment_file_admission::run(
                paths,
                prefix,
                &baseline,
                expected.as_deref().unwrap(),
                phase.unwrap_or(
                    ai_platform_migration_authority::deployment_file_admission::Phase::PreMigration
                )
            )
            .await?
        );
        return Ok(());
    }
    if phase.is_some() {
        return Err(AuthorityError::Refused(
            "phase only applies to deployment-file-admission".into(),
        ));
    }
    if !credentials::APPLICATION_PRINCIPALS.contains(&principal.as_str()) {
        return Err(AuthorityError::Refused(
            "unknown application principal".into(),
        ));
    }
    if matches!(
        command.as_str(),
        "prepare-cutover-ownership" | "initialize-local-credentials"
    ) && expected.as_deref().is_none_or(str::is_empty)
    {
        return Err(AuthorityError::Refused(
            "administrator setup requires --expected-database".into(),
        ));
    }
    if command == "deployment-bootstrap" {
        if expected.as_deref().is_none_or(str::is_empty)
            || expected_admin.as_deref().is_none_or(str::is_empty)
            || no_adoption
            || evidence.is_some()
            || std::env::var("AI_PLATFORM_LOCAL_DEV_CREDENTIALS").as_deref() == Ok("true")
        {
            return Err(AuthorityError::Refused(
                "deployment-bootstrap requires --expected-database and --expected-admin, cannot use local credential opt-in or migration-only flags".into(),
            ));
        }
    } else if expected_admin.is_some() {
        return Err(AuthorityError::Refused(
            "--expected-admin is only available for deployment-bootstrap".into(),
        ));
    }
    let prefix = credentials::role_prefix_from_env()?;
    let paths = commands::default_paths();
    if command == "initialize-local-credentials"
        || std::env::var("AI_PLATFORM_LOCAL_DEV_CREDENTIALS").as_deref() == Ok("true")
    {
        let local_database = expected
            .clone()
            .or_else(|| std::env::var("POSTGRES_DB").ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AuthorityError::Refused("explicit local expected database required".into())
            })?;
        credential_bootstrap::verify_local_target(&paths, &prefix, &local_database).await?;
    }
    commands::verify_source(&paths, &baseline)?;
    if command == "verify-role-connections" {
        let count = credentials::verify_role_connections(&prefix, None).await?;
        println!("verified {count} distinct database role connections");
        return Ok(());
    }
    let identity = match command.as_str() {
        "provision-roles"
        | "provision-extensions"
        | "bootstrap-ledger-acl"
        | "prepare-cutover-ownership"
        | "initialize-local-credentials"
        | "deployment-bootstrap" => "admin",
        "startup-check" => principal.as_str(),
        _ => "migrator",
    };
    let authority = MigrationAuthority::from_principal(identity, paths, prefix)?;
    let outcome: Result<()> = async {
        match command.as_str() {
            "deployment-bootstrap" => println!(
                "{}",
                credential_bootstrap::deployment_bootstrap(
                    &authority,
                    &baseline,
                    expected.as_deref().unwrap(),
                    expected_admin.as_deref().unwrap()
                )
                .await?
            ),
            "initialize-local-credentials" => println!(
                "{}",
                credential_bootstrap::initialize(&authority, expected.as_deref().unwrap()).await?
            ),
            "migrate" => {
                commands::migrate(&authority, &baseline, !no_adoption, true, false).await?
            }
            "cutover" => commands::migrate(&authority, &baseline, true, false, false).await?,
            "init-fresh" => commands::migrate(&authority, &baseline, true, true, true).await?,
            "startup-check" => commands::startup_check(&authority, &principal).await?,
            "status" => commands::status(&authority).await?,
            "source-kind" => commands::source_kind(&authority).await?,
            "verify" => commands::verify(&authority, &baseline).await?,
            "fingerprint" => commands::fingerprint(&authority, &baseline).await?,
            "bootstrap-ledger-acl" => println!(
                "{}",
                commands::bootstrap_ledger_acl(&authority, &baseline).await?
            ),
            "provision-roles" => {
                let mut conn = authority.connect(false).await?;
                bootstrap::provision_roles_admin(
                    &mut conn,
                    &authority.paths,
                    &authority.role_prefix,
                )
                .await?;
            }
            "prepare-cutover-ownership" => {
                let expected = expected
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        AuthorityError::Refused(
                            "prepare-cutover-ownership requires --expected-database".into(),
                        )
                    })?;
                ai_platform_migration_authority::legacy::prepare_cutover_ownership(
                    &authority, &baseline, expected,
                )
                .await?;
            }
            "provision-extensions" => {
                commands::load_baseline(&authority.paths, &baseline)?;
                let mut conn = authority.connect(false).await?;
                authority.acquire_lock(&mut conn).await?;
                let provisioned =
                    bootstrap::provision_extensions_admin(&mut conn, &authority.paths).await;
                let released = authority.release_lock(conn).await;
                provisioned.and(released)?;
                println!(
                    "extension provisioning completed under the separate administrator connection"
                );
            }
            _ => return Err(AuthorityError::Refused("unknown authority command".into())),
        }
        Ok(())
    }
    .await;
    if let Some(path) = evidence {
        if !matches!(
            command.as_str(),
            "migrate" | "cutover" | "prepare-cutover-ownership"
        ) {
            return Err(AuthorityError::Refused(
                "reconciliation evidence is only available for migration commands".into(),
            ));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let receipt = serde_json::json!({"exit_code":if outcome.is_ok(){0}else{1},"numeric_reconciliation":authority.reconciliation_receipt().unwrap_or_else(||serde_json::json!({"verdict":"not_applicable"}))});
        std::fs::write(
            path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&receipt)
                    .map_err(|_| AuthorityError::Refused("evidence serialization failed".into()))?
            ),
        )?;
    }
    outcome
}
