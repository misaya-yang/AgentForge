//! Native PostgreSQL source contracts for KB full chain, migration101 restore,
//! and cross-entrypoint authority serialization/crash rollback. All live tests
//! require explicit opt-in, create unique databases, and never load .env.
use ai_platform_migration_authority::{
    DEFAULT_BASELINE_ID, DEFAULT_ROLE_PREFIX, LOCK_ID, LOCK_NAMESPACE, commands, manifest,
    runner::{AuthorityPaths, MigrationAuthority},
};
use sqlx::{Connection, PgConnection, Row};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
// Return contract failures so the unique-database fixture cleanup still runs.
macro_rules! assert_eq {
    ($left:expr,$right:expr $(,$message:expr)? $(,)?) => {
        if $left != $right {
            return Err(concat!(
                "contract assertion failed: ",
                stringify!($left),
                " == ",
                stringify!($right)
            )
            .into());
        }
    };
}
macro_rules! assert {
    ($condition:expr $(,$message:expr)? $(,)?) => {
        if !$condition {
            return Err(concat!("contract assertion failed: ", stringify!($condition)).into());
        }
    };
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_owned()
}
fn paths() -> AuthorityPaths {
    AuthorityPaths {
        database_dir: root().join("database"),
    }
}
fn authority(dsn: &str) -> MigrationAuthority {
    MigrationAuthority::new(dsn.into(), paths(), DEFAULT_ROLE_PREFIX.into()).unwrap()
}
async fn migrate(dsn: &str) -> Result<()> {
    commands::migrate(&authority(dsn), DEFAULT_BASELINE_ID, false, false, false).await?;
    Ok(())
}
struct Database {
    admin: PgConnection,
    dsn: String,
    name: String,
}
impl Database {
    async fn new() -> Result<Self> {
        assert_eq!(
            std::env::var("KB_MIGRATION_TEST_ALLOW_CREATE_DATABASE").as_deref(),
            Ok("1"),
            "explicit disposable PostgreSQL opt-in required"
        );
        let dsn = std::env::var("KB_MIGRATION_TEST_ADMIN_DSN")
            .map_err(|_| "KB_MIGRATION_TEST_ADMIN_DSN required")?;
        let mut admin = PgConnection::connect(&dsn)
            .await
            .map_err(|_| "fixture PostgreSQL connection failed")?;
        let name = format!("kb_native_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(&format!("CREATE DATABASE \"{name}\""))
            .execute(&mut admin)
            .await?;
        let mut url = url::Url::parse(&dsn).map_err(|_| "fixture URL invalid")?;
        url.set_path(&format!("/{name}"));
        Ok(Self {
            admin,
            dsn: url.to_string(),
            name,
        })
    }
    async fn connection(&self) -> Result<PgConnection> {
        Ok(PgConnection::connect(&self.dsn)
            .await
            .map_err(|_| "fixture database unavailable")?)
    }
    async fn close(mut self) -> Result<()> {
        assert!(self.name.starts_with("kb_native_test_"));
        sqlx::raw_sql(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&mut self.admin)
            .await?;
        self.admin.close().await?;
        Ok(())
    }
    async fn seed(&self, layout: &str, before: i32) -> Result<()> {
        let mut conn = self.connection().await?;
        sqlx::raw_sql(&std::fs::read_to_string(
            root().join("database/schema.sql"),
        )?)
        .execute(&mut conn)
        .await?;
        if layout == "split" {
            sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS knowledge")
                .execute(&mut conn)
                .await?;
            for table in [
                "dataset_queries",
                "dataset_process_rules",
                "segments",
                "documents",
                "datasets",
            ] {
                sqlx::raw_sql(&format!(
                    "ALTER TABLE public.\"{table}\" SET SCHEMA knowledge"
                ))
                .execute(&mut conn)
                .await?;
            }
        }
        sqlx::raw_sql("CREATE TABLE public.schema_migrations(filename TEXT PRIMARY KEY,applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW())").execute(&mut conn).await?;
        for spec in
            manifest::load_legacy_manifest(&paths().migrations_root().join("legacy-manifest.yml"))?
                .changes
        {
            if spec.file.split('_').next().unwrap().parse::<i32>()? < before {
                sqlx::query("INSERT INTO public.schema_migrations(filename) VALUES($1)")
                    .bind(spec.file)
                    .execute(&mut conn)
                    .await?;
            }
        }
        conn.close().await?;
        Ok(())
    }
}
const TABLES: &[&str] = &[
    "document_pipeline_executions",
    "dataset_collection_bindings",
    "embedding_migrations",
    "embedding_migration_progress",
    "embedding_vector_cache",
    "kb_eval_golden",
    "kb_eval_golden_release",
    "kb_bm25_v2_lifecycle",
    "kb_segment_attachment_bindings",
    "kb_parsing_ir",
    "kb_parsing_page_cache",
    "dataset_query_feedback",
    "kb_document_batch_operations",
    "kb_document_batch_items",
    "embedding_migration_action_jobs",
    "kb_document_progress_events",
];
#[tokio::test]
#[ignore = "Requires explicit disposable PostgreSQL database-create opt-in; no ordinary CI database access"]
async fn full_chain_100_to_112_keeps_single_idempotent_ledger_and_schema_ownership() -> Result<()> {
    for layout in ["public", "main", "split"] {
        let db = Database::new().await?;
        let outcome:Result<()>=async{db.seed(layout,100).await?;let mut c=db.connection().await?;for table in ["dataset_query_feedback","kb_document_batch_operations","embedding_migration_action_jobs"]{assert!(!sqlx::query_scalar::<_,bool>("SELECT to_regclass($1) IS NOT NULL").bind(format!("knowledge.{table}")).fetch_one(&mut c).await?);}assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_name='dataset_queries' AND column_name='trace_id')").fetch_one(&mut c).await?);c.close().await?;
 migrate(&db.dsn).await?;migrate(&db.dsn).await?;let mut c=db.connection().await?;let mut columns=sqlx::query_scalar::<_,String>("SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='schema_migrations'").fetch_all(&mut c).await?;columns.sort();assert_eq!(columns,["applied_at","filename"]);
 let recorded=sqlx::query_scalar::<_,String>("SELECT filename FROM public.schema_migrations").fetch_all(&mut c).await?;assert!(recorded.iter().all(|n|!n.ends_with("_rollback.sql")));for spec in manifest::load_legacy_manifest(&paths().migrations_root().join("legacy-manifest.yml"))?.changes{let n=spec.file.split('_').next().unwrap().parse::<i32>()?;if(100..=112).contains(&n){assert!(recorded.contains(&spec.file));}}
 let metadata_owner:String=sqlx::query_scalar("SELECT table_schema FROM information_schema.columns WHERE table_name='dataset_queries' AND column_name='metadata'").fetch_one(&mut c).await?;assert_eq!(metadata_owner,if layout=="split"{"knowledge"}else{"public"});
 for table in TABLES{let row=sqlx::query("SELECT n.nspname AS namespace,pg_get_userbyid(c.relowner)=current_user AS owned FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.oid=to_regclass($1)").bind(format!("knowledge.{table}")).fetch_one(&mut c).await?;assert_eq!(row.get::<String,_>("namespace"),"knowledge");assert!(row.get::<bool,_>("owned"));}
 let duplicates:i64=sqlx::query_scalar("SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind='r' AND c.relname=ANY($1::text[]) AND n.nspname<>'knowledge'").bind(&TABLES[8..]).fetch_one(&mut c).await?;assert_eq!(duplicates,0);
 let invalid:i64=sqlx::query_scalar("SELECT count(*) FROM pg_constraint c JOIN pg_namespace n ON n.oid=c.connamespace WHERE n.nspname IN ('knowledge','public') AND c.conname LIKE 'fk_kb_%' AND NOT c.convalidated").fetch_one(&mut c).await?;assert_eq!(invalid,0);c.close().await?;Ok(())}.await;
        db.close().await?;
        outcome?;
    }
    Ok(())
}
#[tokio::test]
#[ignore = "Requires explicit disposable PostgreSQL database-create opt-in"]
async fn fresh_upgrade_from_97_and_progress_retention_are_observable() -> Result<()> {
    let db = Database::new().await?;
    let outcome:Result<()>=async{db.seed("public",97).await?;migrate(&db.dsn).await?;migrate(&db.dsn).await?;let mut c=db.connection().await?;assert!(sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM public.schema_migrations WHERE filename='097_image_task_runtime_scope.sql')").fetch_one(&mut c).await?);assert!(!sqlx::query_scalar::<_,bool>("SELECT to_regclass('assistant.image_tasks') IS NOT NULL").fetch_one(&mut c).await?);
 sqlx::raw_sql("ALTER TABLE knowledge.kb_document_progress_events DISABLE TRIGGER trg_kb_document_progress_retention; INSERT INTO knowledge.kb_document_progress_events(dataset_id,document_id,event_type,payload,created_at) SELECT 'retention-dataset','document-'||value::text,'progress','{}'::jsonb,CASE WHEN value=1 THEN NOW()-INTERVAL '8 days' ELSE NOW() END FROM generate_series(1,10005) AS value; ALTER TABLE knowledge.kb_document_progress_events ENABLE TRIGGER trg_kb_document_progress_retention").execute(&mut c).await?;
 let maximum:i64=sqlx::query_scalar("SELECT MAX(event_sequence) FROM knowledge.kb_document_progress_events").fetch_one(&mut c).await?;let sequence:String=sqlx::query_scalar("SELECT pg_get_serial_sequence('knowledge.kb_document_progress_events','event_sequence')").fetch_one(&mut c).await?;sqlx::query("SELECT setval($1::regclass,$2,TRUE)").bind(sequence).bind(((maximum/128)+1)*128-1).execute(&mut c).await?;sqlx::raw_sql("INSERT INTO knowledge.kb_document_progress_events(dataset_id,document_id,event_type,payload) VALUES('retention-dataset','cleanup-trigger','progress','{}'::jsonb)").execute(&mut c).await?;assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM knowledge.kb_document_progress_events WHERE dataset_id='retention-dataset'").fetch_one(&mut c).await?,10000);assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM knowledge.kb_document_progress_events WHERE created_at<NOW()-INTERVAL '7 days'").fetch_one(&mut c).await?,0);c.close().await?;Ok(())}.await;
    db.close().await?;
    outcome
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start_cli(dsn: &str) -> Result<Process> {
    Ok(Process(
        Command::new(env!("CARGO_BIN_EXE_database-authority"))
            .args(["migrate", "--no-adoption"])
            .current_dir(root())
            .env_clear()
            .env("AI_GATEWAY_DATABASE_MIGRATOR_DSN", dsn)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ))
}
async fn wait_process(process: &mut Process, success: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = process.0.try_wait()? {
            assert_eq!(
                status.success(),
                success,
                "native authority subprocess status"
            );
            return Ok(());
        }
        assert!(
            Instant::now() < deadline,
            "native authority process timeout"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn locks(c: &mut PgConnection, granted: bool, expected: i64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE l.locktype='advisory' AND l.classid::bigint=$1 AND l.objid::bigint=$2 AND l.objsubid=2 AND l.granted=$3 AND a.datname=current_database() AND a.application_name LIKE 'ai_gateway_authority_%'").bind(i64::from(LOCK_NAMESPACE)).bind(i64::from(LOCK_ID)).bind(granted).fetch_one(&mut *c).await?;
        if count == expected {
            return Ok(());
        }
        assert!(Instant::now() < deadline, "authority lock count mismatch");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn rewind(db: &Database) -> Result<String> {
    db.seed("public", 100).await?;
    migrate(&db.dsn).await?;
    let manifest =
        manifest::load_legacy_manifest(&paths().migrations_root().join("legacy-manifest.yml"))?;
    let freeze = manifest.freeze_point;
    let mut c = db.connection().await?;
    assert_eq!(
        sqlx::query("DELETE FROM public.schema_migrations WHERE filename=$1")
            .bind(&freeze)
            .execute(&mut c)
            .await?
            .rows_affected(),
        1
    );
    c.close().await?;
    Ok(freeze)
}
async fn final_state(c: &mut PgConnection, freeze: &str, applied: bool) -> Result<()> {
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM public.schema_migrations WHERE filename=$1"
        )
        .bind(freeze)
        .fetch_one(&mut *c)
        .await?,
        i64::from(applied)
    );
    for sql in [
        "SELECT to_regclass('knowledge.idx_kb_document_progress_events_dataset_created') IS NOT NULL",
        "SELECT to_regprocedure('knowledge.prune_kb_document_progress_events()') IS NOT NULL",
        "SELECT EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid='knowledge.kb_document_progress_events'::regclass AND tgname='trg_kb_document_progress_retention' AND NOT tgisinternal)",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, bool>(sql)
                .fetch_one(&mut *c)
                .await?,
            applied
        );
    }
    Ok(())
}
#[tokio::test]
#[ignore = "Requires explicit disposable PostgreSQL database-create opt-in and native authority subprocess"]
async fn cli_and_library_serialize_one_legacy_change() -> Result<()> {
    let db = Database::new().await?;
    let outcome: Result<()> = async {
        let freeze = rewind(&db).await?;
        let mut holder = db.connection().await?;
        let mut observer = db.connection().await?;
        sqlx::query("SELECT pg_advisory_lock($1::integer,$2::integer)")
            .bind(LOCK_NAMESPACE)
            .bind(LOCK_ID)
            .execute(&mut holder)
            .await?;
        let mut process = start_cli(&db.dsn)?;
        let coordination = async {
            locks(&mut observer, false, 2).await?;
            sqlx::query("SELECT pg_advisory_unlock($1::integer,$2::integer)")
                .bind(LOCK_NAMESPACE)
                .bind(LOCK_ID)
                .execute(&mut holder)
                .await?;
            wait_process(&mut process, true).await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        };
        // Poll both entrypoints concurrently on this task, preserving borrowed
        // SQLx executor futures without imposing an unrelated Send requirement.
        tokio::try_join!(migrate(&db.dsn), coordination)?;
        locks(&mut observer, true, 0).await?;
        final_state(&mut observer, &freeze, true).await?;
        holder.close().await?;
        observer.close().await?;
        Ok(())
    }
    .await;
    db.close().await?;
    outcome
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires explicit disposable PostgreSQL database-create opt-in; sends TERM/KILL only to spawned authority child"]
async fn authority_crash_releases_lock_without_ghost_change() -> Result<()> {
    for signal in ["-TERM", "-KILL"] {
        let db = Database::new().await?;
        let outcome:Result<()>=async{let freeze=rewind(&db).await?;let mut observer=db.connection().await?;sqlx::raw_sql("DROP TRIGGER IF EXISTS trg_kb_document_progress_retention ON knowledge.kb_document_progress_events; DROP FUNCTION IF EXISTS knowledge.prune_kb_document_progress_events(); DROP INDEX IF EXISTS knowledge.idx_kb_document_progress_events_dataset_created").execute(&mut observer).await?;final_state(&mut observer,&freeze,false).await?;let mut blocker=db.connection().await?;sqlx::raw_sql("BEGIN; LOCK TABLE knowledge.kb_document_progress_events IN ACCESS EXCLUSIVE MODE").execute(&mut blocker).await?;let mut process=start_cli(&db.dsn)?;locks(&mut observer,true,1).await?;
 let deadline=Instant::now()+Duration::from_secs(5);loop{if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE l.locktype='relation' AND l.relation='knowledge.kb_document_progress_events'::regclass AND NOT l.granted AND a.datname=current_database() AND a.application_name LIKE 'ai_gateway_authority_%')").fetch_one(&mut observer).await?{break;}assert!(Instant::now()<deadline,"authority never blocked on final change");tokio::time::sleep(Duration::from_millis(25)).await;}
 assert!(Command::new("kill").args([signal,&process.0.id().to_string()]).status()?.success());wait_process(&mut process,false).await?;locks(&mut observer,true,0).await?;final_state(&mut observer,&freeze,false).await?;sqlx::raw_sql("ROLLBACK").execute(&mut blocker).await?;migrate(&db.dsn).await?;final_state(&mut observer,&freeze,true).await?;blocker.close().await?;observer.close().await?;Ok(())}.await;
        db.close().await?;
        outcome?;
    }
    Ok(())
}
fn pg_client(name: &str, dsn: &str, args: &[&std::ffi::OsStr]) -> Result<()> {
    let url = url::Url::parse(dsn).map_err(|_| "fixture URL invalid")?;
    let decode = |s: &str| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8()
            .map(|v| v.into_owned())
            .map_err(|_| "fixture connection component invalid")
    };
    let executable = std::env::var_os(format!("POSTGRES_{}_BIN", name.to_uppercase()))
        .unwrap_or_else(|| name.into());
    let status = Command::new(executable)
        .args(args)
        .env("PGHOST", url.host_str().ok_or("fixture host missing")?)
        .env("PGPORT", url.port().unwrap_or(5432).to_string())
        .env("PGUSER", decode(url.username())?)
        .env("PGPASSWORD", decode(url.password().unwrap_or(""))?)
        .env("PGDATABASE", decode(url.path().trim_start_matches('/'))?)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    assert!(
        status.success(),
        "PostgreSQL client failed; output intentionally redacted"
    );
    Ok(())
}
#[tokio::test]
#[ignore = "Requires disposable PostgreSQL opt-in plus pg_dump and pg_restore; creates only unique test databases"]
async fn migration_101_requires_dump_restore_for_old_writer_contract() -> Result<()> {
    let source = Database::new().await?;
    let restored = Database::new().await?;
    let dump =
        std::env::temp_dir().join(format!("kb_native_{}.dump", uuid::Uuid::new_v4().simple()));
    let outcome:Result<()>=async{let mut c=source.connection().await?;sqlx::raw_sql(include_str!("fixtures/kb_101_n_minus_one_schema.sql")).execute(&mut c).await?;let upsert=include_str!("fixtures/kb_101_n_minus_one_upsert.sql");sqlx::query(upsert).bind("old-writer-before").bind("before").execute(&mut c).await?;assert_eq!(sqlx::query_scalar::<_,String>("SELECT text FROM segments WHERE document_id='document-a'").fetch_one(&mut c).await?,"before");
 pg_client("pg_dump",&source.dsn,&["--format=custom".as_ref(),"--no-owner".as_ref(),"--no-privileges".as_ref(),"--file".as_ref(),dump.as_os_str()])?;assert!(std::fs::metadata(&dump)?.len()>0);
 sqlx::raw_sql(&std::fs::read_to_string(paths().migrations_root().join("101_kb_ingestion_lifecycle.sql"))?).execute(&mut c).await?;let error=sqlx::query(upsert).bind("old-writer-after").bind("must-not-write").execute(&mut c).await.err().ok_or("old writer unexpectedly succeeded after migration 101")?;assert_eq!(error.as_database_error().and_then(|e|e.code()).as_deref(),Some("42P10"));sqlx::raw_sql("INSERT INTO segments(segment_id,dataset_id,document_id,position,text,content_type) VALUES('new-writer-after','dataset-a','document-a',0,'after','text') ON CONFLICT(document_id,content_type,position) DO UPDATE SET text=EXCLUDED.text").execute(&mut c).await?;assert_eq!(sqlx::query_scalar::<_,String>("SELECT text FROM segments WHERE document_id='document-a'").fetch_one(&mut c).await?,"after");c.close().await?;
 pg_client("pg_restore",&restored.dsn,&["--no-owner".as_ref(),"--no-privileges".as_ref(),"--exit-on-error".as_ref(),"--dbname".as_ref(),restored.name.as_ref(),dump.as_os_str()])?;
 let mut c=restored.connection().await?;assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_constraint WHERE conrelid='public.segments'::regclass AND conname='segments_document_id_position_key'").fetch_one(&mut c).await?,1);assert!(!sqlx::query_scalar::<_,bool>("SELECT to_regclass('knowledge.document_pipeline_executions') IS NOT NULL").fetch_one(&mut c).await?);sqlx::query(upsert).bind("old-writer-restored").bind("restored").execute(&mut c).await?;assert_eq!(sqlx::query_scalar::<_,String>("SELECT text FROM segments WHERE document_id='document-a'").fetch_one(&mut c).await?,"restored");c.close().await?;Ok(())}.await;
    let _ = std::fs::remove_file(&dump);
    source.close().await?;
    restored.close().await?;
    outcome
}
