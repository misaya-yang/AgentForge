//! Source035 supplements (never replaces) the frozen four-schema fingerprints.
//! partition_ops is outside that historical scope: its real catalog and minimal
//! ACL must be measured, source-bound, replay-checked and compared independently.
use super::*;
use crate::credentials;
use sqlx::{Connection, PgConnection};
const POLICY: &[u8] = include_bytes!(
    "../../../../../../../docs/design/proposals/service-successors/partition-authority-policy.json"
);
const POLICY_PATH: &str =
    "docs/design/proposals/service-successors/partition-authority-policy.json";
fn policy() -> Result<Value> {
    let c = catalog()?;
    if step(&c, 35)?.auxiliary_inputs_sha256.get(POLICY_PATH) != Some(&safe::sha256_bytes(POLICY)) {
        return Err(refused("compiled partition policy changed"));
    }
    serde_json::from_slice(POLICY).map_err(|_| refused("partition policy malformed"))
}
fn strings(v: &Value) -> Result<BTreeSet<String>> {
    v.as_array()
        .ok_or_else(|| refused("partition policy array missing"))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or_else(|| refused("partition policy string missing"))
        })
        .collect()
}
fn list<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .ok_or_else(|| refused("partition catalog class missing"))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| refused("partition catalog field missing"))
}
/// Supplemental ACL is an exact set, including owner entries; no subset allowlist.
fn check_policy(snapshot: &Value) -> Result<()> {
    let p = policy()?;
    let acl = &snapshot["acl"];
    let expected_tables = strings(&p["tables"])?;
    let table_owners = list(acl, "acl_lines/owners")?;
    if table_owners.len() != expected_tables.len()
        || table_owners.iter().any(|r| r["owner"] != "owner")
        || table_owners
            .iter()
            .map(|r| r["name"].as_str().unwrap_or("").to_owned())
            .collect::<BTreeSet<_>>()
            != expected_tables
    {
        return Err(refused("partition control table owner/inventory changed"));
    }
    let expected_funcs = list(&p, "functions")?
        .iter()
        .map(|f| {
            Ok((
                format!("{}({})", text(f, "name")?, text(f, "identity_arguments")?),
                strings(&f["roles"])?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let func_key = |r: &Value| -> Result<String> {
        Ok(format!(
            "partition_ops.{}({})",
            text(r, "name")?,
            text(r, "identity_args")?
        ))
    };
    let owners = list(acl, "acl_lines/function_owners")?;
    if owners.len() != expected_funcs.len()
        || owners.iter().any(|r| r["owner"] != "owner")
        || owners
            .iter()
            .map(func_key)
            .collect::<Result<BTreeSet<_>>>()?
            != expected_funcs.keys().cloned().collect()
    {
        return Err(refused("partition function owner/inventory changed"));
    }
    let mut actual = BTreeSet::new();
    for row in list(acl, "acl_lines/function_acls")? {
        if row["grant_option"] != false
            || row["privileges"] != "EXECUTE"
            || !actual.insert((func_key(row)?, text(row, "grantee")?.to_owned()))
        {
            return Err(refused("partition function ACL shape changed"));
        }
    }
    let expected = expected_funcs
        .iter()
        .flat_map(|(f, roles)| {
            std::iter::once((f.clone(), "owner".into()))
                .chain(roles.iter().map(|r| (f.clone(), r.clone())))
        })
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(refused(
            "partition function grants differ from exact source policy",
        ));
    }
    let definitions = list(&snapshot["structural"], "structural_lines/functions")?;
    if definitions.len() != expected_funcs.len()
        || definitions.iter().any(|row| {
            !p["functions"].as_array().is_some_and(|all| {
                all.iter().any(|f| {
                    f["name"] == format!("partition_ops.{}", row["name"].as_str().unwrap_or(""))
                        && f["identity_arguments"] == row["identity_args"]
                        && f["security_definer"] == row["security_definer"]
                })
            })
        })
    {
        return Err(refused(
            "partition routine security/overload contract changed",
        ));
    }
    let tables = list(acl, "acl_lines/acls")?;
    if tables.len() != expected_tables.len()
        || tables.iter().any(|r| {
            r["grantee"] != "owner"
                || r["grant_option"] != false
                || r["privileges"] != "DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE"
        })
        || tables
            .iter()
            .map(|r| r["name"].as_str().unwrap_or("").to_owned())
            .collect::<BTreeSet<_>>()
            != expected_tables
    {
        return Err(refused("private partition table ACL changed"));
    }
    let schema_owners = list(acl, "acl_lines/schema_owners")?;
    if schema_owners.len() != 1 || schema_owners[0]["owner"] != "owner" {
        return Err(refused("partition schema owner changed"));
    }
    let mut schemas = BTreeSet::new();
    for row in list(acl, "acl_lines/schema_acls")? {
        if row["grant_option"] != false
            || !schemas.insert((
                text(row, "grantee")?.to_owned(),
                text(row, "privileges")?.to_owned(),
            ))
        {
            return Err(refused("partition schema ACL shape changed"));
        }
    }
    let mut expected_schemas = strings(&p["readers"])?
        .into_iter()
        .map(|r| (r, "USAGE".into()))
        .collect::<BTreeSet<_>>();
    expected_schemas.insert(("owner".into(), "CREATE,USAGE".into()));
    if schemas != expected_schemas
        || !list(acl, "column_acl_lines/rows")?.is_empty()
        || !list(acl, "policy_acl_lines/rows")?.is_empty()
        || !list(acl, "acl_lines/type_owners")?.is_empty()
        || !list(acl, "acl_lines/type_acls")?.is_empty()
    {
        return Err(refused("partition schema/type/column privileges changed"));
    }
    let families = list(&snapshot["reference"], "families")?;
    if families.len() != 22
        || families
            .iter()
            .map(|r| r["family"].as_str().unwrap_or("").to_owned())
            .collect::<BTreeSet<_>>()
            != strings(&p["families"])?
    {
        return Err(refused("all22 partition families are mandatory"));
    }
    Ok(())
}
pub(crate) fn verify_value(value: &Value) -> Result<()> {
    if value.as_object().is_none_or(|v| {
        v.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != BTreeSet::from([
                "schema",
                "source_policy_sha256",
                "catalog_sha256",
                "catalog",
            ])
    }) || value["schema"] != "agentforge/partition-authority-measurement/v1"
        || value["source_policy_sha256"] != safe::sha256_bytes(POLICY)
        || value["catalog_sha256"] != digest(&value["catalog"])?
    {
        return Err(refused("supplemental partition catalog evidence changed"));
    }
    check_policy(&value["catalog"])
}
// The supplemental schema has its own source-owned reader. Do not extend or
// rebind the retired baseline producer: its exact source identity is retained in
// the original producer descriptor and in the frozen018..020 receipts.
async fn catalog_rows(conn: &mut PgConnection, key: &str, prefix: &str) -> Result<Vec<Value>> {
    let queries: Value =
        serde_json::from_str(include_str!("../../fingerprint/catalog_queries.json"))
            .map_err(|_| refused("invalid bundled partition catalog queries"))?;
    let query = queries[key]
        .as_str()
        .ok_or_else(|| refused("missing partition catalog query"))?;
    let sql = format!("SELECT to_jsonb(fingerprint_row) FROM ({query}) AS fingerprint_row");
    let mut q = sqlx::query_scalar::<_, Value>(&sql).bind(vec!["partition_ops"]);
    if key == "acl_lines/default_privileges" {
        q = q.bind(crate::fingerprint::like_prefix_pattern(prefix));
    }
    let mut result = q.fetch_all(conn).await?;
    // Keep PostgreSQL internal char values in the same explicit byte encoding
    // used by the catalog snapshot. No historical fingerprint entry point runs.
    let fields: &[&str] = match key {
        "structural_lines/relations" => &["kind", "persistence", "replica_identity"],
        "structural_lines/columns" => &["identity", "generated", "storage", "compression"],
        "structural_lines/constraints" => &["type"],
        "structural_lines/types" | "acl_lines/type_owners" | "acl_lines/type_acls" => &["type"],
        "structural_catalog_detail_lines/views" => &["kind"],
        "structural_catalog_detail_lines/triggers" => &["enabled"],
        "structural_catalog_detail_lines/aggregates" => {
            &["aggkind", "aggfinalmodify", "aggmfinalmodify"]
        }
        "policy_acl_lines/rows" => &["command"],
        _ => &[],
    };
    for row in &mut result {
        for field in fields {
            if let Some(value) = row[*field].as_str() {
                row[*field] = json!(catalog_byte_repr(value));
            }
        }
    }
    Ok(result)
}
fn catalog_byte_repr(value: &str) -> String {
    let byte = value.as_bytes().first().copied().unwrap_or(0);
    match byte {
        0 => "b'\\x00'".into(),
        b'\'' => "b\"'\"".into(),
        b'\\' => "b'\\\\'".into(),
        32..=126 => format!("b'{}'", byte as char),
        _ => format!("b'\\x{byte:02x}'"),
    }
}
pub(crate) async fn capture(conn: &mut PgConnection, prefix: &str) -> Result<Value> {
    let mut structural = serde_json::Map::new();
    let mut acl = serde_json::Map::new();
    for key in [
        "structural_lines/schemas",
        "structural_lines/relations",
        "structural_lines/columns",
        "structural_lines/constraints",
        "structural_lines/indexes",
        "structural_lines/sequences",
        "structural_lines/functions",
        "structural_lines/types",
        "structural_catalog_detail_lines/views",
        "structural_catalog_detail_lines/triggers",
        "structural_catalog_detail_lines/aggregates",
        "acl_lines/owners",
        "acl_lines/schema_owners",
        "acl_lines/function_owners",
        "acl_lines/type_owners",
        "acl_lines/acls",
        "column_acl_lines/rows",
        "acl_lines/function_acls",
        "acl_lines/schema_acls",
        "acl_lines/type_acls",
        "policy_acl_lines/rows",
        "acl_lines/default_privileges",
    ] {
        let mut rows = catalog_rows(conn, key, prefix).await?;
        for row in &mut rows {
            for field in ["owner", "grantee", "grantor", "grantor_default"] {
                if let Some(v) = row[field].as_str() {
                    let normalized = v
                        .strip_prefix(prefix)
                        .filter(|s| credentials::ALL_PRINCIPALS.contains(s) || *s == "owner")
                        .unwrap_or(v)
                        .to_owned();
                    row[field] = json!(normalized);
                }
            }
        }
        if key.starts_with("structural") {
            structural.insert(key.into(), json!(rows));
        } else {
            acl.insert(key.into(), json!(rows));
        }
    }
    let families:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('family',family,'clock_column',clock_column,'cadence',cadence,'safe_retention_predicate',safe_retention_predicate) FROM partition_ops.families ORDER BY family").fetch_all(&mut *conn).await?;
    let policies: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM partition_ops.statement_policies p ORDER BY family,statement_tail",
    )
    .fetch_all(&mut *conn)
    .await?;
    let catalog = json!({"structural":structural,"acl":acl,"reference":{"families":families,"statement_policies":policies}});
    let value = json!({"schema":"agentforge/partition-authority-measurement/v1","source_policy_sha256":safe::sha256_bytes(POLICY),"catalog_sha256":digest(&catalog)?,"catalog":catalog});
    verify_value(&value)?;
    Ok(value)
}
/// Called only for the exact installed fixed035 prefix; absent on every historical
/// evidence record, preserving all original four-class hashes and receipts.
pub(crate) async fn capture_for_prefix(
    conn: &mut PgConnection,
    prefix: &str,
    applied: &BTreeMap<i32, String>,
) -> Result<Option<Value>> {
    if applied.last_key_value().is_none_or(|(n, _)| *n < 35) {
        return Ok(None);
    }
    if applied.get(&35) != Some(&step(&catalog()?, 35)?.change.sha256) {
        return Err(refused(
            "partition extension requires exact035 source checksum",
        ));
    }
    let mut tx = conn.begin().await?;
    crate::runner::set_role(&mut tx, &format!("{prefix}owner")).await?;
    let value = capture(&mut tx, prefix).await;
    tx.rollback().await?;
    value.map(Some)
}
pub(crate) async fn require_for_ledger(
    authority: &crate::runner::MigrationAuthority,
    conn: &mut PgConnection,
    applied: &BTreeMap<i32, String>,
) -> Result<()> {
    let Some(actual) = capture_for_prefix(conn, &authority.role_prefix, applied).await? else {
        return Ok(());
    };
    let n = *applied
        .last_key_value()
        .ok_or_else(|| refused("partition ledger missing"))?
        .0;
    let value: Value = serde_json::from_slice(&fs::read(
        authority.paths.epoch_dir(BASELINE).join(reference_name(n)),
    )?)
    .map_err(|_| refused("partition reference malformed"))?;
    if value["supplemental_partition_catalog"] != actual {
        return Err(refused(
            "supplemental partition catalog drift; operator maintenance needs its separate reviewed authority stage",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Pure policy fixtures exercise set/role rejection only, never produce an
    // authority fingerprint, measurement receipt or claimed live acceptance.
    fn fixture() -> Value {
        let p = policy().unwrap();
        let mut owners = Vec::new();
        let mut funcs = Vec::new();
        let mut definitions = Vec::new();
        for f in p["functions"].as_array().unwrap() {
            let name = f["name"]
                .as_str()
                .unwrap()
                .strip_prefix("partition_ops.")
                .unwrap();
            let args = f["identity_arguments"].as_str().unwrap();
            owners.push(json!({"name":name,"identity_args":args,"owner":"owner"}));
            definitions.push(
                json!({"name":name,"identity_args":args,"security_definer":f["security_definer"]}),
            );
            for role in std::iter::once("owner").chain(
                f["roles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap()),
            ) {
                funcs.push(json!({"name":name,"identity_args":args,"grantee":role,"privileges":"EXECUTE","grant_option":false}));
            }
        }
        let tables = p["tables"].as_array().unwrap();
        let schema_acls = std::iter::once(
            json!({"grantee":"owner","privileges":"CREATE,USAGE","grant_option":false}),
        )
        .chain(
            p["readers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| json!({"grantee":r,"privileges":"USAGE","grant_option":false})),
        )
        .collect::<Vec<_>>();
        json!({"structural":{"structural_lines/functions":definitions},"acl":{"acl_lines/owners":tables.iter().map(|t|json!({"name":t,"owner":"owner"})).collect::<Vec<_>>(),"acl_lines/acls":tables.iter().map(|t|json!({"name":t,"grantee":"owner","privileges":"DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE","grant_option":false})).collect::<Vec<_>>(),"acl_lines/function_owners":owners,"acl_lines/function_acls":funcs,"acl_lines/schema_owners":[{"owner":"owner"}],"acl_lines/schema_acls":schema_acls,"column_acl_lines/rows":[],"policy_acl_lines/rows":[],"acl_lines/type_owners":[],"acl_lines/type_acls":[]},"reference":{"families":p["families"].as_array().unwrap().iter().map(|f|json!({"family":f})).collect::<Vec<_>>()}})
    }
    #[test]
    fn exact_partition_acl_refuses_public_directory_ddl_and_security_escalation() {
        let original = fixture();
        check_policy(&original).unwrap();
        let mut bad = original.clone();
        bad["acl"]["acl_lines/function_acls"].as_array_mut().unwrap().push(json!({"name":"expand","identity_args":"p_family text, p_boundary timestamp with time zone, p_admission_sha256 text","grantee":"gateway","privileges":"EXECUTE","grant_option":false}));
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        bad["acl"]["acl_lines/acls"][0]["grantee"] = json!("knowledge_worker");
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        bad["acl"]["acl_lines/function_acls"][0]["grant_option"] = json!(true);
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        bad["acl"]["acl_lines/schema_acls"][1]["privileges"] = json!("CREATE,USAGE");
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        bad["structural"]["structural_lines/functions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["name"] == "write_json")
            .unwrap()["security_definer"] = json!(true);
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        bad["reference"]["families"].as_array_mut().unwrap().pop();
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        bad["acl"]["acl_lines/function_acls"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        assert!(check_policy(&bad).is_err());
        let mut bad = original.clone();
        let extra = bad["acl"]["acl_lines/function_acls"][0].clone();
        bad["acl"]["acl_lines/function_acls"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        assert!(check_policy(&bad).is_err());
    }
    #[test]
    fn supplemental_measurement_is_required_and_cannot_be_a_fake_hash_or_unknown_receipt() {
        assert!(verify_value(&json!({"schema":"agentforge/partition-authority-measurement/v1","catalog_sha256":"f".repeat(64),"catalog":fixture()})).is_err());
        let snapshot = fixture();
        let mut v = json!({"schema":"agentforge/partition-authority-measurement/v1","source_policy_sha256":safe::sha256_bytes(POLICY),"catalog_sha256":digest(&snapshot).unwrap(),"catalog":snapshot});
        assert!(verify_value(&v).is_ok());
        v["fabricated_live_acceptance"] = json!(true);
        assert!(verify_value(&v).is_err());
    }
}
