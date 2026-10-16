use crate::{
    AuthorityError, DEFAULT_BASELINE_ID, Result,
    bootstrap::{self, validate_adoption_marker},
    manifest::{self, BaselineManifest},
    runner::{AuthorityPaths, MigrationAuthority, set_role, validate_applied_prefix},
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeMap, path::Path};
pub fn default_paths() -> AuthorityPaths {
    let path = std::env::var_os("AI_GATEWAY_DATABASE_DIR")
        .map(Into::into)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database"));
    AuthorityPaths { database_dir: path }
}
pub fn load_baseline(paths: &AuthorityPaths, id: &str) -> Result<(BaselineManifest, String)> {
    load_baseline_with_mode(paths, id, crate::distribution::SourceMode::from_process()?)
}
pub fn verify_source(paths: &AuthorityPaths, id: &str) -> Result<()> {
    match crate::distribution::SourceMode::from_process()? {
        crate::distribution::SourceMode::Distribution => {
            crate::distribution::verify(&paths.database_dir, id)?
        }
        crate::distribution::SourceMode::Development => {
            let manifest = paths.baseline_dir(id).join("manifest.json");
            if manifest.is_file() && manifest::baseline_artifact_state(&manifest)? == "frozen" {
                // Frozen development inputs retain the original provenance
                // check, including before role provisioning or any fallback.
                load_baseline_with_mode(paths, id, crate::distribution::SourceMode::Development)?;
            }
        }
    }
    Ok(())
}
pub fn load_baseline_with_mode(
    paths: &AuthorityPaths,
    id: &str,
    mode: crate::distribution::SourceMode,
) -> Result<(BaselineManifest, String)> {
    // A baseline argument is never accepted as a filesystem path.
    if id != DEFAULT_BASELINE_ID {
        return Err(AuthorityError::Refused("unsupported baseline".into()));
    }
    let path = paths.baseline_dir(id).join("manifest.json");
    let baseline = manifest::load_baseline_manifest(&path)?;
    let root = paths
        .database_dir
        .parent()
        .ok_or_else(|| AuthorityError::Refused("invalid database artifact root".into()))?;
    if mode == crate::distribution::SourceMode::Distribution {
        crate::distribution::verify(&paths.database_dir, id)?;
    } else if root.join(".git").exists() {
        manifest::verify_baseline_git_provenance(&path, &baseline, root)?;
    }
    let legacy =
        manifest::load_legacy_manifest(&paths.migrations_root().join("legacy-manifest.yml"))?;
    if legacy.freeze_point != baseline.last_legacy_change {
        return Err(AuthorityError::Refused(
            "baseline freeze point disagrees with immutable legacy manifest".into(),
        ));
    }
    Ok((baseline, manifest::file_sha256(&path)?))
}
pub fn expected_epoch_fingerprints(
    paths: &AuthorityPaths,
    baseline: &BaselineManifest,
    applied: &BTreeMap<i32, String>,
) -> Result<BTreeMap<String, String>> {
    let Some((&sequence, _)) = applied.last_key_value() else {
        return Ok(baseline.fingerprints());
    };
    let directory = paths.epoch_dir(&baseline.baseline_id);
    let manifest = manifest::load_epoch_manifest(&directory.join("manifest.yml"))?;
    validate_applied_prefix(&manifest, applied)?;
    let value: Value = serde_json::from_slice(&std::fs::read(
        directory.join(format!("{sequence:03}_verification.json")),
    )?)
    .map_err(|_| AuthorityError::Refused("invalid epoch verification reference".into()))?;
    let changes = applied
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect::<serde_json::Map<_, _>>();
    let fingerprints: BTreeMap<String, String> =
        serde_json::from_value(value["fingerprints"].clone())
            .map_err(|_| AuthorityError::Refused("invalid epoch fingerprints".into()))?;
    let valid = value["baseline_id"] == baseline.baseline_id
        && value["sequence"] == sequence
        && value["changes"] == Value::Object(changes)
        && value["baseline_manifest_sha256"]
            == manifest::file_sha256(
                &paths
                    .baseline_dir(&baseline.baseline_id)
                    .join("manifest.json"),
            )?
        && fingerprints.keys().eq(baseline.fingerprints().keys())
        && fingerprints.values().all(|v| {
            v.len() == 64
                && v.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        });
    if !valid {
        return Err(AuthorityError::Refused(
            "epoch reference fingerprints do not bind current baseline and changes".into(),
        ));
    }
    Ok(fingerprints)
}
pub async fn require_ledger_state(
    authority: &MigrationAuthority,
    conn: &mut PgConnection,
    baseline: &BaselineManifest,
) -> Result<BTreeMap<String, String>> {
    let applied = authority
        .applied_changes(conn, &baseline.baseline_id)
        .await?;
    let expected = expected_epoch_fingerprints(&authority.paths, baseline, &applied)?;
    let expected =
        crate::ledger_acl::compatible_expected(conn, &expected, &authority.role_prefix).await?;
    let mut tx = conn.begin().await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *tx)
        .await?;
    set_role(&mut tx, &format!("{}owner", authority.role_prefix)).await?;
    let actual = crate::fingerprint::compute_fingerprints(
        &mut tx,
        &authority.role_prefix,
        &baseline.reference_data,
    )
    .await?;
    if actual != expected {
        return Err(AuthorityError::Refused(
            "authority ledger compatibility gate detected fingerprint drift".into(),
        ));
    }
    set_role(&mut tx, &previous).await?;
    tx.commit().await?;
    Ok(actual)
}
pub fn baseline_ready(paths: &AuthorityPaths, id: &str) -> Result<bool> {
    if id != DEFAULT_BASELINE_ID {
        return Err(AuthorityError::Refused("unsupported baseline".into()));
    }
    let directory = paths.baseline_dir(id);
    if [
        "manifest.json",
        "init.sql",
        "reference_data.sql",
        "grants.sql",
        "verify.sql",
        "cutover_convergence.sql",
    ]
    .iter()
    .any(|name| !directory.join(name).exists())
    {
        return Ok(false);
    }
    Ok(manifest::baseline_artifact_state(&directory.join("manifest.json"))? == "frozen")
}
pub async fn migrate(
    authority: &MigrationAuthority,
    baseline_id: &str,
    allow_adoption: bool,
    allow_fresh: bool,
    init_fresh: bool,
) -> Result<()> {
    verify_source(&authority.paths, baseline_id)?;
    if !baseline_ready(&authority.paths, baseline_id)? {
        if init_fresh {
            return Err(AuthorityError::Refused(
                "baseline is not frozen; refusing fresh install".into(),
            ));
        }
        println!(
            "authority: baseline files not frozen yet — compatibility mode (legacy chain only, no adoption)"
        );
        let mut lock = authority.connect(false).await?;
        authority.acquire_lock(&mut lock).await?;
        let outcome = async {
            let mut conn = authority.connect(false).await?;
            if authority.adopted_baseline(&mut conn).await?.is_some() {
                return Err(AuthorityError::Refused(
                    "database carries adopted baseline but frozen local manifest is unavailable"
                        .into(),
                ));
            }
            crate::legacy::migrate_legacy_only(authority, &mut conn).await?;
            conn.close().await?;
            Ok(())
        }
        .await;
        let released = authority.release_lock(lock).await;
        return outcome.and(released);
    }
    let (baseline, sha) = load_baseline(&authority.paths, baseline_id)?;
    let mut lock = authority.connect(false).await?;
    authority.acquire_lock(&mut lock).await?;
    let outcome=async{
        let mut conn=authority.connect(false).await?;
        let adopted=authority.adopted_baseline(&mut conn).await?;
        if init_fresh{
            bootstrap::fresh_install(&mut conn,&authority.paths,&baseline,&sha,&authority.role_prefix).await?;
        }else if let Some(marker)=adopted{
            validate_adoption_marker(&marker,&baseline,&sha)?;require_ledger_state(authority,&mut conn,&baseline).await?;
        }else if bootstrap::database_empty(&mut conn,bootstrap::SCHEMAS).await?{
            if !allow_fresh{return Err(AuthorityError::Blocked("cutover requires existing legacy database; empty databases must use init-fresh".into()))}
            bootstrap::fresh_install(&mut conn,&authority.paths,&baseline,&sha,&authority.role_prefix).await?;
        }else{
            crate::legacy::migrate_and_adopt(authority,&mut conn,&baseline,&sha,allow_adoption).await?;
            if !allow_adoption{return Ok(())}
        }
        let directory=authority.paths.epoch_dir(baseline_id);let path=directory.join("manifest.yml");
        if path.is_file(){let epoch=manifest::load_epoch_manifest(&path)?;for line in authority.apply_epoch(&mut conn,&epoch,&directory).await?{println!("authority: {line}");}}
        conn.close().await?;Ok(())
    }.await;
    let released = authority.release_lock(lock).await;
    outcome.and(released)
}
pub async fn startup_check(authority: &MigrationAuthority, principal: &str) -> Result<()> {
    if !crate::credentials::APPLICATION_PRINCIPALS.contains(&principal) {
        return Err(AuthorityError::Refused(
            "unknown application principal".into(),
        ));
    }
    let mut conn = authority.connect(true).await?;
    match principal {
        "gateway" => {
            // Match the runtime's default and its explicit environment override.
            // The production Gateway repeats this admission from its resolved
            // settings before password/catalog writes or worker startup.
            let local_node_profile = match std::env::var("AI_PLATFORM_LOCAL_NODE_CHANNEL_ENABLED") {
                Ok(value) if matches!(value.as_str(), "true" | "1" | "enabled") => true,
                Ok(value) if matches!(value.as_str(), "false" | "0" | "disabled") => false,
                Err(std::env::VarError::NotPresent) => false,
                _ => {
                    return Err(AuthorityError::Refused(
                        "invalid Local Node channel setting".into(),
                    ));
                }
            };
            let delivery_profile = match std::env::var("GATEWAY_TASK_DELIVERY_MODE") {
                Ok(value) if value == "memory" => {
                    crate::application_guard::ApplicationPrincipal::Gateway
                }
                Ok(value) if value == "durable" => {
                    crate::application_guard::ApplicationPrincipal::GatewayDurable
                }
                Err(std::env::VarError::NotPresent) => {
                    crate::application_guard::ApplicationPrincipal::Gateway
                }
                _ => {
                    return Err(AuthorityError::Refused(
                        "invalid Gateway task delivery mode".into(),
                    ));
                }
            };
            let profile = if local_node_profile {
                crate::application_guard::ApplicationPrincipal::GatewayLocalNode
            } else {
                delivery_profile
            };
            crate::application_guard::require_compatible_schema(&mut conn, profile).await?;
            conn.close().await?;
            println!("startup schema check passed (profile={profile:?})");
            Ok(())
        }
        "knowledge_api" | "knowledge_worker" => {
            let profile = if principal == "knowledge_api" {
                crate::application_guard::ApplicationPrincipal::KnowledgeApi
            } else {
                crate::application_guard::ApplicationPrincipal::KnowledgeWorker
            };
            crate::application_guard::require_compatible_schema(&mut conn, profile).await?;
            conn.close().await?;
            println!("startup schema check passed (profile={profile:?})");
            Ok(())
        }
        // Runtime and capability-worker retain their existing source-specific
        // object/epoch gate; this change only replaces application authority
        // admission for Gateway and Knowledge.
        "runtime" | "capability_worker" => {
            let result = bootstrap::startup_schema_check(&mut conn, 2, 10).await?;
            conn.close().await?;
            if result["ok"] != true {
                return Err(AuthorityError::Refused(format!(
                    "startup schema check failed: {}",
                    result.get("reason").unwrap_or(&result["missing_objects"])
                )));
            }
            println!(
                "startup schema check passed (epoch={})",
                result["epoch"].as_str().unwrap_or("")
            );
            Ok(())
        }
        _ => Err(AuthorityError::Refused(
            "unknown application principal".into(),
        )),
    }
}
pub async fn verify(authority: &MigrationAuthority, baseline_id: &str) -> Result<()> {
    let (baseline, sha) = load_baseline(&authority.paths, baseline_id)?;
    let mut conn = authority.connect(true).await?;
    if let Some(marker) = authority.adopted_baseline(&mut conn).await? {
        validate_adoption_marker(&marker, &baseline, &sha)?;
    }
    require_ledger_state(authority, &mut conn, &baseline).await?;
    let applied = authority.applied_changes(&mut conn, baseline_id).await?;
    if applied.is_empty() {
        bootstrap::verify_baseline_sql_file_with_acl(
            &mut conn,
            &authority.paths.baseline_dir(baseline_id).join("verify.sql"),
            &authority.role_prefix,
            Some(&baseline.fingerprints()),
        )
        .await?;
    } else {
        let epoch = manifest::load_epoch_manifest(
            &authority.paths.epoch_dir(baseline_id).join("manifest.yml"),
        )?;
        for change in epoch.changes {
            if applied.contains_key(&change.sequence) {
                crate::runner::evaluate_conditions(
                    &mut conn,
                    &authority.role_prefix,
                    &change.postconditions,
                    "verify",
                    &change.name,
                )
                .await?;
            }
        }
    }
    conn.close().await?;
    println!("verify: baseline/epoch fingerprints match; database is read-only-verified");
    Ok(())
}
pub async fn fingerprint(authority: &MigrationAuthority, baseline_id: &str) -> Result<()> {
    let (baseline, _) = load_baseline(&authority.paths, baseline_id)?;
    let mut conn = authority.connect(true).await?;
    let mut tx = conn.begin().await?;
    set_role(&mut tx, &format!("{}owner", authority.role_prefix)).await?;
    let values = crate::fingerprint::compute_fingerprints(
        &mut tx,
        &authority.role_prefix,
        &baseline.reference_data,
    )
    .await?;
    tx.rollback().await?;
    conn.close().await?;
    for name in ["structural", "acl", "extensions", "reference_data"] {
        println!("{name}: {}", values[name]);
    }
    Ok(())
}

