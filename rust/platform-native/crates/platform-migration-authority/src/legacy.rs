//! Existing immutable pre-baseline execution and adoption contracts.
use crate::{
    AuthorityError, Result, bootstrap, commands, ledger,
    manifest::{
        self, BaselineManifest, LegacyChangeSpec, LegacyManifest, RollbackClass, TransactionMode,
    },
    runner::{AuthorityPaths, MigrationAuthority, table_exists},
    sql::strip_sql_bodies,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Connection, PgConnection};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Instant,
};
#[path = "legacy_sql.rs"]
mod queries;
fn refuse(s: impl Into<String>) -> AuthorityError {
    AuthorityError::Refused(s.into())
}
fn block(s: impl Into<String>) -> AuthorityError {
    AuthorityError::Blocked(s.into())
}
fn alias(name: &str) -> Option<String> {
    if [
        "089_agent_runtime_thread_store.sql",
        "090_agent_runtime_model_leases.sql",
        "092_agent_runtime_legacy_import.sql",
        "093_agent_runtime_assistant_session_fks.sql",
        "094_agent_runtime_legacy_import_normalization.sql",
    ]
    .contains(&name)
    {
        Some(name.replace("agent_runtime", "codex_runtime"))
    } else {
        None
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Filename,
    Version,
    None,
}
async fn columns(conn: &mut PgConnection) -> Result<BTreeSet<String>> {
    Ok(sqlx::query_scalar::<_,String>("SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='schema_migrations'").fetch_all(conn).await?.into_iter().collect())
}
async fn mode(conn: &mut PgConnection) -> Result<Mode> {
    let c = columns(conn).await?;
    if c.contains("filename") {
        Ok(Mode::Filename)
    } else if c.contains("version") {
        Ok(Mode::Version)
    } else if c.is_empty() {
        Ok(Mode::None)
    } else {
        Err(refuse("legacy ledger has no recognized identity column"))
    }
}
async fn applied(conn: &mut PgConnection, m: Mode) -> Result<BTreeSet<String>> {
    if m == Mode::None {
        return Ok(BTreeSet::new());
    }
    if m == Mode::Filename {
        return Ok(sqlx::query_scalar::<_, String>(
            "SELECT filename FROM public.schema_migrations",
        )
        .fetch_all(conn)
        .await?
        .into_iter()
        .collect());
    }
    let filter = if columns(conn).await?.contains("dirty") {
        " WHERE dirty=FALSE"
    } else {
        ""
    };
    let rows = sqlx::query_scalar::<_, String>(&format!(
        "SELECT version::text FROM public.schema_migrations{filter}"
    ))
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|s| {
            s.parse::<i64>()
                .map(|v| format!("{v:03}"))
                .map_err(|_| refuse("invalid numeric legacy version"))
        })
        .collect()
}
fn is_applied(m: Mode, seen: &BTreeSet<String>, file: &str) -> bool {
    if m == Mode::Version {
        seen.contains(&file[..3])
    } else {
        seen.contains(file) || alias(file).is_some_and(|a| seen.contains(&a))
    }
}
fn sql_path(root: &Path, name: &str) -> PathBuf {
    let flat = root.join(name);
    if flat.exists() {
        flat
    } else {
        root.join("legacy").join(name)
    }
}
fn validate_chain(m: &LegacyManifest) -> Result<()> {
    let mut versions: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for c in &m.changes {
        versions.entry(&c.file[..3]).or_default().push(&c.file);
    }
    for (v, files) in versions {
        if files.len() > 1 {
            let expected: Vec<&str> = match v {
                "016" => vec![
                    "016_confluence_multi_root_pages.sql",
                    "016_usage_hourly_aggregates.sql",
                ],
                "031" => vec![
                    "031_align_model_prices_20260211.sql",
                    "031_hierarchical_segments.sql",
                ],
                _ => return Err(refuse("new duplicate legacy version")),
            };
            if files != expected {
                return Err(refuse("ambiguous legacy chain"));
            }
        }
    }
    Ok(())
}
pub fn strip_outer_transaction(sql: &str) -> Result<(String, bool)> {
    let masked = strip_sql_bodies(sql)?;
    let mut statements = vec![];
    let mut start = 0;
    for (i, b) in masked.bytes().enumerate() {
        if b == b';' {
            let n = masked[start..i]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_uppercase();
            if !n.is_empty() {
                statements.push((start, i + 1, n));
            }
            start = i + 1;
        }
    }
    let n = masked[start..]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    if !n.is_empty() {
        statements.push((start, sql.len(), n));
    }
    let controls: Vec<_> = statements
        .iter()
        .enumerate()
        .filter(|(_, (_, _, s))| {
            [
                "BEGIN",
                "BEGIN TRANSACTION",
                "START TRANSACTION",
                "COMMIT",
                "END",
                "ROLLBACK",
                "ABORT",
            ]
            .contains(&s.as_str())
        })
        .collect();
    if controls.is_empty() {
        return Ok((sql.into(), false));
    }
    if controls.len() != 2
        || controls[0].0 != 0
        || controls[1].0 != statements.len() - 1
        || !["BEGIN", "BEGIN TRANSACTION", "START TRANSACTION"].contains(&controls[0].1.2.as_str())
        || !["COMMIT", "END"].contains(&controls[1].1.2.as_str())
    {
        return Err(refuse(
            "legacy transaction must be one paired outer BEGIN/COMMIT",
        ));
    }
    Ok((
        sql[statements[0].1..statements.last().unwrap().0].into(),
        true,
    ))
}
async fn record(
    conn: &mut PgConnection,
    m: Mode,
    s: &LegacyChangeSpec,
    duration: i64,
) -> Result<()> {
    if m == Mode::Filename {
        sqlx::query("INSERT INTO public.schema_migrations(filename) VALUES($1) ON CONFLICT(filename) DO NOTHING").bind(&s.file).execute(conn).await?;
        return Ok(());
    }
    let c = columns(conn).await?;
    let version = s.file[..3]
        .parse::<i64>()
        .map_err(|_| refuse("invalid version"))?;
    if c.contains("dirty") {
        sqlx::query("INSERT INTO public.schema_migrations(version,dirty) VALUES($1,FALSE) ON CONFLICT(version) DO UPDATE SET dirty=FALSE").bind(version).execute(conn).await?;
    } else if ["name", "checksum", "execution_time_ms"]
        .iter()
        .all(|k| c.contains(*k))
    {
        sqlx::query("INSERT INTO public.schema_migrations(version,name,checksum,execution_time_ms) VALUES($1,$2,$3,$4) ON CONFLICT(version) DO UPDATE SET applied_at=NOW(),checksum=$3,execution_time_ms=$4").bind(&s.file[..3]).bind(&s.file).bind(s.legacy_checksum()).bind(duration).execute(conn).await?;
    } else {
        sqlx::query("INSERT INTO public.schema_migrations(version) VALUES($1) ON CONFLICT(version) DO NOTHING").bind(version).execute(conn).await?;
    }
    Ok(())
}
async fn run_one(
    conn: &mut PgConnection,
    m: Mode,
    s: &LegacyChangeSpec,
    root: &Path,
) -> Result<()> {
    let path = sql_path(root, &s.file);
    if manifest::file_sha256(&path)? != s.sha256 {
        return Err(refuse(
            "legacy migration changed after manifest verification",
        ));
    }
    let (sql, outer) = strip_outer_transaction(&std::fs::read_to_string(path)?)?;
    let started = Instant::now();
    if s.transaction_mode == TransactionMode::NonTransactional {
        let normalized = strip_sql_bodies(&sql)?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_uppercase();
        if s.file != "049_session_list_performance.sql"
            || outer
            || normalized
                != "CREATE INDEX CONCURRENTLY IF NOT EXISTS IDX_SESSIONS_USER_TENANT_STATUS_UPDATED ON SESSIONS (USER_ID, TENANT_ID, STATUS, UPDATED_AT DESC);"
        {
            return Err(refuse("invalid nontransactional legacy contract"));
        }
        let rows: Vec<(String, bool, bool, String)> =
            sqlx::query_as(queries::NON_TRANSACTIONAL_INDEX_STATE_SQL)
                .fetch_all(&mut *conn)
                .await?;
        if rows.len() > 1 {
            return Err(refuse("duplicate same-named session indexes"));
        }
        if let Some((schema, valid, ready, _)) = rows.first() {
            if !valid || !ready {
                if !["public", "gateway", "assistant", "knowledge"].contains(&schema.as_str()) {
                    return Err(refuse("unexpected index schema"));
                }
                sqlx::raw_sql(&format!("DROP INDEX CONCURRENTLY IF EXISTS \"{schema}\".\"idx_sessions_user_tenant_status_updated\"")).execute(&mut *conn).await?;
            }
        }
        sqlx::raw_sql(&sql).execute(&mut *conn).await?;
        if sqlx::query_scalar::<_, Option<bool>>(queries::NON_TRANSACTIONAL_049_POSTCHECK_SQL)
            .fetch_one(&mut *conn)
            .await?
            != Some(true)
        {
            return Err(refuse("legacy index postcondition failed"));
        }
        record(conn, m, s, started.elapsed().as_millis() as i64).await?
    } else {
        let mut tx = conn.begin().await?;
        sqlx::raw_sql(&sql).execute(&mut *tx).await?;
        record(&mut tx, m, s, started.elapsed().as_millis() as i64).await?;
        tx.commit().await?;
    }
    Ok(())
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PerChange {
    key: String,
    file: String,
    sha256: String,
    transaction_mode: TransactionMode,
    rollback_class: RollbackClass,
    historical_markers: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PerManifest {
    schema: String,
    changes: Vec<PerChange>,
}
fn per_manifest(root: &Path) -> Result<PerManifest> {
    let m: PerManifest = serde_yaml::from_slice(&std::fs::read(root.join("manifest.yml"))?)
        .map_err(|_| refuse("invalid per-service manifest"))?;
    if m.schema != "migration-authority/per-service-manifest/v1" || m.changes.is_empty() {
        return Err(refuse("invalid per-service schema"));
    }
    let re =
        regex::Regex::new(r"^(_global|gateway|assistant|knowledge)/([0-9]{3}_[a-z0-9_]+\.sql)$")
            .unwrap();
    let marker = regex::Regex::new(r"^[a-z][a-z0-9_]{0,127}$").unwrap();
    let mut files = BTreeSet::new();
    let mut markers = BTreeSet::new();
    let mut order = vec![];
    for c in &m.changes {
        let cap = re
            .captures(&c.file)
            .ok_or_else(|| refuse("unsafe per-service file"))?;
        if c.key != format!("{}:{}", &cap[1], &cap[2])
            || !files.insert(c.file.clone())
            || c.transaction_mode != TransactionMode::Transactional
        {
            return Err(refuse("invalid per-service identity"));
        }
        let idx = ["_global", "gateway", "assistant", "knowledge"]
            .iter()
            .position(|s| *s == &cap[1])
            .unwrap();
        order.push((idx, cap[2].to_owned()));
        for h in &c.historical_markers {
            if !marker.is_match(h) || !markers.insert(h) {
                return Err(refuse("invalid per-service historical marker"));
            }
        }
        if manifest::file_sha256(&root.join(&c.file))? != c.sha256 {
            return Err(refuse("per-service checksum drift"));
        }
    }
    let mut sorted = order.clone();
    sorted.sort();
    if sorted != order {
        return Err(refuse("per-service order drift"));
    }
    fn walk(root: &Path, dir: &Path, files: &mut BTreeSet<String>) -> Result<()> {
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                walk(root, &p, files)?;
            } else if p.extension().and_then(|v| v.to_str()) == Some("sql") {
                files.insert(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    let mut actual = BTreeSet::new();
    walk(root, root, &mut actual)?;
    if actual != files {
        return Err(refuse("per-service SQL coverage mismatch"));
    }
    Ok(m)
}
async fn apply_per_service(conn: &mut PgConnection, paths: &AuthorityPaths) -> Result<()> {
    if !table_exists(conn, "schema_migrations_meta").await? {
        return Ok(());
    }
    let root = paths.migrations_root().join("per_service");
    if !root.is_dir() {
        return Ok(());
    }
    let m = per_manifest(&root)?;
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name,notes FROM public.schema_migrations_meta")
            .fetch_all(&mut *conn)
            .await?;
    let applied: BTreeMap<_, _> = rows
        .into_iter()
        .map(|(k, v)| (k, v.unwrap_or_default()))
        .collect();
    let first = &m.changes[0];
    if !applied.contains_key(&first.key)
        && !first
            .historical_markers
            .iter()
            .any(|k| applied.contains_key(k))
    {
        return Err(block("per-service bootstrap receipt absent"));
    }
    for s in m.changes {
        let rollback = serde_json::to_value(s.rollback_class)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let old = format!("file=migrations/per_service/{}", s.file);
        let note = format!("{old};sha256={};rollback={rollback}", s.sha256);
        if let Some(actual) = applied.get(&s.key) {
            if actual != &old && actual != &note {
                return Err(block("per-service receipt mismatch"));
            }
            continue;
        }
        let historical: Vec<_> = s
            .historical_markers
            .iter()
            .filter(|k| applied.contains_key(*k))
            .collect();
        if !historical.is_empty() {
            for h in historical {
                let expected = match h.as_str() {
                    "phase6_schemas_created" => "gateway/assistant/knowledge schemas + search_path",
                    "phase6_tables_moved" => {
                        "tables relocated from public to gateway/assistant/knowledge"
                    }
                    _ => return Err(block("unknown historical marker")),
                };
                if applied[h] != expected {
                    return Err(block("historical marker notes mismatch"));
                }
            }
            continue;
        }
        let (sql, _) = strip_outer_transaction(&std::fs::read_to_string(root.join(&s.file))?)?;
        let mut tx = conn.begin().await?;
        sqlx::raw_sql(&sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO public.schema_migrations_meta(name,notes) VALUES($1,$2) ON CONFLICT(name) DO NOTHING").bind(s.key).bind(note).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}
async fn reconcile(
    a: &MigrationAuthority,
    conn: &mut PgConnection,
    m: &LegacyManifest,
) -> Result<()> {
    match crate::numeric::reconcile_numeric_legacy_history(conn, m).await {
        Ok(v) => {
            a.set_reconciliation_receipt(v);
            Ok(())
        }
        Err(AuthorityError::Blocked(s)) => {
            if let Ok(v) = serde_json::from_str(&s) {
                a.set_reconciliation_receipt(v);
            }
            Err(block(s))
        }
        Err(e) => Err(e),
    }
}
async fn verify_ready(
    a: &MigrationAuthority,
    conn: &mut PgConnection,
    m: &LegacyManifest,
    ledgerless: bool,
) -> Result<()> {
    let mode = mode(conn).await?;
    let meta = table_exists(conn, "schema_migrations_meta").await?;
    if mode == Mode::None && !meta {
        if ledgerless && commands::base_schema_present(conn).await? {
            return Ok(());
        }
        return Err(block("no legacy ledger for cutover"));
    }
    let seen = applied(conn, mode).await?;
    if m.changes.iter().any(|c| !is_applied(mode, &seen, &c.file)) {
        return Err(block("legacy chain incomplete before adoption"));
    }
    if mode == Mode::Version {
        reconcile(a, conn, m).await?;
    }
    Ok(())
}
/// Historical pending-freeze mode: tracked legacy chains only, no adoption.
pub async fn migrate_legacy_only(a: &MigrationAuthority, conn: &mut PgConnection) -> Result<()> {
    if !sqlx::query_scalar::<_, bool>(queries::BASE_SCHEMA_PROBE)
        .fetch_one(&mut *conn)
        .await?
    {
        return Err(block(
            "existing legacy base objects incomplete; schema snapshot replay refused",
        ));
    }
    let root = a.paths.migrations_root();
    let manifest = manifest::load_legacy_manifest(&root.join("legacy-manifest.yml"))?;
    validate_chain(&manifest)?;
    let mut tracking = mode(conn).await?;
    if tracking == Mode::None && !table_exists(conn, "schema_migrations_meta").await? {
        return Err(block(
            "existing platform schema has no legacy ledger and no frozen baseline is available for fingerprint reconciliation; refusing to replay schema.sql or guess historical migrations",
        ));
    }
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS knowledge; SET search_path TO knowledge,gateway,assistant,public").execute(&mut *conn).await?;
    if tracking == Mode::None {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS public.schema_migrations(filename TEXT PRIMARY KEY,applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())").execute(&mut *conn).await?;
        tracking = Mode::Filename;
    }
    if tracking == Mode::Version {
        reconcile(a, conn, &manifest).await?;
    }
    let seen = applied(conn, tracking).await?;
    for spec in &manifest.changes {
        if !is_applied(tracking, &seen, &spec.file) {
            run_one(conn, tracking, spec, &root).await?;
        }
    }
    apply_per_service(conn, &a.paths).await
}
pub async fn migrate_and_adopt(
    a: &MigrationAuthority,
    conn: &mut PgConnection,
    b: &BaselineManifest,
    sha: &str,
    allow_adoption: bool,
) -> Result<()> {
    if !sqlx::query_scalar::<_, bool>(queries::BASE_SCHEMA_PROBE)
        .fetch_one(&mut *conn)
        .await?
    {
        return Err(block(
            "existing legacy base objects incomplete; schema snapshot replay refused",
        ));
    }
    let root = a.paths.migrations_root();
    let m = manifest::load_legacy_manifest(&root.join("legacy-manifest.yml"))?;
    validate_chain(&m)?;
    let mut mode = mode(conn).await?;
    let ledgerless = mode == Mode::None && !table_exists(conn, "schema_migrations_meta").await?;
    if ledgerless {
        if !allow_adoption {
            return Err(block("ledgerless reconciliation must finish adoption"));
        }
    } else {
        sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS knowledge; SET search_path TO knowledge,gateway,assistant,public").execute(&mut *conn).await?;
        if mode == Mode::None {
            sqlx::raw_sql("CREATE TABLE IF NOT EXISTS public.schema_migrations(filename TEXT PRIMARY KEY,applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())").execute(&mut *conn).await?;
            mode = Mode::Filename;
        }
        if mode == Mode::Version {
            reconcile(a, conn, &m).await?;
        }
        let seen = applied(conn, mode).await?;
        for spec in &m.changes {
            if !is_applied(mode, &seen, &spec.file) {
                run_one(conn, mode, spec, &root).await?;
            }
        }
        apply_per_service(conn, &a.paths).await?;
    }
    if !allow_adoption {
        return Ok(());
    }
    verify_ready(a, conn, &m, ledgerless).await?;
    bootstrap::bootstrap_roles(conn, &a.role_prefix).await?;
    bootstrap::bootstrap_extensions(conn, &a.paths, &a.role_prefix).await?;
    let dir = a.paths.baseline_dir(&b.baseline_id);
    let owner = format!("{}owner", a.role_prefix);
    let mut tx = conn.begin().await?;
    for file in ["cutover_convergence.sql", "grants.sql"] {
        bootstrap::run_baseline_sql_file(
            &mut tx,
            &dir.join(file),
            Some(&a.role_prefix),
            Some(&owner),
        )
        .await?;
    }
    bootstrap::verify_baseline_sql_file(&mut tx, &dir.join("verify.sql"), &a.role_prefix).await?;
    sqlx::raw_sql(ledger::LEDGER_DDL).execute(&mut *tx).await?;
    let actual =
        crate::fingerprint::compute_fingerprints(&mut tx, &a.role_prefix, &b.reference_data)
            .await?;
    if actual != b.fingerprints() {
        return Err(refuse("baseline fingerprint drift; adoption rolled back"));
    }
    bootstrap::insert_baseline_marker(&mut tx, b, sha).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn print_status(conn: &mut PgConnection, paths: &AuthorityPaths) -> Result<()> {
    let columns = columns(conn).await?;
    let filenames = if columns.contains("filename") {
        applied(conn, Mode::Filename).await?
    } else {
        BTreeSet::new()
    };
    let versions = if columns.contains("version") {
        applied(conn, Mode::Version).await?
    } else {
        BTreeSet::new()
    };
    if columns.contains("filename") {
        println!("legacy filename ledger rows: {}", filenames.len());
    }
    if columns.contains("version") {
        println!("legacy numeric ledger rows: {}", versions.len());
    }
    if table_exists(conn, "schema_migrations_meta").await? {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT name FROM public.schema_migrations_meta")
                .fetch_all(&mut *conn)
                .await?;
        println!(
            "per-service ledger rows: {}",
            rows.into_iter().collect::<BTreeSet<_>>().len()
        );
    }
    let manifest =
        manifest::load_legacy_manifest(&paths.migrations_root().join("legacy-manifest.yml"))?;
    let pending = manifest
        .changes
        .iter()
        .filter(|spec| {
            if columns.contains("filename") {
                !is_applied(Mode::Filename, &filenames, &spec.file)
            } else if columns.contains("version") {
                !is_applied(Mode::Version, &versions, &spec.file)
            } else {
                true
            }
        })
        .count();
    println!("legacy chain pending files: {pending}");
    Ok(())
}
pub async fn prepare_cutover_ownership(
    a: &MigrationAuthority,
    id: &str,
    expected: &str,
) -> Result<()> {
    if expected.is_empty() {
        return Err(block("expected database required"));
    }
    let (b, _) = commands::load_baseline(&a.paths, id)?;
    let lock = a.connect(false).await?;
    let mut lock = lock;
    a.acquire_lock(&mut lock).await?;
    let result = async {
        let mut conn = a.connect(false).await?;
        let actual: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&mut conn)
            .await?;
        if actual != expected {
            return Err(block("admin DSN does not match expected database"));
        }
        if bootstrap::database_empty(&mut conn, &["gateway", "assistant", "knowledge", "public"])
            .await?
            || !commands::base_schema_present(&mut conn).await?
        {
            return Err(block("admin cutover requires existing platform database"));
        }
        let m =
            manifest::load_legacy_manifest(&a.paths.migrations_root().join("legacy-manifest.yml"))?;
        verify_ready(a, &mut conn, &m, true).await?;
        bootstrap::provision_roles_admin(&mut conn, &a.paths, &a.role_prefix).await?;
        bootstrap::provision_extensions_admin(&mut conn, &a.paths).await?;
        bootstrap::run_baseline_sql_file(
            &mut conn,
            &a.paths
                .bootstrap_dir()
                .join("legacy_duplicate_reconciliation.sql"),
            Some(&a.role_prefix),
            None,
        )
        .await?;
        bootstrap::run_baseline_sql_file(
            &mut conn,
            &a.paths
                .baseline_dir(&b.baseline_id)
                .join("cutover_convergence.sql"),
            Some(&a.role_prefix),
            None,
        )
        .await?;
        bootstrap::run_baseline_sql_file(
            &mut conn,
            &a.paths
                .bootstrap_dir()
                .join("legacy_structural_reconciliation.sql"),
            None,
            Some(&format!("{}owner", a.role_prefix)),
        )
        .await?;
        conn.close().await?;
        Ok(())
    }
    .await;
    let unlocked = a.release_lock(lock).await;
    result.and(unlocked)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outer_transactions() {
        assert_eq!(
            strip_outer_transaction("BEGIN; SELECT 1; COMMIT;").unwrap(),
            (" SELECT 1;".into(), true)
        );
        assert!(strip_outer_transaction("SELECT 1; BEGIN;COMMIT;").is_err());
        assert!(
            !strip_outer_transaction("DO $$ BEGIN NULL; END $$;")
                .unwrap()
                .1
        );
    }
    #[test]
    fn source_manifest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../database/migrations/per_service");
        per_manifest(&root).unwrap();
    }
}
