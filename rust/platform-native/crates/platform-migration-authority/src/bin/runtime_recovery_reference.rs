mod runtime_recovery_common;
use ai_platform_migration_authority::{
    bootstrap, commands, fingerprint::compute_fingerprints, manifest::load_epoch_manifest,
    runner::MigrationAuthority,
};
use runtime_recovery_common::*;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("Isolated reference generation failed (credentials and driver details withheld)");
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    let mut replace = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--replace-current-reference" => replace = true,
            "--help" | "-h" => {
                println!("runtime_recovery_reference [--replace-current-reference]");
                return Ok(());
            }
            _ => return Err("unknown argument".into()),
        }
    }
    let paths = commands::default_paths();
    let prefix = "p1ref_";
    let name = format!(
        "runtime_reference_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    );
    let opts = options()?;
    let mut admin = PgConnection::connect_with(&opts).await?;
    sqlx::raw_sql(&format!("CREATE DATABASE \"{name}\""))
        .execute(&mut admin)
        .await?;
    admin.close().await?;
    let mut conn = PgConnection::connect_with(&opts.database(&name)).await?;
    let result:Result<()>=async {bootstrap::provision_roles_admin(&mut conn,&paths,prefix).await?;bootstrap::provision_extensions_admin(&mut conn,&paths).await?;let(baseline,sha)=commands::load_baseline(&paths,"2026_08_post_kb_v1")?;bootstrap::fresh_install(&mut conn,&paths,&baseline,&sha,prefix).await?;if compute_fingerprints(&mut conn,prefix,&baseline.reference_data).await?!=baseline.fingerprints(){return Err("fresh frozen baseline fingerprint mismatch".into())}
 let directory=paths.epoch_dir(&baseline.baseline_id);let declared=load_epoch_manifest(&directory.join("manifest.yml"))?;let authority=MigrationAuthority::new("unused-reference-only".into(),paths,prefix.into())?;for sequence in [3,4]{let mut manifest=declared.clone();manifest.epoch=sequence;manifest.changes.retain(|c|c.sequence<=sequence);authority.apply_epoch(&mut conn,&manifest,&directory).await?;let actual=compute_fingerprints(&mut conn,prefix,&baseline.reference_data).await?;let changes=manifest.changes.iter().map(|c|(c.sequence.to_string(),Value::String(c.sha256.clone()))).collect::<serde_json::Map<_,_>>();let record=json!({"baseline_id":baseline.baseline_id,"sequence":sequence,"baseline_manifest_sha256":sha,"changes":changes,"fingerprints":actual,"provenance":"Isolated fresh frozen baseline (all four hashes matched), then authority epochs through this sequence; p1ref_ service roles; PostgreSQL 16; synthetic empty reference database, no application data copied."});let output=directory.join(format!("{sequence:03}_verification.json"));if output.exists(){let existing:Value=serde_json::from_slice(&std::fs::read(&output)?)?;if existing["fingerprints"]!=record["fingerprints"]&&!(sequence==4&&replace){return Err("An existing reference differs; review before replacing it".into())}}write(&output,&record)?;}
 write(std::path::Path::new("tmp/runtime-durable-recovery/reference-database.json"),&json!({"database":name,"role_prefix":prefix,"sequences":[3,4],"frozen_baseline":"four hashes matched"}))?;println!("PASS: isolated baseline and epoch 3/4 reference fingerprints recorded");Ok(())}.await;
    conn.close().await?;
    result
}
