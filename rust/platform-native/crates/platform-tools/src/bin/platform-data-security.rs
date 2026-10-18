use ai_platform_service_primitives::data_connection::{
    ConnectionConfig, ConnectionPurpose, DatabaseRole, TenantPool, TenantScope,
};
use ai_platform_tools::data_security::{self as data, Result};
use std::{collections::BTreeMap, path::PathBuf};
fn required<'a>(opts: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    opts.get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("missing {key}"))
}
async fn run() -> Result<serde_json::Value> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let command = args
        .first()
        .map(String::as_str)
        .ok_or("usage: platform-data-security validate|prepare|prepare-local-env|validate-local-env|validate-material|rotation-bundle|pool-budget|rls-catalog|rls-probe|retention-plan|probe [options]")?;
    let mut opts = BTreeMap::new();
    let mut i = 1;
    while i < args.len() {
        data::ensure(
            [
                "--repo-root",
                "--directory",
                "--database",
                "--client-cidr",
                "--backend-cidr",
                "--password-files",
                "--local-env",
                "--bootstrap-password-file",
                "--openssl",
                "--partition",
                "--cutoff",
                "--role",
                "--tenant",
                "--environment-prefix",
                "--old-ca",
                "--new-ca",
                "--output",
                "--budget",
                "--catalog",
            ]
            .contains(&args[i].as_str()),
            "unknown data-security option",
        )?;
        let value = args.get(i + 1).ok_or("missing option value")?;
        data::ensure(
            opts.insert(args[i].clone(), value.clone()).is_none(),
            "duplicate option",
        )?;
        i += 2;
    }
    let root = opts
        .get("--repo-root")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|_| "working directory unavailable")?);
    match command {
        "validate" => data::validate_repository(&root),
        "pool-budget" => {
            data::ensure(
                opts.keys()
                    .all(|k| ["--budget", "--repo-root"].contains(&k.as_str())),
                "unsupported pool-budget option",
            )?;
            let bytes = data::append_partitions::read_bounded(
                std::path::Path::new(required(&opts, "--budget")?),
                1024 * 1024,
            )?;
            let budget: ai_platform_service_primitives::data_connection::pooling::Budget =
                serde_json::from_slice(&bytes).map_err(|_| "pool budget JSON invalid")?;
            let ini = std::fs::read_to_string(root.join("deploy/data/pgbouncer/pgbouncer.ini"))
                .map_err(|_| "reviewed pooler config unavailable")?;
            data::validate_ini(&ini)?;
            data::ensure(
                budget.role_limits.values().all(|limit| *limit == 64),
                "budget role limits differ from reviewed role bootstrap",
            )?;
            data::ensure(
                budget.poolers.iter().all(|p| {
                    p.max_client_conn == 256
                        && p.max_user_connections == 8
                        && p.max_prepared_statements == 200
                }),
                "budget does not match reviewed physical pooler caps",
            )?;
            serde_json::to_value(budget.validate().map_err(|e| e.to_string())?)
                .map_err(|_| "budget report encoding failed".into())
        }
        "rls-catalog" => {
            data::ensure(
                opts.keys()
                    .all(|k| ["--catalog", "--repo-root"].contains(&k.as_str())),
                "unsupported rls-catalog option",
            )?;
            let bytes = data::append_partitions::read_bounded(
                std::path::Path::new(required(&opts, "--catalog")?),
                4 * 1024 * 1024,
            )?;
            let catalog: data::rls::Catalog =
                serde_json::from_slice(&bytes).map_err(|_| "RLS catalog JSON invalid")?;
            data::rls::verify(&catalog)
        }
        "validate-material" => {
            data::material::validate_material(&PathBuf::from(required(&opts, "--directory")?))
        }
        "validate-local-env" => {
            data::ensure(
                opts.keys()
                    .all(|k| ["--directory", "--repo-root"].contains(&k.as_str())),
                "unsupported local-env validation option",
            )?;
            data::local_env::validate(&PathBuf::from(required(&opts, "--directory")?))
        }
        "rotation-bundle" => data::material::rotation_bundle(
            &PathBuf::from(required(&opts, "--old-ca")?),
            &PathBuf::from(required(&opts, "--new-ca")?),
            &PathBuf::from(required(&opts, "--output")?),
        ),
        "retention-plan" => {
            let p: data::retention::Partition = serde_json::from_slice(
                &std::fs::read(required(&opts, "--partition")?)
                    .map_err(|_| "partition snapshot unavailable")?,
            )
            .map_err(|_| "invalid partition snapshot")?;
            data::retention::plan(
                &p,
                required(&opts, "--cutoff")?
                    .parse()
                    .map_err(|_| "invalid retention cutoff")?,
            )
        }
        "prepare-local-env" => {
            data::ensure(
                opts.keys().all(|k| {
                    [
                        "--repo-root",
                        "--local-env",
                        "--directory",
                        "--database",
                        "--client-cidr",
                        "--backend-cidr",
                        "--openssl",
                    ]
                    .contains(&k.as_str())
                }),
                "unsupported local-env preparation option",
            )?;
            data::local_env::prepare(
                &root,
                &PathBuf::from(required(&opts, "--local-env")?),
                &PathBuf::from(required(&opts, "--directory")?),
                required(&opts, "--database")?,
                required(&opts, "--client-cidr")?,
                required(&opts, "--backend-cidr")?,
                &PathBuf::from(required(&opts, "--openssl")?),
            )
        }
        "prepare" => {
            data::ensure(
                !opts.contains_key("--local-env"),
                "local-env requires explicit prepare-local-env mode",
            )?;
            let passwords: BTreeMap<String, String> = serde_json::from_slice(
                &std::fs::read(required(&opts, "--password-files")?)
                    .map_err(|_| "password path map unavailable")?,
            )
            .map_err(|_| "password path map invalid")?;
            data::material::prepare(
                &root,
                &PathBuf::from(required(&opts, "--directory")?),
                required(&opts, "--database")?,
                required(&opts, "--client-cidr")?,
                required(&opts, "--backend-cidr")?,
                &passwords,
                &PathBuf::from(required(&opts, "--bootstrap-password-file")?),
                &PathBuf::from(required(&opts, "--openssl")?),
            )
        }
        "rls-probe" => {
            data::ensure(
                opts.keys().all(|key| {
                    ["--role", "--environment-prefix", "--repo-root"].contains(&key.as_str())
                }),
                "unsupported rls-probe option",
            )?;
            let role = DatabaseRole::ALL
                .into_iter()
                .find(|role| role.alias() == required(&opts, "--role").unwrap_or(""))
                .ok_or("unknown application role")?;
            let config = ConnectionConfig::from_environment(
                required(&opts, "--environment-prefix")?,
                role,
                ConnectionPurpose::BackendPinned,
                &std::env::vars().collect(),
            )
            .map_err(|e| e.to_string())?;
            data::rls::observe(&config).await
        }
        "probe" => {
            let role = DatabaseRole::ALL
                .into_iter()
                .find(|role| role.alias() == required(&opts, "--role").unwrap_or(""))
                .ok_or("unknown application role")?;
            let env = std::env::vars().collect();
            let config = ConnectionConfig::from_environment(
                required(&opts, "--environment-prefix")?,
                role,
                ConnectionPurpose::TenantTransactions,
                &env,
            )
            .map_err(|e| e.to_string())?;
            let pool = TenantPool::connect(&config)
                .await
                .map_err(|e| e.to_string())?;
            let expected =
                TenantScope::new(required(&opts, "--tenant")?).map_err(|e| e.to_string())?;
            let mut tx = pool.begin(&expected).await.map_err(|e| e.to_string())?;
            let actual: String = sqlx_probe(&mut tx).await?;
            data::ensure(actual == expected.as_str(), "transaction tenant mismatch")?;
            tx.rollback().await.map_err(|e| e.to_string())?;
            pool.close().await;
            Ok(
                serde_json::json!({"status":"passed","scope":"selected-role-mtls-and-transaction-context","application_role":role.name(),"schema_mutations":false,"cross_tenant_rls_acceptance":false}),
            )
        }
        _ => Err("unknown data-security command".into()),
    }
}
async fn sqlx_probe(
    tx: &mut ai_platform_service_primitives::data_connection::TenantTransaction,
) -> Result<String> {
    tx.verify_context().await.map_err(|e| e.to_string())
}
#[tokio::main]
async fn main() {
    match run().await {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
        Err(error) => {
            eprintln!("data security failed: {error}");
            std::process::exit(2);
        }
    }
}
