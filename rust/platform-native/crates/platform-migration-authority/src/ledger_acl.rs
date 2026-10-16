//! Exact versioned ACL matrix; no grants are silently widened or ignored.
use crate::{AuthorityError, Result, credentials::validate_role_prefix, runner::set_role};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use std::collections::BTreeMap;
pub const REVISION: &str = "authority-ledger-acl/v1";
pub const MANIFEST_SHA256: &str =
    "e1e12e4578b3edd3fa8ed9178f4adc0ba4fc22faac1d41a68e1bffd7ee1d1850";
const MANIFEST: &[u8] = include_bytes!("../../../../../database/authority/ledger_acl_v1.json");
const TABLES: &[&str] = &[
    "platform_schema_baselines",
    "platform_schema_change_attempts",
    "platform_schema_changes",
];
fn privileges(table: &str) -> &'static [&'static str] {
    if table == "platform_schema_change_attempts" {
        &["INSERT", "SELECT", "UPDATE"]
    } else {
        &["INSERT", "SELECT"]
    }
}
pub fn receipt_text(prefix: &str) -> Result<String> {
    validate_role_prefix(prefix)?;
    if format!("{:x}", Sha256::digest(MANIFEST)) != MANIFEST_SHA256 {
        return Err(AuthorityError::Refused(
            "authority ledger ACL manifest digest mismatch".into(),
        ));
    }
    // Python sort_keys=True canonical order; no whitespace.
    Ok(format!(
        "{{\"manifest_sha256\":\"{MANIFEST_SHA256}\",\"revision\":\"{REVISION}\",\"role_prefix\":\"{prefix}\"}}"
    ))
}
pub async fn inspect(conn: &mut PgConnection, prefix: &str) -> Result<bool> {
    let receipt = receipt_text(prefix)?;
    let owner = format!("{prefix}owner");
    let migrator = format!("{prefix}migrator");
    let rows=sqlx::query("SELECT c.relname,c.relkind::text AS relkind,pg_get_userbyid(c.relowner)::text AS owner,obj_description(c.oid,'pg_class') AS receipt FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname=ANY($1::text[]) ORDER BY c.relname").bind(TABLES).fetch_all(&mut *conn).await?;
    let schema=sqlx::query("SELECT a.privilege_type,a.is_grantable,pg_get_userbyid(a.grantor)::text AS grantor FROM pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) a WHERE n.nspname='public' AND a.grantee=(SELECT oid FROM pg_roles WHERE rolname=$1) ORDER BY a.privilege_type").bind(&migrator).fetch_all(&mut *conn).await?;
    if rows.is_empty() {
        if !schema.is_empty() {
            return Err(AuthorityError::Refused(
                "authority ACL partial bootstrap: schema grant without ledgers".into(),
            ));
        }
        return Ok(false);
    }
    if rows.len() != TABLES.len() {
        return Err(AuthorityError::Refused(
            "authority ledger ownership/table matrix mismatch".into(),
        ));
    }
    let mut marker = None;
    for row in rows {
        if row.try_get::<String, _>("owner")? != owner
            || row.try_get::<String, _>("relkind")? != "r"
        {
            return Err(AuthorityError::Refused(
                "authority ledger ownership/table matrix mismatch".into(),
            ));
        }
        if row.try_get::<String, _>("relname")? == "platform_schema_baselines" {
            marker = row.try_get::<Option<String>, _>("receipt")?;
        }
    }
    let active = marker.as_deref() == Some(&receipt);
    if marker.is_some() && !active {
        return Err(AuthorityError::Refused(
            "authority ledger bootstrap receipt absent or tampered".into(),
        ));
    }
    let rows=sqlx::query("SELECT c.relname,pg_get_userbyid(a.grantee)::text AS grantee,pg_get_userbyid(a.grantor)::text AS grantor,a.privilege_type,a.is_grantable FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a WHERE n.nspname='public' AND c.relname=ANY($1::text[]) AND a.grantee<>c.relowner ORDER BY c.relname,a.grantee,a.privilege_type").bind(TABLES).fetch_all(&mut *conn).await?;
    let columns:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname=ANY($1::text[]) AND a.attnum>0 AND NOT a.attisdropped AND cardinality(a.attacl)>0)").bind(TABLES).fetch_one(&mut *conn).await?;
    if columns {
        return Err(AuthorityError::Refused(
            "authority ledger column-level grants are outside versioned matrix".into(),
        ));
    }
    let mut expected = Vec::new();
    if active {
        for table in TABLES {
            for privilege in privileges(table) {
                expected.push((
                    table.to_string(),
                    migrator.clone(),
                    owner.clone(),
                    privilege.to_string(),
                    false,
                ));
            }
        }
    }
    let mut actual = Vec::new();
    for r in rows {
        actual.push((
            r.try_get::<String, _>("relname")?,
            r.try_get::<String, _>("grantee")?,
            r.try_get::<String, _>("grantor")?,
            r.try_get::<String, _>("privilege_type")?,
            r.try_get::<bool, _>("is_grantable")?,
        ));
    }
    expected.sort();
    actual.sort();
    let mut schema_actual = Vec::new();
    for r in schema {
        schema_actual.push((
            r.try_get::<String, _>("privilege_type")?,
            r.try_get::<bool, _>("is_grantable")?,
            r.try_get::<String, _>("grantor")?,
        ));
    }
    let schema_expected = if active {
        vec![("USAGE".to_string(), false, owner)]
    } else {
        vec![]
    };
    if expected != actual || schema_actual != schema_expected {
        return Err(AuthorityError::Refused(
            "authority ledger ACL matrix incomplete or exceeds versioned grants".into(),
        ));
    }
    Ok(active)
}
pub async fn install(conn: &mut PgConnection, prefix: &str) -> Result<()> {
    if inspect(conn, prefix).await? {
        return Ok(());
    }
    let mut tx = conn.begin().await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *tx)
        .await?;
    set_role(&mut tx, &format!("{prefix}owner")).await?;
    sqlx::raw_sql(&format!(
        "GRANT USAGE ON SCHEMA public TO \"{prefix}migrator\";"
    ))
    .execute(&mut *tx)
    .await?;
    for table in TABLES {
        sqlx::raw_sql(&format!(
            "GRANT {} ON TABLE public.\"{table}\" TO \"{prefix}migrator\"",
            privileges(table).join(", ")
        ))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::raw_sql(&format!(
        "COMMENT ON TABLE public.platform_schema_baselines IS '{}'",
        receipt_text(prefix)?
    ))
    .execute(&mut *tx)
    .await?;
    set_role(&mut tx, &previous).await?;
    tx.commit().await?;
    if !inspect(conn, prefix).await? {
        return Err(AuthorityError::Refused(
            "authority ledger ACL bootstrap did not verify".into(),
        ));
    }
    Ok(())
}
pub async fn compatible_expected(
    conn: &mut PgConnection,
    expected: &BTreeMap<String, String>,
    prefix: &str,
) -> Result<BTreeMap<String, String>> {
    if !inspect(conn, prefix).await? {
        return Err(AuthorityError::Refused(
            "authority ledger ACL v1 receipt is required; run explicit bootstrap".into(),
        ));
    }
    let mut tx = conn.begin().await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *tx)
        .await?;
    set_role(&mut tx, &format!("{prefix}owner")).await?;
    let search: String = sqlx::query_scalar("SELECT current_setting('search_path')")
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('search_path','pg_catalog',true)")
        .execute(&mut *tx)
        .await?;
    let lines = crate::fingerprint::acl_lines(&mut tx, prefix).await?;
    sqlx::query("SELECT set_config('search_path',$1,true)")
        .bind(search)
        .execute(&mut *tx)
        .await?;
    set_role(&mut tx, &previous).await?;
    tx.commit().await?;
    let delta = "schema_acl:public:migrator:USAGE:grantopt=0";
    if lines.iter().filter(|line| line.as_str() == delta).count() != 1 {
        return Err(AuthorityError::Refused(
            "authority ACL delta absent, duplicated or altered".into(),
        ));
    }
    let original = lines
        .iter()
        .filter(|line| line.as_str() != delta)
        .cloned()
        .collect::<Vec<_>>();
    if Some(&crate::fingerprint::canonical_digest(&original)) != expected.get("acl") {
        return Err(AuthorityError::Refused(
            "base ACL drift outside the versioned authority delta".into(),
        ));
    }
    let mut result = expected.clone();
    result.insert("acl".into(), crate::fingerprint::canonical_digest(&lines));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_matches_python_canonical_bytes() {
        assert_eq!(
            receipt_text("ai_gateway_").unwrap(),
            "{\"manifest_sha256\":\"e1e12e4578b3edd3fa8ed9178f4adc0ba4fc22faac1d41a68e1bffd7ee1d1850\",\"revision\":\"authority-ledger-acl/v1\",\"role_prefix\":\"ai_gateway_\"}"
        );
        assert!(receipt_text("unsafe'_").is_err());
    }
}

pub async fn receipt(
    conn: &mut PgConnection,
    expected: &BTreeMap<String, String>,
    prefix: &str,
) -> Result<serde_json::Value> {
    if !inspect(conn, prefix).await? {
        return Err(AuthorityError::Refused(
            "authority ledger ACL receipt unavailable".into(),
        ));
    }
    let derived = compatible_expected(conn, expected, prefix).await?;
    let matrix:Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(receipt_row) FROM (SELECT c.relname,pg_get_userbyid(a.grantee)::text AS grantee,pg_get_userbyid(a.grantor)::text AS grantor,a.privilege_type,a.is_grantable FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a WHERE n.nspname='public' AND c.relname=ANY($1::text[]) AND a.grantee<>c.relowner ORDER BY c.relname,a.grantee,a.privilege_type) receipt_row").bind(TABLES).fetch_all(conn).await?;
    Ok(
        serde_json::json!({"revision":REVISION,"manifest_sha256":MANIFEST_SHA256,"role_prefix":prefix,"base_acl_sha256":expected["acl"],"effective_acl_sha256":derived["acl"],"ledger_matrix":matrix}),
    )
}
