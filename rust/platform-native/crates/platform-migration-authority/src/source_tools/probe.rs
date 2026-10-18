//! Opt-in disposable-database probes. Offline mode never reads DSN variables.
use crate::{
    AuthorityError, DEFAULT_BASELINE_ID as BASELINE, Result, commands, credentials, manifest,
    runner::AuthorityPaths,
    source_tools::{measure_successors, successors},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
};
const SOURCE: &str =
    "rust/platform-native/crates/platform-knowledge-domain/src/migration_knowledge_ingest/sql.rs";
// Pin the extracted production statement; changes need explicit contract review.
const REQUEUE_SQL_SHA256: &str = "076606710cce625d8071ed96c091fa2a08b55b1962fab3c559afda14f4717d97";
const ROLES: &[&str] = &[
    "gateway",
    "knowledge_api",
    "knowledge_worker",
    "runtime",
    "capability_worker",
];
const CASES: &[(&str, bool)] = &[
    ("same_running_generation", true),
    ("wrong_document", false),
    ("wrong_dataset", false),
    ("wrong_execution", false),
    ("missing_execution_marker", false),
    ("replaced_generation", false),
    ("execution_cancelled", false),
    ("execution_failed", false),
    ("execution_completed", false),
    ("null_process_rule", false),
    ("special_document", false),
    ("special_manifest", false),
    ("dataset_deleted", false),
    ("dataset_archived", false),
    ("dataset_deletion_fence", false),
    ("document_disabled", false),
    ("document_archived", false),
    ("lifecycle_deactivating", false),
    ("lifecycle_restore_pending", true),
    ("lifecycle_restore_disabled", false),
    ("document_terminal", false),
    ("document_waiting", false),
];
fn fail() -> AuthorityError {
    AuthorityError::Refused("isolated successor probe assertion failed".into())
}

#[derive(Debug)]
pub struct ProbeError {
    phase: &'static str,
    role: Option<&'static str>,
    error_class: &'static str,
    sqlstate: Option<String>,
}

impl ProbeError {
    fn new(phase: &'static str, role: Option<&'static str>, error: AuthorityError) -> Self {
        let error_class = match &error {
            AuthorityError::Refused(_) => "refused",
            AuthorityError::Blocked(_) => "blocked",
            AuthorityError::Database(_) => "database",
            AuthorityError::Io(_) => "io",
        };
        let sqlstate = match &error {
            AuthorityError::Database(sqlx::Error::Database(database)) => {
                database.code().map(|code| code.into_owned())
            }
            _ => None,
        };
        Self {
            phase,
            role,
            error_class,
            sqlstate,
        }
    }

    pub fn phase(&self) -> &'static str {
        self.phase
    }

    pub fn role(&self) -> Option<&'static str> {
        self.role
    }

    pub fn error_class(&self) -> &'static str {
        self.error_class
    }

    pub fn sqlstate(&self) -> Option<&str> {
        self.sqlstate.as_deref()
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.role {
            Some(role) => write!(
                formatter,
                "successor role probe failed during {} for role {role}",
                self.phase
            ),
            None => write!(
                formatter,
                "successor role probe failed during {}",
                self.phase
            ),
        }
    }
}

impl std::error::Error for ProbeError {}

impl From<AuthorityError> for ProbeError {
    fn from(error: AuthorityError) -> Self {
        Self::new("argument_validation", None, error)
    }
}

fn contextual<T>(
    result: Result<T>,
    phase: &'static str,
    role: Option<&'static str>,
) -> std::result::Result<T, ProbeError> {
    result.map_err(|error| ProbeError::new(phase, role, error))
}

fn require(ok: bool) -> Result<()> {
    if ok { Ok(()) } else { Err(fail()) }
}
fn id() -> String {
    format!("schema_probe_{}", uuid::Uuid::new_v4().simple())
}
pub struct ProbeOptions {
    /// Candidate artifact root containing measured baseline and epoch files.
    pub root: PathBuf,
    /// Source checkout used for frozen-preimage validation and production SQL.
    /// Omission retains the historical single-root behavior.
    pub source_root: Option<PathBuf>,
    /// Verified preserved checkout used only for the retired frozen Python preimage.
    pub preimage_root: Option<PathBuf>,
    pub run: bool,
    pub shadow: bool,
    pub database: String,
    pub prefix: String,
    pub dataset: String,
    pub other_dataset: String,
    pub rule: String,
}
pub fn production_requeue(root: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    require(root.is_dir())?;
    let mut path = root.clone();
    for component in SOURCE.split('/') {
        path.push(component);
        require(!std::fs::symlink_metadata(&path)?.file_type().is_symlink())?;
    }
    let path = path.canonicalize()?;
    require(path.starts_with(&root) && path.is_file())?;
    let source = std::fs::read_to_string(path)?;
    let pattern = regex::Regex::new(
        "(?s)pub const REQUEUE_UNPUBLISHED_DOCUMENT_GENERATION_0: &str = r###\"(.*?)\"###;",
    )
    .unwrap();
    let captures = pattern.captures_iter(&source).collect::<Vec<_>>();
    require(captures.len() == 1)?;
    let sql = captures[0][1].to_owned();
    require(format!("{:x}", Sha256::digest(sql.as_bytes())) == REQUEUE_SQL_SHA256)?;
    Ok(sql)
}
pub fn source_contract(candidate_root: &Path, source_root: &Path) -> Result<Value> {
    source_contract_with_preimage_root(candidate_root, source_root, None)
}

