use ai_platform_migration_authority::{
    AuthorityError, DEFAULT_BASELINE_ID, Result, bootstrap, commands, credentials,
    runner::MigrationAuthority,
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
            "usage: database-authority {{migrate,init-fresh,status,source-kind,verify,startup-check,fingerprint,provision-roles,provision-extensions,bootstrap-ledger-acl,prepare-cutover-ownership,cutover,verify-role-connections}} [--baseline ID] [--principal NAME] [--expected-database NAME] [--no-adoption] [--reconciliation-evidence-out PATH]"
        );
        return Ok(());
    }
    if ![
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
    ]
    .contains(&command.as_str())
    {
        return Err(AuthorityError::Refused("unknown authority command".into()));
    }
    let mut baseline = DEFAULT_BASELINE_ID.to_owned();
    let mut principal = "gateway".to_string();
    let mut expected = None;
    let mut no_adoption = false;
    let mut evidence = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baseline" => {
                baseline = args
                    .next()
                    .ok_or_else(|| AuthorityError::Refused("--baseline requires a value".into()))?
            }
            "--principal" => {
                principal = args
                    .next()
                    .ok_or_else(|| AuthorityError::Refused("--principal requires a value".into()))?
            }
            "--expected-database" => {
                expected = Some(args.next().ok_or_else(|| {
                    AuthorityError::Refused("--expected-database requires a value".into())
                })?)
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
    if !credentials::APPLICATION_PRINCIPALS.contains(&principal.as_str()) {
        return Err(AuthorityError::Refused(
            "unknown application principal".into(),
        ));
    }
    if command == "prepare-cutover-ownership" && expected.as_deref().is_none_or(str::is_empty) {
        return Err(AuthorityError::Refused(
            "prepare-cutover-ownership requires --expected-database".into(),
        ));
    }
    let prefix = credentials::role_prefix_from_env()?;
    let paths = commands::default_paths();
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
        | "prepare-cutover-ownership" => "admin",
        "startup-check" => principal.as_str(),
        _ => "migrator",
    };
    let authority =
        MigrationAuthority::new(credentials::dsn_for_principal(identity)?, paths, prefix)?;
    let outcome: Result<()> = async {
        match command.as_str() {
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
