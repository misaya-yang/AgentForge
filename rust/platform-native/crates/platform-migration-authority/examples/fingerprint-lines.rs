//! Isolated acceptance diagnostic. Credentials arrive through the existing env contract.
use ai_platform_migration_authority::{credentials,fingerprint,Result};
use sqlx::{Connection,PgConnection};
#[tokio::main]
async fn main(){if run().await.is_err(){eprintln!("fingerprint diagnostic failed");std::process::exit(1);}}
async fn run()->Result<()>{let mut c=PgConnection::connect(&credentials::dsn_for_principal("migrator")?).await?;let prefix=credentials::role_prefix_from_env()?;let mut tx=c.begin().await?;sqlx::query("SELECT set_config('role',$1,true)").bind(format!("{prefix}owner")).execute(&mut *tx).await?;sqlx::query("SET LOCAL search_path=pg_catalog").execute(&mut *tx).await?;for line in fingerprint::structural_lines(&mut tx).await?{println!("{}",serde_json::to_string(&line).expect("string JSON"));}tx.rollback().await?;Ok(())}
