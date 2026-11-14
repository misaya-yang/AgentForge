//! Exact source-bound forward correction for the measured fresh-37 predecessor.
//! It is not an arbitrary grant/rebind command and never edits business rows.
use super::*;
use crate::source_tools::gateway_automations_032 as correction;
use sqlx::{Connection, PgConnection};

pub(super) const PREDECESSOR: &str = "7767073b762c9263072de4eb5e1d58796822d468a49f448330a6f59428950f70";
const METADATA_PREDECESSOR: &str = "a51825587530f6d9b779b2f618dec9fdf9665d6a4aab1c5d014f43ea328372f4";
const METADATA_CATALOG: &str = "82767d7f14f5829a4acc53a8a2169a27b2a438cb1042e3c446c58ed7de264a3c";
// Measured from PostgreSQL JSONB decoded/serialized with the workspace's
// serde_json preserve_order feature, not Python sort_keys canonicalization.
const CATALOG: &str = "8a8e42b1fc231a2f096570ac1437f62b8097ff53001bc136bea88c7b9519aab1";
fn predecessor_fingerprints() -> Value {
    json!({"acl":"772d8a93dc4130c950779b0c4debba1b2a5cacb04d4cf50248b857f32ff170cd",
        "extensions":"fd20c5b99a0fbe847de60c75290f3135f3a9513fe2ad07b65529ca859973376a",
        "reference_data":"c71c1fd656e4e2559f4c6b09f8cbd1d0b2d57de6e5991bcd2c10d1b068c02318",
        "structural":"038ca1c736f007db7b8a06365f4778286936f5badd38423e9d916c9eccfe34af"})
}
fn expected_fingerprints(metadata_only: bool) -> Value {
    let mut value=predecessor_fingerprints();
    if metadata_only {
        value["acl"]=json!("bfb1c384b5cb034b9b86bd481776c56c03727ca3d94722422e62d93a30cc57e0");
    }
    value
}
pub(super) fn can_repair(source: &str) -> bool {
    matches!(source,PREDECESSOR|METADATA_PREDECESSOR)
}
/// Prove the predecessor is exactly the target matrix minus the four declared
/// records. Unknown/missing/partial grants are never filtered or accepted.
pub(super) fn predecessor_acl(actual: &[String]) -> Result<()> {
    if correction::RUNTIME_ACL_LINES.iter().any(|line| actual.iter().any(|v| v==line)) {
        return Err(refused("partial or already-applied fresh ACL correction refused"));
    }
    let mut target = actual.to_vec();
    target.extend(correction::RUNTIME_ACL_LINES.iter().map(|line| (*line).to_owned()));
    install::exact_installed_acl(&target)
}
pub(super) async fn apply(conn: &mut PgConnection, defs: &source::Definitions) -> Result<()> {
    let (spec, sql) = defs.canonical_additions.iter().find(|(spec,_)| spec.sequence==32)
        .ok_or_else(|| refused("canonical 032 ACL source absent"))?;
    execution::execute(conn, correction::runtime_acl_correction_sql(sql)?).await?;
    install::conditions(conn, &spec.postconditions).await
}
async fn protected_data(conn: &mut PgConnection) -> Result<Value> {
    // Hash complete rows on the server: neither password hashes nor account
    // identities leave the database. Role attributes/memberships are immutable.
    Ok(sqlx::query_scalar(r#"SELECT jsonb_build_object(
      'users',(SELECT jsonb_build_object('count',count(*),'sha256',encode(pg_catalog.sha256(pg_catalog.convert_to(COALESCE(jsonb_agg(to_jsonb(u) ORDER BY user_id),'[]'::jsonb)::text,'UTF8')),'hex')) FROM gateway.users u),
      'user_roles',(SELECT encode(pg_catalog.sha256(pg_catalog.convert_to(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id),'[]'::jsonb)::text,'UTF8')),'hex') FROM gateway.user_roles r),
      'user_permissions',(SELECT encode(pg_catalog.sha256(pg_catalog.convert_to(COALESCE(jsonb_agg(to_jsonb(p) ORDER BY id),'[]'::jsonb)::text,'UTF8')),'hex') FROM gateway.user_permissions p),
      'roles',(SELECT jsonb_agg(to_jsonb(r) ORDER BY rolname) FROM (SELECT rolname,rolcanlogin,rolsuper,rolcreatedb,rolcreaterole,rolreplication,rolbypassrls,rolinherit,rolconnlimit,rolvaliduntil,rolconfig FROM pg_catalog.pg_roles WHERE rolname LIKE 'ai_gateway_%') r),
      'memberships',(SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY roleid,member),'[]'::jsonb) FROM pg_catalog.pg_auth_members m))"#)
        .fetch_one(conn).await?)
}
pub(super) async fn repair(conn: &mut PgConnection, prefix: &str, defs: &source::Definitions) -> Result<()> {
    let mut tx = conn.begin().await?;
    execution::execute(&mut tx,"SET LOCAL statement_timeout='30s';SET LOCAL lock_timeout='5s';SET LOCAL idle_in_transaction_session_timeout='20s';SET LOCAL client_connection_check_interval='1s'").await?;
    let operator: bool = sqlx::query_scalar("SELECT current_database()='gateway_dev_fresh' AND inet_client_addr() IS NULL AND inet_server_addr() IS NULL AND current_user=session_user AND (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)")
        .fetch_one(&mut *tx).await?;
    if !operator || prefix!="ai_gateway_" {
        return Err(refused("measured fresh ACL repair requires its independent local peer operator and exact role prefix"));
    }
    crate::runner::set_role(&mut tx,&format!("{prefix}owner")).await?;
    let stored:Value=sqlx::query_scalar("SELECT jsonb_build_object('source',source_sha256,'catalog',catalog,'fingerprints',fingerprints,'partition',partition_catalog) FROM public.platform_fresh_schema_state WHERE singleton FOR UPDATE")
        .fetch_one(&mut *tx).await?;
    let observed=catalog::capture(&mut tx).await?;
    let actual=crate::fingerprint::compute_fingerprints(&mut tx,prefix,&defs.baseline.reference_data).await?;
    let partition=crate::source_tools::service_successors::partition_catalog::capture(&mut tx,prefix).await?;
    let observed_sha=sha(&serde_json::to_vec(&observed).map_err(|_|refused("catalog encoding"))?);
    let metadata_only=stored["source"]==METADATA_PREDECESSOR;
    let source_matches=stored["source"]==PREDECESSOR || metadata_only;
    let catalog_matches=observed==stored["catalog"];
    let catalog_hash_matches=observed_sha==if metadata_only {METADATA_CATALOG}else{CATALOG};
    let expected=expected_fingerprints(metadata_only);
    let stored_fingerprints_match=stored["fingerprints"]==expected;
    let actual_fingerprints_match=json!(&actual)==expected;
    let partition_matches=stored["partition"]==partition;
    let new_source=stored["source"]!=source_sha256();
    if !source_matches || !catalog_matches || !catalog_hash_matches
        || !stored_fingerprints_match || !actual_fingerprints_match || !partition_matches || !new_source {
        // Only booleans and locally computed hashes leave this failure path.
        // Never print stored JSON, accounts, role values, DSNs or credentials.
        eprintln!("fresh-schema ACL predecessor source={source_matches} catalog={catalog_matches} catalog_hash={catalog_hash_matches} stored_fingerprints={stored_fingerprints_match} actual_fingerprints={actual_fingerprints_match} partition={partition_matches} new_source={new_source} observed_catalog_sha256={observed_sha}");
        for class in ["structural","acl","extensions","reference_data"] {
            eprintln!("fresh-schema actual_fingerprint {class}={}",actual.get(class).map(String::as_str).unwrap_or("missing"));
        }
        return Err(refused("only the exact measured fresh-37 ACL predecessor without drift can be repaired"));
    }
    let acl=crate::fingerprint::acl_lines(&mut tx,prefix).await?;
    if metadata_only {install::exact_installed_acl(&acl)?;} else {predecessor_acl(&acl)?;}
    let protected=protected_data(&mut tx).await?;
    if metadata_only {
        // The measured a518 schema already owns the exact four ACL additions.
        // Only verify the OID-only predicates; never execute grants again.
        let (spec,_)=defs.canonical_additions.iter().find(|(spec,_)|spec.sequence==32)
            .ok_or_else(||refused("canonical 032 source absent"))?;
        install::conditions(&mut tx,&spec.postconditions).await?;
    } else {apply(&mut tx,defs).await?;}
    install::exact_installed_acl(&crate::fingerprint::acl_lines(&mut tx,prefix).await?)?;
    let fingerprints=crate::fingerprint::compute_fingerprints(&mut tx,prefix,&defs.baseline.reference_data).await?;
    for class in ["structural","extensions","reference_data"] {
        if actual.get(class)!=fingerprints.get(class) {
            return Err(refused("fresh ACL correction changed a non-ACL fingerprint"));
        }
    }
    let acl_transition=if metadata_only {actual.get("acl")==fingerprints.get("acl")}else{actual.get("acl")!=fingerprints.get("acl")};
    if !acl_transition || protected_data(&mut tx).await?!=protected
        || crate::source_tools::service_successors::partition_catalog::capture(&mut tx,prefix).await?!=partition {
        return Err(refused("fresh ACL correction did not preserve account data, roles or partitions"));
    }
    let observed=catalog::capture(&mut tx).await?;
    let updated=sqlx::query("UPDATE public.platform_fresh_schema_state SET source_sha256=$1,catalog=$2,fingerprints=$3 WHERE singleton AND source_sha256=$4")
        .bind(source_sha256()).bind(observed).bind(json!(&fingerprints))
        .bind(if metadata_only {METADATA_PREDECESSOR}else{PREDECESSOR}).execute(&mut *tx).await?;
    if updated.rows_affected()!=1 { return Err(refused("fresh predecessor identity changed during correction")); }
    let snapshot:Value=sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()").fetch_one(&mut *tx).await?;
    validate_snapshot(&snapshot)?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_acl_delta_rejects_partial_missing_and_extra_privileges() {
        let policy:Value=serde_json::from_str(include_str!("acl_expected.json")).unwrap();
        let mut installed:Vec<String>=serde_json::from_value(policy["raw_acl_lines"].clone()).unwrap();
        installed.extend(install::metadata_acl());
        let old:Vec<_>=installed.iter().filter(|line|!correction::RUNTIME_ACL_LINES.contains(&line.as_str())).cloned().collect();
        predecessor_acl(&old).unwrap();
        for line in correction::RUNTIME_ACL_LINES {
            let mut partial=old.clone();partial.push(line.into());assert!(predecessor_acl(&partial).is_err());
        }
        let mut missing=old.clone();missing.pop();assert!(predecessor_acl(&missing).is_err());
        let mut extra=old;extra.push("column_acl:knowledge.dataset_keyword_tables.keywords:knowledge_api:SELECT:grantopt=0".into());
        assert!(predecessor_acl(&extra).is_err());
    }
}