pub async fn source_kind(authority: &MigrationAuthority) -> Result<()> {
    let mut conn = authority.connect(true).await?;
    let kind = if authority.adopted_baseline(&mut conn).await?.is_some() {
        "adopted"
    } else if bootstrap::database_empty(&mut conn, bootstrap::SCHEMAS).await? {
        "empty"
    } else {
        if !base_schema_present(&mut conn).await? {
            return Err(AuthorityError::Blocked("database is nonempty and lacks required platform objects; source cannot be classified safely".into()));
        }
        let columns: Vec<String> = sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='schema_migrations'").fetch_all(&mut conn).await?;
        let filename = columns.iter().any(|c| c == "filename");
        let numeric = columns.iter().any(|c| c == "version");
        if filename {
            sqlx::query("SELECT filename FROM public.schema_migrations")
                .fetch_all(&mut conn)
                .await?;
        }
        if numeric {
            let filter = if columns.iter().any(|c| c == "dirty") {
                " WHERE dirty=FALSE"
            } else {
                ""
            };
            let versions: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT version::text FROM public.schema_migrations{filter}"
            ))
            .fetch_all(&mut conn)
            .await?;
            for version in versions {
                version.parse::<i64>().map_err(|_| {
                    AuthorityError::Refused("invalid numeric legacy version".into())
                })?;
            }
        }
        let per_service = crate::runner::table_exists(&mut conn, "schema_migrations_meta").await?;
        if per_service {
            sqlx::query("SELECT name FROM public.schema_migrations_meta")
                .fetch_all(&mut conn)
                .await?;
        }
        if filename || numeric || per_service {
            "tracked-legacy"
        } else {
            "ledgerless-platform"
        }
    };
    conn.close().await?;
    println!("{kind}");
    Ok(())
}
pub async fn base_schema_present(conn: &mut PgConnection) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT COALESCE(to_regclass('gateway.services'),to_regclass('public.services')) IS NOT NULL AND COALESCE(to_regclass('knowledge.datasets'),to_regclass('public.datasets')) IS NOT NULL AND COALESCE(to_regclass('knowledge.documents'),to_regclass('public.documents')) IS NOT NULL AND COALESCE(to_regclass('knowledge.segments'),to_regclass('public.segments')) IS NOT NULL").fetch_one(conn).await?)
}
pub async fn status(authority: &MigrationAuthority) -> Result<()> {
    let mut conn = authority.connect(true).await?;
    if let Some(marker) = authority.adopted_baseline(&mut conn).await? {
        let id = marker["baseline_id"]
            .as_str()
            .ok_or_else(|| AuthorityError::Refused("malformed adoption marker".into()))?;
        let (baseline, sha) = load_baseline(&authority.paths, id)?;
        validate_adoption_marker(&marker, &baseline, &sha)?;
        require_ledger_state(authority, &mut conn, &baseline).await?;
        println!(
            "baseline: {id} (adopted {})",
            marker["adopted_at"]
                .as_str()
                .unwrap_or("None")
                .replace('T', " ")
        );
        let applied = authority.applied_changes(&mut conn, id).await?;
        if applied.is_empty() {
            println!("  no post-baseline epoch changes applied");
        }
        for (sequence, checksum) in applied {
            println!("  epoch change {sequence}: {checksum}");
        }
        if crate::runner::table_exists(&mut conn, crate::ledger::ATTEMPTS_TABLE).await? {
            let rows=sqlx::query("SELECT attempt_id,sequence,state,checkpoint FROM public.platform_schema_change_attempts ORDER BY started_at DESC LIMIT 5").fetch_all(&mut conn).await?;
            for r in rows {
                println!(
                    "  attempt {}: seq={} state={} checkpoint={}",
                    r.try_get::<String, _>("attempt_id")?,
                    r.try_get::<i32, _>("sequence")?,
                    r.try_get::<String, _>("state")?,
                    r.try_get::<String, _>("checkpoint")?
                );
            }
        }
    } else {
        println!("baseline: none adopted");
    }
    crate::legacy::print_status(&mut conn, &authority.paths).await?;
    conn.close().await?;
    Ok(())
}
pub async fn bootstrap_ledger_acl(
    authority: &MigrationAuthority,
    baseline_id: &str,
) -> Result<Value> {
    let (baseline, sha) = load_baseline(&authority.paths, baseline_id)?;
    let mut conn = authority.connect(false).await?;
    authority.acquire_lock(&mut conn).await?;
    let outcome=async{
        let admin:bool=sqlx::query_scalar("SELECT current_setting('is_superuser',true)='on'").fetch_one(&mut conn).await?;
        if !admin{return Err(AuthorityError::Refused("ledger ACL bootstrap requires explicit admin connection".into()))}
        let mut tx=conn.begin().await?;
        let marker=authority.adopted_baseline(&mut tx).await?.ok_or_else(||AuthorityError::Refused("ledger ACL repair requires verified adopted baseline; empty databases use init-fresh".into()))?;
        validate_adoption_marker(&marker,&baseline,&sha)?;
        let applied=authority.applied_changes(&mut tx,baseline_id).await?;let expected=expected_epoch_fingerprints(&authority.paths,&baseline,&applied)?;
        let active=crate::ledger_acl::inspect(&mut tx,&authority.role_prefix).await?;
        let before_expected=if active{crate::ledger_acl::compatible_expected(&mut tx,&expected,&authority.role_prefix).await?}else{expected.clone()};
        let before=crate::fingerprint::compute_fingerprints(&mut tx,&authority.role_prefix,&baseline.reference_data).await?;
        if before!=before_expected{return Err(AuthorityError::Refused("ledger ACL bootstrap refuses pre-existing fingerprint drift".into()))}
        crate::ledger_acl::install(&mut tx,&authority.role_prefix).await?;
        let after=crate::fingerprint::compute_fingerprints(&mut tx,&authority.role_prefix,&baseline.reference_data).await?;
        let derived=crate::ledger_acl::compatible_expected(&mut tx,&expected,&authority.role_prefix).await?;
        if after!=derived{return Err(AuthorityError::Refused("ledger ACL bootstrap postcondition fingerprint mismatch".into()))}
        let mut result=crate::ledger_acl::receipt(&mut tx,&expected,&authority.role_prefix).await?;
        result["baseline_id"]=json!(baseline_id);
        result["baseline_manifest_sha256"]=json!(sha);
        result["epoch_sequence"]=json!(applied.keys().next_back().copied().unwrap_or(0));
        result["fingerprints"]=json!(after);
        tx.commit().await?;Ok(result)
    }.await;
    let release = authority.release_lock(conn).await;
    match outcome {
        Ok(value) => {
            release?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_paths() -> AuthorityPaths {
        AuthorityPaths {
            database_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../database"),
        }
    }
    #[test]
    fn frozen_baseline_and_every_epoch_verification_bind_current_files() {
        let paths = fixture_paths();
        // The shipped fixture binds packaged bytes independently of the
        // checkout's unrelated Git history; development provenance is tested
        // separately and must not be disabled through a process-wide env edit.
        let (baseline, _) = load_baseline_with_mode(
            &paths,
            DEFAULT_BASELINE_ID,
            crate::distribution::SourceMode::Distribution,
        )
        .unwrap();
        let epoch = manifest::load_epoch_manifest(
            &paths.epoch_dir(DEFAULT_BASELINE_ID).join("manifest.yml"),
        )
        .unwrap();
        let mut applied = BTreeMap::new();
        assert_eq!(
            expected_epoch_fingerprints(&paths, &baseline, &applied).unwrap(),
            baseline.fingerprints()
        );
        for change in &epoch.changes {
            applied.insert(change.sequence, change.sha256.clone());
            let result = expected_epoch_fingerprints(&paths, &baseline, &applied).unwrap();
            assert_eq!(result.len(), 4);
        }
        applied.remove(&1);
        assert!(expected_epoch_fingerprints(&paths, &baseline, &applied).is_err());
    }
    #[test]
    fn pending_baseline_is_legacy_only_until_complete_freeze() {
        let temp = std::env::temp_dir().join(format!("authority-ready-{}", uuid::Uuid::new_v4()));
        let paths = AuthorityPaths {
            database_dir: temp.clone(),
        };
        assert!(!baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
        let baseline = paths.baseline_dir(DEFAULT_BASELINE_ID);
        std::fs::create_dir_all(&baseline).unwrap();
        for name in [
            "init.sql",
            "reference_data.sql",
            "grants.sql",
            "verify.sql",
            "cutover_convergence.sql",
        ] {
            std::fs::write(baseline.join(name), "").unwrap();
        }
        let manifest = json!({"schema":manifest::BASELINE_SCHEMA,"baseline_id":DEFAULT_BASELINE_ID,"state":"pending-live-freeze"});
        std::fs::write(baseline.join("manifest.json"), manifest.to_string()).unwrap();
        assert!(!baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
        let mut frozen = manifest;
        frozen["state"] = json!("frozen");
        std::fs::write(baseline.join("manifest.json"), frozen.to_string()).unwrap();
        assert!(baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
        std::fs::remove_file(baseline.join("init.sql")).unwrap();
        assert!(!baseline_ready(&paths, DEFAULT_BASELINE_ID).unwrap());
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn unknown_or_tampered_epoch_history_fails_before_execution() {
        let paths = fixture_paths();
        let epoch = manifest::load_epoch_manifest(
            &paths.epoch_dir(DEFAULT_BASELINE_ID).join("manifest.yml"),
        )
        .unwrap();
        assert!(validate_applied_prefix(&epoch, &BTreeMap::from([(1, "0".repeat(64))])).is_err());
        assert!(validate_applied_prefix(&epoch, &BTreeMap::from([(999, "0".repeat(64))])).is_err());
        assert!(load_baseline(&paths, "../../outside").is_err());
    }
}