pub fn source_contract_with_preimage_root(
    candidate_root: &Path,
    source_root: &Path,
    preimage_root: Option<&Path>,
) -> Result<Value> {
    let candidate_root = candidate_root.canonicalize()?;
    require(candidate_root.is_dir())?;
    let source_root = source_root.canonicalize()?;
    require(source_root.is_dir())?;
    let epoch_path = candidate_root
        .join("database/migrations")
        .join(BASELINE)
        .join("manifest.yml");
    let epoch = manifest::load_epoch_manifest(&epoch_path)?;
    if source_root == candidate_root {
        require((20..=35).contains(&epoch.epoch))?;
        successors::verify_activated_source(&source_root)?;
    } else if epoch.epoch == 20 {
        let measured = candidate_root.join("SUCCESSOR_MEASUREMENT.json").is_file();
        if measured {
            successors::verify_measured_preview_with_preimage_root(
                &source_root,
                &candidate_root,
                preimage_root,
            )?;
        } else {
            successors::verify_preview_with_preimage_root(
                &source_root,
                &candidate_root,
                preimage_root,
            )?;
        }
    } else if epoch.epoch == 26 {
        let measured = candidate_root
            .join("database/migrations")
            .join(BASELINE)
            .join("SUCCESSOR_026_MEASUREMENT.json")
            .is_file();
        crate::source_tools::next_successor_026::verify_preview(
            &source_root,
            &candidate_root,
            measured,
        )?;
    } else if epoch.epoch == 27 {
        let measured = candidate_root
            .join("database/migrations")
            .join(BASELINE)
            .join("SUCCESSOR_027_MEASUREMENT.json")
            .is_file();
        crate::source_tools::next_successor_027::verify_preview(
            &source_root,
            &candidate_root,
            measured,
        )?;
    } else if (30..=35).contains(&epoch.epoch) {
        let measured = candidate_root
            .join("database/migrations")
            .join(BASELINE)
            .join(format!(
                "SERVICE_SUCCESSOR_{:03}_MEASUREMENT.json",
                epoch.epoch
            ))
            .is_file();
        crate::source_tools::service_successors::verify_preview(
            &source_root,
            &candidate_root,
            epoch.epoch,
            measured,
        )?;
    } else if epoch.epoch == 29 {
        let measured = candidate_root
            .join("database/migrations")
            .join(BASELINE)
            .join("SUCCESSOR_029_MEASUREMENT.json")
            .is_file();
        crate::source_tools::next_successor_029::verify_preview(
            &source_root,
            &candidate_root,
            measured,
        )?;
    } else if epoch.epoch == 28 {
        let measured = candidate_root
            .join("database/migrations")
            .join(BASELINE)
            .join("SUCCESSOR_028_MEASUREMENT.json")
            .is_file();
        crate::source_tools::next_successor_028::verify_preview(
            &source_root,
            &candidate_root,
            measured,
        )?;
    } else {
        return Err(fail());
    }
    let path = candidate_root
        .join("database/baselines")
        .join(BASELINE)
        .join("manifest.json");
    let baseline: Value = serde_json::from_slice(&std::fs::read(&path)?).map_err(|_| fail())?;
    let mut marker = serde_json::Map::new();
    for key in [
        "baseline_id",
        "structural_sha256",
        "acl_sha256",
        "extensions_sha256",
        "reference_data_sha256",
        "source_git_sha",
    ] {
        marker.insert(key.into(), baseline[key].clone());
    }
    marker.insert(
        "manifest_sha256".into(),
        json!(manifest::file_sha256(&path)?),
    );
    Ok(
        json!({"baseline_id":BASELINE,"epoch":epoch.epoch,"baseline":marker,"changes":epoch.changes.iter().map(|c|json!({"baseline_id":BASELINE,"sequence":c.sequence,"name":c.name,"checksum_sha256":c.sha256})).collect::<Vec<_>>(),"production_requeue_sha256":format!("{:x}",Sha256::digest(production_requeue(&source_root)?.as_bytes()))}),
    )
}
pub fn check_destination(o: &ProbeOptions) -> Result<()> {
    require(
        regex::Regex::new("^schema_successor_verify_[a-z0-9_]{8,48}$")
            .unwrap()
            .is_match(&o.database),
    )?;
    credentials::validate_role_prefix(&o.prefix)?;
    let r = regex::Regex::new("^schema_successor_probe_[a-z0-9_]{8,48}$").unwrap();
    require(
        r.is_match(&o.dataset)
            && r.is_match(&o.other_dataset)
            && o.dataset != o.other_dataset
            && !o.rule.is_empty(),
    )
}
async fn query(conn: &mut PgConnection, sql: &str, args: &[&str]) -> Result<u64> {
    let mut q = sqlx::query(sql);
    for arg in args {
        q = q.bind(*arg);
    }
    Ok(q.execute(conn).await?.rows_affected())
}
async fn boolean(conn: &mut PgConnection, sql: &str, args: &[&str]) -> Result<bool> {
    let mut q = sqlx::query_scalar::<_, bool>(sql);
    for arg in args {
        q = q.bind(*arg);
    }
    Ok(q.fetch_one(conn).await?)
}
async fn begin(conn: &mut PgConnection) -> Result<()> {
    sqlx::raw_sql("BEGIN READ WRITE").execute(conn).await?;
    Ok(())
}
async fn begin_shadow(conn: &mut PgConnection) -> Result<()> {
    sqlx::raw_sql("BEGIN READ WRITE; SET LOCAL search_path=pg_temp,pg_catalog")
        .execute(conn)
        .await?;
    Ok(())
}
async fn rollback(conn: &mut PgConnection) -> Result<()> {
    sqlx::raw_sql("ROLLBACK").execute(conn).await?;
    Ok(())
}
async fn denied(conn: &mut PgConnection, sql: &str) -> Result<()> {
    sqlx::raw_sql("SAVEPOINT expected_denial")
        .execute(&mut *conn)
        .await?;
    let result = sqlx::raw_sql(sql).execute(&mut *conn).await;
    let denied =
        matches!(&result,Err(sqlx::Error::Database(e)) if e.code().as_deref()==Some("42501"));
    sqlx::raw_sql("ROLLBACK TO SAVEPOINT expected_denial; RELEASE SAVEPOINT expected_denial")
        .execute(&mut *conn)
        .await?;
    require(denied)
}
async fn connect(o: &ProbeOptions, role: &str) -> Result<PgConnection> {
    let dsn = credentials::dsn_for_principal(role)?;
    measure_successors::validate_loopback_dsn(&dsn, &o.database, &format!("{}{role}", o.prefix))?;
    let mut conn = credentials::connect_principal(role).await?;
    sqlx::raw_sql(
        "SET default_transaction_read_only=on;SET statement_timeout='5000';SET lock_timeout='1000'",
    )
    .execute(&mut conn)
    .await?;
    let row = sqlx::query(
        "SELECT current_database()::text AS database,current_user=session_user AS same,session_user::text AS role,current_setting('search_path')::text AS search_path",
    )
    .fetch_one(&mut conn)
    .await?;
    let expected_search_path = if matches!(role, "knowledge_api" | "knowledge_worker") {
        "pg_catalog, knowledge, gateway, assistant, public"
    } else {
        "pg_catalog, gateway, assistant, knowledge, public"
    };
    require(
        row.try_get::<String, _>("database")? == o.database
            && row.try_get::<bool, _>("same")?
            && row.try_get::<String, _>("role")? == format!("{}{role}", o.prefix)
            && row.try_get::<String, _>("search_path")?.as_str() == expected_search_path,
    )?;
    Ok(conn)
}
async fn matrix(
    conn: &mut PgConnection,
    role: &str,
    o: &ProbeOptions,
    identity: &Value,
) -> Result<Value> {
    require(
        !boolean(
            conn,
            "SELECT pg_has_role(current_user,$1,'MEMBER')",
            &[&format!("{}owner", o.prefix)],
        )
        .await?,
    )?;
    for schema in ["gateway", "knowledge"] {
        require(
            !boolean(
                conn,
                "SELECT has_schema_privilege(current_user,$1,'CREATE')",
                &[schema],
            )
            .await?,
        )?;
    }
    if ["gateway", "knowledge_api", "knowledge_worker"].contains(&role) {
        let value: Value =
            sqlx::query_scalar("SELECT gateway.platform_schema_admission_snapshot_v1()")
                .fetch_one(&mut *conn)
                .await?;
        require(
            value["schema_version"] == "ai-platform/schema-admission-snapshot/v1"
                && value["epoch"] == identity["epoch"]
                && value["changes"] == identity["changes"]
                && value["baselines"] == json!([identity["baseline"]])
                && value["has_attempts"] == false
                && value["has_incomplete_attempts"] == false,
        )?;
        denied(conn, "SELECT * FROM public.platform_schema_changes LIMIT 0").await?;
    } else {
        denied(
            conn,
            "SELECT gateway.platform_schema_admission_snapshot_v1()",
        )
        .await?;
    }
    for privilege in [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
    ] {
        let allowed = role == "gateway" && ["SELECT", "INSERT", "UPDATE"].contains(&privilege);
        // Resolve the relation through catalogs so roles without gateway schema USAGE can
        // still be checked for table ACLs without attempting a schema-qualified name lookup.
        require(
            boolean(
                conn,
                "SELECT COALESCE((SELECT has_table_privilege(current_user, relation.oid, $1) FROM pg_catalog.pg_class AS relation JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace WHERE namespace.nspname = 'gateway' AND relation.relname = 'task_deliveries'), false)",
                &[privilege],
            )
            .await?
                == allowed,
        )?;
    }
    if role != "gateway" {
        denied(conn, "SELECT * FROM gateway.task_deliveries LIMIT 0").await?;
    }
    if ["knowledge_api", "knowledge_worker"].contains(&role) {
        for sql in [
            "UPDATE knowledge.kb_document_progress_events SET payload=payload WHERE false",
            "DELETE FROM knowledge.kb_document_progress_events WHERE false",
            "SELECT knowledge.record_kb_document_progress_event()",
        ] {
            denied(conn, sql).await?;
        }
        require(!boolean(conn,"SELECT has_sequence_privilege(current_user,'knowledge.kb_document_progress_events_event_sequence_seq','USAGE,SELECT,UPDATE')",&[]).await?)?;
    }
    let installed = identity["epoch"].as_i64().ok_or_else(fail)? as i32;
    for sql in crate::source_tools::service_successors::admission_checks(installed, role)? {
        require(boolean(conn, &sql, &[]).await?)?;
    }
    if installed >= 35 {
        for sql in [
            "SELECT * FROM partition_ops.families LIMIT 0",
            "SELECT * FROM partition_ops.partitions LIMIT 0",
            "SELECT * FROM partition_ops.foreign_keys LIMIT 0",
            "SELECT * FROM partition_ops.statement_policies LIMIT 0",
            "SELECT * FROM partition_ops.operator_approvals LIMIT 0",
            "UPDATE partition_ops.families SET phase='active' WHERE false",
            "SELECT partition_ops.expand('gateway.audit_logs',clock_timestamp(),'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
            "SELECT partition_ops.approve_operator_stage('gateway.audit_logs','expand',0,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','{}'::jsonb)",
            "CREATE TABLE partition_ops.__authority_probe_denied(id integer)",
        ] {
            denied(conn, sql).await?;
        }
    }
    Ok(
        json!({"role":role,"actual_login":true,"acl":"passed","service_admission_queries_checked":installed>=30,"partition_owner_operations_denied":installed>=35}),
    )
}
async fn add_document(
    conn: &mut PgConnection,
    o: &ProbeOptions,
    doc: &str,
    execution: Option<&str>,
    status: &str,
    special: bool,
) -> Result<()> {
    let metadata = execution
        .map(|e| json!({"_document_pipeline_execution_id":e}))
        .unwrap_or(json!({}))
        .to_string();
    query(conn,"INSERT INTO knowledge.documents(document_id,dataset_id,title,status,process_rule_id,metadata) VALUES($1,$2,'schema successor synthetic probe','indexing',$3,$4::pg_catalog.jsonb)",&[doc,&o.dataset,&o.rule,&metadata]).await?;
    if let Some(execution) = execution {
        query(conn,"INSERT INTO knowledge.document_pipeline_executions(execution_id,document_id,dataset_id,action,process_rule_id,status,manifest) VALUES($1,$2,$3,'recover',$4,$5,$6::pg_catalog.jsonb)",&[execution,doc,&o.dataset,&o.rule,status,&if special{json!({"special_publication":true})}else{json!({})}.to_string()]).await?;
    }
    Ok(())
}
async fn fixtures(conn: &mut PgConnection, o: &ProbeOptions) -> Result<()> {
    require(boolean(conn,"SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE dataset_id=$1 AND NOT is_deleted AND NOT is_archived)",&[&o.dataset]).await?)?;
    require(boolean(conn,"SELECT EXISTS(SELECT 1 FROM knowledge.dataset_process_rules WHERE dataset_id=$1 AND id=$2)",&[&o.dataset,&o.rule]).await?)
}
async fn requeue_cases(
    conn: &mut PgConnection,
    o: &ProbeOptions,
    sql: &str,
) -> Result<Vec<String>> {
    fixtures(conn, o).await?;
    require(
        boolean(
            conn,
            "SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE dataset_id=$1)",
            &[&o.other_dataset],
        )
        .await?,
    )?;
    let mut passed = Vec::new();
    for (case, expected) in CASES {
        begin(conn).await?;
        let result:Result<()>=async{let doc=id();let execution=id();add_document(conn,o,&doc,Some(&execution),case.strip_prefix("execution_").unwrap_or("running"),*case=="special_manifest").await?;let mut params=[doc.clone(),o.dataset.clone(),execution.clone()];if let Some(index)=["wrong_document","wrong_dataset","wrong_execution"].iter().position(|c|c==case){params[index]+="_wrong";}else{match *case{
 "missing_execution_marker"|"replaced_generation"|"special_document"=>{let metadata=match *case{"missing_execution_marker"=>json!({}),"replaced_generation"=>json!({"_document_pipeline_execution_id":format!("{execution}_new")}),_=>json!({"_document_pipeline_execution_id":execution,"_special_publication_generation_id":"special"})};query(conn,"UPDATE knowledge.documents SET metadata=$2::pg_catalog.jsonb WHERE document_id=$1",&[&doc,&metadata.to_string()]).await?;},
 "null_process_rule"=>{query(conn,"UPDATE knowledge.documents SET process_rule_id=NULL WHERE document_id=$1",&[&doc]).await?;},
 "dataset_deleted"=>{query(conn,"UPDATE knowledge.datasets SET is_deleted=true WHERE dataset_id=$1",&[&o.dataset]).await?;},
 "dataset_archived"=>{query(conn,"UPDATE knowledge.datasets SET is_archived=true,archived_at=clock_timestamp(),archived_by='schema_successor_probe' WHERE dataset_id=$1",&[&o.dataset]).await?;},
 "dataset_deletion_fence"=>{query(conn,"UPDATE knowledge.datasets SET index_config=$2::pg_catalog.jsonb WHERE dataset_id=$1",&[&o.dataset,&json!({"retrieval":{"_index_deletion_fence":{}}}).to_string()]).await?;},
 "document_disabled"|"document_archived"=>{let clause=if *case=="document_disabled"{"enabled=false"}else{"archived=true"};query(conn,&format!("UPDATE knowledge.documents SET {clause} WHERE document_id=$1"),&[&doc]).await?;},
 "lifecycle_deactivating"|"lifecycle_restore_pending"|"lifecycle_restore_disabled"=>{let metadata=json!({"_document_lifecycle_reindex":{"status":if *case=="lifecycle_deactivating"{"deactivating"}else{"pending"},"desired_enabled":*case!="lifecycle_restore_disabled","desired_archived":false}});query(conn,"UPDATE knowledge.documents SET enabled=false,archived=true,metadata=metadata||$2::pg_catalog.jsonb WHERE document_id=$1",&[&doc,&metadata.to_string()]).await?;},
 "document_terminal"|"document_waiting"=>{query(conn,"UPDATE knowledge.documents SET status=$2 WHERE document_id=$1",&[&doc,if *case=="document_terminal"{"completed"}else{"waiting"}]).await?;},_=>{}}}
 let row=sqlx::query(sql).bind(&params[0]).bind(&params[1]).bind(&params[2]).fetch_optional(&mut *conn).await?;require(row.is_some()==*expected)?;if *expected{require(boolean(conn,"SELECT status='waiting' AND progress=0 AND error IS NULL FROM knowledge.documents WHERE document_id=$1",&[&doc]).await?)?;}Ok(())}.await;
        rollback(conn).await?;
        result?;
        passed.push((*case).into());
    }
    begin(conn).await?;
    let result:Result<()>=async{let doc=id();add_document(conn,o,&doc,None,"running",false).await?;sqlx::raw_sql("SAVEPOINT expected_fk").execute(&mut *conn).await?;let result=sqlx::query("INSERT INTO knowledge.document_pipeline_executions(execution_id,document_id,dataset_id,action,process_rule_id,status) VALUES($1,$2,$3,'recover',NULL,'running')").bind(id()).bind(&doc).bind(&o.other_dataset).execute(&mut *conn).await;let rejected=matches!(&result,Err(sqlx::Error::Database(e)) if e.code().as_deref()==Some("23503")&&e.constraint()==Some("fk_pipeline_execution_document_dataset"));sqlx::raw_sql("ROLLBACK TO SAVEPOINT expected_fk; RELEASE SAVEPOINT expected_fk").execute(&mut *conn).await?;require(rejected)?;require(query(conn,"DELETE FROM knowledge.documents WHERE document_id=$1",&[&doc]).await?==1)}.await;
    rollback(conn).await?;
    result?;
    passed.push("composite_fk_rejected".into());
    Ok(passed)
}
async fn api_cases(conn: &mut PgConnection, o: &ProbeOptions) -> Result<()> {
    fixtures(conn, o).await?;
    begin(conn).await?;
    let result:Result<()>=async{let doc=id();add_document(conn,o,&doc,None,"running",false).await?;let count=|doc:String,dataset:String|sqlx::query_scalar::<_,i64>("SELECT count(*) FROM knowledge.kb_document_progress_events WHERE dataset_id=$1 AND document_id=$2").bind(dataset).bind(doc);require(count(doc.clone(),o.dataset.clone()).fetch_one(&mut *conn).await?==1)?;query(conn,"UPDATE knowledge.documents SET title=title WHERE document_id=$1",&[&doc]).await?;require(count(doc.clone(),o.dataset.clone()).fetch_one(&mut *conn).await?==1)?;query(conn,"UPDATE knowledge.documents SET status='completed',progress=100 WHERE document_id=$1",&[&doc]).await?;query(conn,"DELETE FROM knowledge.documents WHERE document_id=$1",&[&doc]).await?;let events=sqlx::query("SELECT event_type,payload FROM knowledge.kb_document_progress_events WHERE dataset_id=$1 AND document_id=$2 ORDER BY event_sequence").bind(&o.dataset).bind(&doc).fetch_all(&mut *conn).await?;let types=events.iter().map(|r|r.try_get::<String,_>("event_type")).collect::<std::result::Result<Vec<_>,_>>()?;require(types==["progress","terminal","deleted"])?;let payload:Value=events.last().ok_or_else(fail)?.try_get("payload")?;require(payload["document_id"]==doc&&payload["terminal"]==true&&payload["progress"]["state"]=="deleted")}.await;
    rollback(conn).await?;
    result
}
async fn durable(conn: &mut PgConnection) -> Result<Value> {
    begin(conn).await?;
    let result:Result<()>=async{let task=id();query(conn,"INSERT INTO gateway.tasks(task_id,request_id,service_id,user_id,tenant_id) VALUES($1,$1,$1,$1,$1)",&[&task]).await?;query(conn,"INSERT INTO gateway.task_deliveries(task_id,request_id,service_id,user_id,tenant_id,payload,priority,state) VALUES($1,$1,$1,$1,$1,'{}'::pg_catalog.jsonb,0,'pending')",&[&task]).await?;let claim=uuid::Uuid::new_v4().to_string();require(query(conn,"UPDATE gateway.task_deliveries SET state='claimed_unknown',claim_id=$2::uuid,claimed_at=clock_timestamp() WHERE task_id=$1 AND state='pending'",&[&task,&claim]).await?==1)?;let complete="UPDATE gateway.task_deliveries SET state='completed',finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2::uuid AND state='claimed_unknown'";require(query(conn,complete,&[&task,&uuid::Uuid::new_v4().to_string()]).await?==0)?;require(query(conn,complete,&[&task,&claim]).await?==1)?;require(query(conn,"UPDATE gateway.task_deliveries SET state='failed',finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2::uuid AND state='claimed_unknown'",&[&task,&claim]).await?==0)?;denied(conn,"DELETE FROM gateway.task_deliveries WHERE false").await}.await;
    rollback(conn).await?;
    result?;
    Ok(
        json!({"gateway_ledger_insert_claim_fence_terminal":"passed","all_rows_rolled_back":true,"native_ledger_code_execution":false}),
    )
}
async fn shadow(conn: &mut PgConnection, o: &ProbeOptions, identity: &Value) -> Result<Value> {
    if !boolean(
        conn,
        "SELECT has_database_privilege(current_user,current_database(),'TEMP')",
        &[],
    )
    .await?
    {
        return Ok(
            json!({"status":"temp_creation_denied_by_existing_role","shadow_execution":false}),
        );
    }
    begin_shadow(conn).await?;
    let result: Result<()> = async {
        query(
            conn,
            "CREATE TEMP TABLE kb_document_progress_events (blocked pg_catalog.bool)",
            &[],
        )
        .await?;
        for name in ["text", "jsonb", "regclass"] {
            query(
                conn,
                &format!("CREATE DOMAIN pg_temp.{name} AS pg_catalog.{name} CHECK(false)"),
                &[],
            )
            .await?;
        }
        let snapshot: Value =
            sqlx::query_scalar("SELECT gateway.platform_schema_admission_snapshot_v1()")
                .fetch_one(&mut *conn)
                .await?;
        require(snapshot["epoch"] == identity["epoch"])?;
        let doc = id();
        add_document(conn, o, &doc, None, "running", false).await?;
        query(
            conn,
            "UPDATE knowledge.documents SET status='completed',progress=100 WHERE document_id=$1",
            &[&doc],
        )
        .await?;
        query(
            conn,
            "DELETE FROM knowledge.documents WHERE document_id=$1",
            &[&doc],
        )
        .await?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_temp.kb_document_progress_events")
                .fetch_one(&mut *conn)
                .await?;
        require(count == 0)
    }
    .await;
    rollback(conn).await?;
    result?;
    Ok(json!({"status":"passed_hostile_temporary_types_and_relation","ddl_rolled_back":true}))
}
pub async fn run(o: ProbeOptions) -> std::result::Result<Value, ProbeError> {
    let source_root = o.source_root.as_deref().unwrap_or(&o.root);
    let identity = contextual(
        source_contract_with_preimage_root(&o.root, source_root, o.preimage_root.as_deref()),
        "source_validation",
        None,
    )?;
    contextual(require(!o.shadow || o.run), "argument_validation", None)?;
    if !o.run {
        return Ok(
            json!({"status":"offline_source_only","production_requeue_sha256":identity["production_requeue_sha256"],"database_actions":false}),
        );
    }
    let canonical_source_root = contextual(
        source_root.canonicalize().map_err(AuthorityError::from),
        "source_validation",
        None,
    )?;
    let canonical_candidate_root = contextual(
        o.root.canonicalize().map_err(AuthorityError::from),
        "source_validation",
        None,
    )?;
    if canonical_source_root != canonical_candidate_root {
        let candidate_epoch = contextual(
            manifest::load_epoch_manifest(
                &o.root
                    .join("database/migrations")
                    .join(BASELINE)
                    .join("manifest.yml"),
            ),
            "source_validation",
            None,
        )?
        .epoch;
        if candidate_epoch == 26 {
            contextual(
                crate::source_tools::next_successor_026::verify_preview(source_root, &o.root, true),
                "source_validation",
                None,
            )?;
        } else if candidate_epoch == 27 {
            contextual(
                crate::source_tools::next_successor_027::verify_preview(source_root, &o.root, true),
                "source_validation",
                None,
            )?;
        } else if (30..=35).contains(&candidate_epoch) {
            contextual(
                crate::source_tools::service_successors::verify_preview(
                    source_root,
                    &o.root,
                    candidate_epoch,
                    true,
                ),
                "source_validation",
                None,
            )?;
        } else if candidate_epoch == 29 {
            contextual(
                crate::source_tools::next_successor_029::verify_preview(source_root, &o.root, true),
                "source_validation",
                None,
            )?;
        } else if candidate_epoch == 28 {
            contextual(
                crate::source_tools::next_successor_028::verify_preview(source_root, &o.root, true),
                "source_validation",
                None,
            )?;
        } else {
            contextual(
                successors::verify_measured_preview_with_preimage_root(
                    source_root,
                    &o.root,
                    o.preimage_root.as_deref(),
                ),
                "source_validation",
                None,
            )?;
        }
    }
    contextual(check_destination(&o), "argument_validation", None)?;
    let paths = AuthorityPaths {
        database_dir: o.root.join("database"),
    };
    let baseline = contextual(
        manifest::load_baseline_manifest(&paths.baseline_dir(BASELINE).join("manifest.json")),
        "source_validation",
        None,
    )?;
    let epoch_manifest = contextual(
        manifest::load_epoch_manifest(&paths.epoch_dir(BASELINE).join("manifest.yml")),
        "source_validation",
        None,
    )?;
    let current_epoch = epoch_manifest.epoch;
    let changes = epoch_manifest.changes;
    for epoch in BTreeSet::from([18, 19, 20, current_epoch]) {
        let applied = changes
            .iter()
            .filter(|c| c.sequence <= epoch)
            .map(|c| (c.sequence, c.sha256.clone()))
            .collect::<BTreeMap<_, _>>();
        contextual(
            commands::expected_epoch_fingerprints(&paths, &baseline, &applied),
            "verification_fingerprints",
            None,
        )?;
    }
    let mut connections = BTreeMap::new();
    let mut matrices = Vec::new();
    for role in ROLES {
        let mut conn = contextual(connect(&o, role).await, "principal_connection", Some(*role))?;
        contextual(begin(&mut conn).await, "role_matrix", Some(*role))?;
        let result = matrix(&mut conn, role, &o, &identity).await;
        let rollback_result = rollback(&mut conn).await;
        contextual(rollback_result, "role_matrix", Some(*role))?;
        matrices.push(contextual(result, "role_matrix", Some(*role))?);
        connections.insert(*role, conn);
    }
    let requeue_sql = contextual(production_requeue(source_root), "source_validation", None)?;
    let passed = contextual(
        requeue_cases(
            connections.get_mut("knowledge_worker").unwrap(),
            &o,
            &requeue_sql,
        )
        .await,
        "knowledge_worker_requeue",
        Some("knowledge_worker"),
    )?;
    contextual(
        api_cases(connections.get_mut("knowledge_api").unwrap(), &o).await,
        "knowledge_api_progress",
        Some("knowledge_api"),
    )?;
    let durable = contextual(
        durable(connections.get_mut("gateway").unwrap()).await,
        "gateway_durable_delivery",
        Some("gateway"),
    )?;
    let shadow = if o.shadow {
        let mut fresh = contextual(
            connect(&o, "knowledge_api").await,
            "principal_connection",
            Some("knowledge_api"),
        )?;
        let result = shadow(&mut fresh, &o, &identity).await;
        contextual(
            fresh.close().await.map_err(AuthorityError::from),
            "connection_close",
            Some("knowledge_api"),
        )?;
        contextual(result, "temporary_shadow", Some("knowledge_api"))?
    } else {
        json!({"status":"not_run_requires_separate_ddl_approval"})
    };
    for (role, conn) in connections {
        contextual(
            conn.close().await.map_err(AuthorityError::from),
            "connection_close",
            Some(role),
        )?;
    }
    Ok(
        json!({"status":"passed_bounded_actual_role_sql","matrix":matrices,"knowledge":{"actual_worker_requeue_cases":passed,"actual_api_progress_payload":"passed","all_rows_rolled_back":true,"sequence_increments_may_remain":true},"durable":durable,"temporary_shadow":shadow,"source":identity,"not_verified":["native process startup","native PgTaskDeliveryLedger execution","Redis/session revocation","concurrent connections/claim race","process crash","provider/HTTP","foreign trigger attachment","seven-day retention"]}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const LEDGER_ACL_PREIMAGE_PATH: &str = "database/authority/ledger_acl.py";
    const LEDGER_ACL_PREIMAGE_SHA256: &str =
        "9df9e3a5d6f572da0ea325720b9b346dac774055de8e9d900f3f82c2b55857fc";
    const LEDGER_ACL_PREIMAGE_FIXTURE: &str =
        include_str!("test_fixtures/ledger_acl_preimage_v1.json");

    fn pinned_ledger_acl_preimage() -> Vec<u8> {
        let fixture: Value = serde_json::from_str(LEDGER_ACL_PREIMAGE_FIXTURE)
            .expect("pinned ledger ACL fixture must be valid JSON");
        assert_eq!(fixture["schema"], "agentforge/test-pinned-preimage/v1");
        assert_eq!(fixture["path"], LEDGER_ACL_PREIMAGE_PATH);
        assert_eq!(fixture["sha256"], LEDGER_ACL_PREIMAGE_SHA256);
        let bytes = fixture["content"]
            .as_str()
            .expect("pinned ledger ACL fixture content must be UTF-8")
            .as_bytes();
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            LEDGER_ACL_PREIMAGE_SHA256
        );
        bytes.to_vec()
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ai-platform-probe-{label}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct SourceFixture {
        root: PathBuf,
        _scratch: Option<Scratch>,
    }

    impl SourceFixture {
        fn source_17() -> Self {
            let current = project_root().canonicalize().unwrap();
            let epoch_path = current
                .join("database/migrations")
                .join(BASELINE)
                .join("manifest.yml");
            let current_epoch = manifest::load_epoch_manifest(&epoch_path).unwrap();
            assert!(matches!(
                current_epoch.epoch,
                17 | 20 | 21 | 22 | 23 | 24 | 25 | 26 | 27 | 28 | 29 | 30 | 31 | 32 | 33 | 34 | 35
            ));
            if current_epoch.epoch == 25 {
                let receipt =
                    crate::source_tools::next_successor_025::verify_activated_source(&current)
                        .unwrap();
                assert_eq!(receipt["candidate_epoch"], current_epoch.epoch);
            } else if current_epoch.epoch == 24 {
                let receipt =
                    crate::source_tools::next_successor_024::verify_activated_source(&current)
                        .unwrap();
                assert_eq!(receipt["candidate_epoch"], current_epoch.epoch);
            } else if current_epoch.epoch == 23 {
                let (_, verified_epoch) =
                    crate::source_tools::next_successor_023::verify_activated_source(&current)
                        .unwrap();
                assert_eq!(verified_epoch.epoch, current_epoch.epoch);
            } else if current_epoch.epoch != 17 {
                let (_, verified_epoch) = successors::verify_activated_source(&current).unwrap();
                assert_eq!(verified_epoch.epoch, current_epoch.epoch);
            }

            let epoch_dir = current.join("database/migrations").join(BASELINE);
            let predecessor_manifest = if current_epoch.epoch == 17 {
                epoch_path
            } else {
                epoch_dir.join("017_predecessor_manifest.yml")
            };
            let predecessor_lock = if current_epoch.epoch == 17 {
                current.join("database/authority/package-authority-lock.json")
            } else {
                epoch_dir.join("017_predecessor_package_lock.json")
            };
            let lock: Value =
                serde_json::from_slice(&std::fs::read(&predecessor_lock).unwrap()).unwrap();
            let scratch = Scratch::new("source-17-fixture");
            let fixture_root = scratch.0.join("source");
            std::fs::create_dir_all(&fixture_root).unwrap();

            fn copy(source: &Path, destination: &Path) {
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                if destination.exists() {
                    assert_eq!(
                        std::fs::read(source).unwrap(),
                        std::fs::read(destination).unwrap()
                    );
                } else {
                    std::fs::copy(source, destination).unwrap();
                }
            }

            let files = lock["files_sha256"].as_object().unwrap();
            let old_manifest_relative = format!("migrations/{BASELINE}/manifest.yml");
            for (relative, expected) in files {
                let active_source = current.join("database").join(relative);
                let source = if relative == &old_manifest_relative {
                    &predecessor_manifest
                } else {
                    &active_source
                };
                assert_eq!(
                    manifest::file_sha256(source).unwrap(),
                    expected.as_str().unwrap()
                );
                copy(source, &fixture_root.join("database").join(relative));
            }
            copy(
                &current.join("database/authority/ledger_acl_v1.json"),
                &fixture_root.join("database/authority/ledger_acl_v1.json"),
            );
            copy(
                &predecessor_lock,
                &fixture_root.join("database/authority/package-authority-lock.json"),
            );
            copy(
                &current.join("database/authority/native-producer-migration-v1.json"),
                &fixture_root.join("database/authority/native-producer-migration-v1.json"),
            );

            let descriptor: Value = serde_json::from_slice(
                &std::fs::read(
                    current.join("database/authority/native-producer-migration-v1.json"),
                )
                .unwrap(),
            )
            .unwrap();
            for retired in descriptor["retired"].as_array().unwrap() {
                for replacement in retired["replacements"].as_object().unwrap().keys() {
                    copy(&current.join(replacement), &fixture_root.join(replacement));
                }
            }

            for relative in [
                "docs/design/canonical-schema-successors.json",
                "docs/design/application-schema-admission.sql.proposal",
                "docs/design/knowledge-progress-trigger-authority.sql.proposal",
                "docs/design/generic-task-durable-schema.sql.proposal",
                SOURCE,
            ] {
                copy(&current.join(relative), &fixture_root.join(relative));
            }
            let import_path = current.join("reports/dots-source-import-20261015.json");
            let mut import: Value =
                serde_json::from_slice(&std::fs::read(&import_path).unwrap()).unwrap();
            let canonical_fixture = std::fs::canonicalize(&fixture_root).unwrap();
            // Unit-only source-distribution envelope: this deliberately tests
            // offline hash/path binding, not a historical archive import or DB
            // measurement. Never mutate/re-create the real frozen import receipt.
            let archive = scratch.0.join("synthetic-offline-archive.fixture");
            std::fs::write(
                &archive,
                b"synthetic offline verifier input; not the historical source archive\n",
            )
            .unwrap();
            let import_manifest = scratch.0.join("synthetic-offline-import-manifest.json");
            std::fs::write(
                &import_manifest,
                br#"{"unit_fixture":true,"historical_import_verified":false}"#,
            )
            .unwrap();
            // Materialize the single hash-pinned historical fixture outside
            // the checkout. No Git metadata or retired service tree is needed.
            let preimage_root = scratch.0.join("historical-preimage-fixture");
            let preimage = preimage_root.join(LEDGER_ACL_PREIMAGE_PATH);
            std::fs::create_dir_all(preimage.parent().unwrap()).unwrap();
            // Inert public fixture only: this retired source is hashed, copied
            // into the synthetic preimage root, and never imported or executed.
            std::fs::write(&preimage, pinned_ledger_acl_preimage()).unwrap();
            assert_eq!(
                manifest::file_sha256(&preimage).unwrap(),
                LEDGER_ACL_PREIMAGE_SHA256
            );
            import["target"] = json!(canonical_fixture);
            import["previous_checkout"] = json!(preimage_root.canonicalize().unwrap());
            import["source_archive"] = json!(archive.canonicalize().unwrap());
            import["archive_sha256"] = json!(manifest::file_sha256(&archive).unwrap());
            import["manifest_sha256"] = json!(manifest::file_sha256(&import_manifest).unwrap());
            import["unit_fixture"] = json!(true);
            import["historical_import_evidence"] = json!(false);
            let report_path = fixture_root.join("reports/dots-source-import-20261015.json");
            std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
            std::fs::write(&report_path, serde_json::to_vec_pretty(&import).unwrap()).unwrap();

            Self {
                root: canonical_fixture,
                _scratch: Some(scratch),
            }
        }
    }

    fn project_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
    }
    fn preimage_root(source_root: &Path) -> PathBuf {
        let receipt: Value = serde_json::from_slice(
            &std::fs::read(source_root.join("reports/dots-source-import-20261015.json")).unwrap(),
        )
        .unwrap();
        PathBuf::from(receipt["previous_checkout"].as_str().unwrap())
    }

    #[test]
    fn destination_guard() {
        let mut o = ProbeOptions {
            root: ".".into(),
            source_root: None,
            preimage_root: None,
            run: true,
            shadow: false,
            database: "schema_successor_verify_abcdefgh".into(),
            prefix: "ai_gateway_".into(),
            dataset: "schema_successor_probe_abcdefgh".into(),
            other_dataset: "schema_successor_probe_ijklmnop".into(),
            rule: "existing".into(),
        };
        assert!(check_destination(&o).is_ok());
        o.other_dataset = o.dataset.clone();
        assert!(check_destination(&o).is_err());
    }
    #[test]
    fn cases_are_pinned() {
        assert_eq!(CASES.len(), 22);
        assert_eq!(CASES.iter().filter(|(_, v)| *v).count(), 2);
    }

    #[test]
    fn probe_error_reports_only_bounded_phase_and_role_context() {
        let error = ProbeError::new(
            "role_matrix",
            Some("runtime"),
            AuthorityError::Refused("postgresql://secret.example/db".into()),
        );
        assert_eq!(error.phase(), "role_matrix");
        assert_eq!(error.role(), Some("runtime"));
        assert_eq!(error.error_class(), "refused");
        assert_eq!(error.sqlstate(), None);
        assert!(!error.to_string().contains("secret.example"));
    }

    #[test]
    fn source_fixture_is_external_synthetic_and_frozen_preimage_checked() {
        let fixture = SourceFixture::source_17();
        let import: Value = serde_json::from_slice(
            &std::fs::read(
                fixture
                    .root
                    .join("reports/dots-source-import-20261015.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(import["unit_fixture"], true);
        assert_eq!(import["historical_import_evidence"], false);
        let archive = PathBuf::from(import["source_archive"].as_str().unwrap());
        let preimages = preimage_root(&fixture.root);
        assert!(!archive.starts_with(&fixture.root));
        assert!(!preimages.starts_with(&fixture.root));
        assert_eq!(
            manifest::file_sha256(&archive).unwrap(),
            import["archive_sha256"]
        );
        assert!(
            !fixture
                .root
                .join("database/authority/ledger_acl.py")
                .exists()
        );
        assert!(successors::validate_with_preimage_root(&fixture.root, Some(&preimages)).is_ok());
        std::fs::write(
            preimages.join("database/authority/ledger_acl.py"),
            b"tampered fixture",
        )
        .unwrap();
        assert!(successors::validate_with_preimage_root(&fixture.root, Some(&preimages)).is_err());
    }

    #[test]
    fn external_preview_binds_source_and_rejects_candidate_tampering() {
        let source_fixture = SourceFixture::source_17();
        let source_root = source_fixture.root.clone();
        let preimage_root = preimage_root(&source_root);
        let scratch = Scratch::new("external-preview");
        let candidate_root = scratch.0.join("candidate");
        let receipt = successors::materialize_with_preimage_root(
            &source_root,
            &candidate_root,
            Some(&preimage_root),
        )
        .unwrap();
        assert_eq!(receipt["epoch"], 20);

        let identity =
            source_contract_with_preimage_root(&candidate_root, &source_root, Some(&preimage_root))
                .unwrap();
        assert_eq!(identity["baseline_id"], BASELINE);
        assert_eq!(identity["epoch"], 20);
        assert_eq!(identity["production_requeue_sha256"], REQUEUE_SQL_SHA256);

        let support_file = candidate_root.join("database/bootstrap/extensions.sql");
        let mut changed = std::fs::read(&support_file).unwrap();
        changed.extend_from_slice(b"\n-- tampered");
        std::fs::write(support_file, changed).unwrap();
        assert!(
            successors::verify_preview_with_preimage_root(
                &source_root,
                &candidate_root,
                Some(&preimage_root)
            )
            .is_err()
        );
    }

    #[test]
    fn measured_preview_accepts_only_bound_reference_set() {
        // This builds synthetic JSON evidence to exercise the offline verifier;
        // it does not claim that a database measurement ran.
        let source_fixture = SourceFixture::source_17();
        let source_root = source_fixture.root.clone();
        let preimage_root = preimage_root(&source_root);
        let scratch = Scratch::new("measured-preview");
        let candidate_root = scratch.0.join("candidate");
        successors::materialize_with_preimage_root(
            &source_root,
            &candidate_root,
            Some(&preimage_root),
        )
        .unwrap();

        let paths = AuthorityPaths {
            database_dir: candidate_root.join("database"),
        };
        let (baseline, baseline_sha) = commands::load_baseline(&paths, BASELINE).unwrap();
        let epoch_path = paths.epoch_dir(BASELINE).join("manifest.yml");
        let epoch = manifest::load_epoch_manifest(&epoch_path).unwrap();
        let epoch_sha = manifest::file_sha256(&epoch_path).unwrap();
        let mut references = serde_json::Map::new();
        for sequence in 18..=20 {
            let changes = epoch
                .changes
                .iter()
                .filter(|change| change.sequence <= sequence)
                .map(|change| (change.sequence.to_string(), json!(change.sha256)))
                .collect::<serde_json::Map<_, _>>();
            let reference = json!({
                "baseline_id": BASELINE,
                "sequence": sequence,
                "baseline_manifest_sha256": baseline_sha,
                "changes": changes,
                "fingerprints": baseline.fingerprints()
            });
            let name = format!("{sequence:03}_verification.json");
            let path = paths.epoch_dir(BASELINE).join(&name);
            std::fs::write(&path, serde_json::to_vec(&reference).unwrap()).unwrap();
            references.insert(name, json!(manifest::file_sha256(&path).unwrap()));
        }
        let source_plan_sha = manifest::file_sha256(
            &source_root.join("docs/design/canonical-schema-successors.json"),
        )
        .unwrap();
        let import_path = source_root.join("reports/dots-source-import-20261015.json");
        let import: Value = serde_json::from_slice(&std::fs::read(&import_path).unwrap()).unwrap();
        let (producer_descriptor_sha256, producer_replacements_verified) =
            crate::source_tools::measure_successors::verify_native_producer_descriptor(
                &source_root,
                &baseline,
                &baseline_sha,
            )
            .unwrap();
        let source_distribution = json!({
            "archive_sha256": import["archive_sha256"],
            "import_manifest_sha256": import["manifest_sha256"],
            "import_receipt_sha256": manifest::file_sha256(&import_path).unwrap(),
            "source_files": 4599,
            "rust_files": 2082,
            "verified_source_files": 4599,
            "all_source_files_match_archive": true,
            "python_source_files_in_archive": 0,
            "producer_descriptor_sha256": producer_descriptor_sha256,
            "producer_replacements_verified": producer_replacements_verified,
            "historical_preimage_sha256": "9df9e3a5d6f572da0ea325720b9b346dac774055de8e9d900f3f82c2b55857fc",
            "historical_preimage_verified": true,
            "historical_git_blobs_verified": false
        });
        let measurement_receipt = json!({
            "schema": "ai-platform/canonical-successor-measurement/v1",
            "status": "measured_external_preview",
            "database_actions": true,
            "database_actions_scope": "named_isolated_database_only",
            "source_activation": false,
            "expected_database": "schema_successor_verify_abcdefgh",
            "source_plan_sha256": source_plan_sha,
            "baseline_manifest_sha256": baseline_sha,
            "epoch_manifest_sha256": epoch_sha,
            "successor_sequences": [18, 19, 20],
            "verification_refs": references,
            "post_migration_noop": true,
            "source_distribution": source_distribution
        });
        std::fs::write(
            candidate_root.join("SUCCESSOR_MEASUREMENT.json"),
            serde_json::to_vec(&measurement_receipt).unwrap(),
        )
        .unwrap();
        assert!(
            successors::verify_measured_preview_with_preimage_root(
                &source_root,
                &candidate_root,
                Some(&preimage_root)
            )
            .is_ok(),
            "synthetic but source-bound artifact shape should validate offline"
        );

        std::fs::write(
            paths.epoch_dir(BASELINE).join("018_verification.json"),
            br#"{"tampered":true}"#,
        )
        .unwrap();
        assert!(
            successors::verify_measured_preview_with_preimage_root(
                &source_root,
                &candidate_root,
                Some(&preimage_root)
            )
            .is_err()
        );
    }

    #[test]
    fn production_requeue_rejects_source_tampering_and_path_escape() {
        let source_fixture = SourceFixture::source_17();
        let source_root = source_fixture.root.clone();
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(production_requeue(&source_root).unwrap().as_bytes())
            ),
            REQUEUE_SQL_SHA256
        );
        let original = std::fs::read_to_string(source_root.join(SOURCE)).unwrap();
        let pinned_sql_fragment =
            "AND document.metadata ->> '_document_pipeline_execution_id' = $3";
        let tampered = original.replacen(
            pinned_sql_fragment,
            "AND document.metadata ->> '_document_pipeline_execution_id' <> $3",
            1,
        );
        assert!(original.contains(pinned_sql_fragment));
        assert_ne!(tampered, original);
        let scratch = Scratch::new("tampered-source");
        let copied_source = scratch.0.join(SOURCE);
        std::fs::create_dir_all(copied_source.parent().unwrap()).unwrap();
        std::fs::write(&copied_source, tampered).unwrap();
        assert!(production_requeue(&scratch.0).is_err());

        #[cfg(unix)]
        {
            let escaped = Scratch::new("escaped-source");
            let escaped_source = escaped.0.join(SOURCE);
            std::fs::create_dir_all(escaped_source.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(source_root.join(SOURCE), &escaped_source).unwrap();
            assert!(production_requeue(&escaped.0).is_err());
        }
    }
}
