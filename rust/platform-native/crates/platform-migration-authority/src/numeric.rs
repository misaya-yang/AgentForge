//! Read-only reconciliation of ambiguous immutable legacy numeric identities.
//! No ledger updates, administrator-price comparisons, or automatic replay.
use crate::{AuthorityError, Result, manifest::LegacyManifest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
#[path = "numeric_sql.rs"]
mod sql_evidence;
pub const HISTORICAL: &[(&str, &[&str])] = &[
    (
        "016",
        &[
            "016_confluence_multi_root_pages.sql",
            "016_usage_hourly_aggregates.sql",
        ],
    ),
    (
        "030",
        &[
            "030_fix_timestamp_and_security_constraint.sql",
            "030_fix_timestamp_and_security_constraint_rollback.sql",
        ],
    ),
    (
        "031",
        &[
            "031_align_model_prices_20260211.sql",
            "031_hierarchical_segments.sql",
        ],
    ),
];
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NumericLedgerRecord {
    pub version: String,
    pub name: Option<String>,
    pub checksum: Option<String>,
    pub dirty: bool,
}
fn description(filename: &str) -> String {
    let Some(name) = filename.get(4..filename.len().saturating_sub(4)) else {
        return String::new();
    };
    let mut out = String::new();
    let mut previous_cased = false;
    for c in name.replace('_', " ").chars() {
        if c.is_alphabetic() {
            if previous_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            previous_cased = true;
        } else {
            out.push(c);
            previous_cased = false;
        }
    }
    out
}
fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() && c as u32 <= 255 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
fn checksum_shape(s: &str) -> bool {
    matches!(s.len(), 16 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn alias(file: &str) -> Option<String> {
    match file {
        "089_agent_runtime_thread_store.sql"
        | "090_agent_runtime_model_leases.sql"
        | "092_agent_runtime_legacy_import.sql"
        | "093_agent_runtime_assistant_session_fks.sql"
        | "094_agent_runtime_legacy_import_normalization.sql" => {
            Some(file.replacen("_agent_runtime_", "_codex_runtime_", 1))
        }
        _ => None,
    }
}
fn block(receipt: &mut Value, version: &str, reason: impl Into<String>) {
    receipt["verdict"] = json!("blocked");
    receipt["versions"][version]["verdict"] = json!("blocked");
    receipt["versions"][version]["reason"] = json!(reason.into());
}
type Identities = BTreeMap<String, (String, String)>;
/// Return identity and evidence basis, or the exact fail-closed policy reason.
pub fn match_numeric_identity(
    record: &NumericLedgerRecord,
    identities: &Identities,
    ordered: &[&str],
) -> std::result::Result<(String, &'static str), String> {
    let name = record.name.as_deref().filter(|s| !s.is_empty());
    let checksum = record.checksum.as_deref().filter(|s| !s.is_empty());
    let names = if let Some(name) = name {
        let values: BTreeSet<String> = identities
            .keys()
            .filter(|f| name == f.as_str() || name == description(f))
            .cloned()
            .collect();
        if values.is_empty() {
            return Err(format!("unknown legacy migration name {}", repr(name)));
        }
        Some(values)
    } else {
        None
    };
    let hashes = if let Some(checksum) = checksum {
        if !checksum_shape(checksum) {
            return Err(format!("invalid legacy checksum shape {}", repr(checksum)));
        }
        let values: BTreeSet<String> = identities
            .iter()
            .filter(|(_, v)| checksum == v.0 || checksum == v.1)
            .map(|(k, _)| k.clone())
            .collect();
        if values.is_empty() {
            return Err(format!(
                "checksum {} matches no immutable legacy file",
                repr(checksum)
            ));
        }
        Some(values)
    } else {
        None
    };
    let matches = match (&names, &hashes) {
        (None, None) => {
            return Err("bare numeric row has neither a migration name nor a known checksum".into());
        }
        (Some(n), None) => n.clone(),
        (None, Some(h)) => h.clone(),
        (Some(n), Some(h)) => {
            let intersection: BTreeSet<String> = n.intersection(h).cloned().collect();
            if intersection.is_empty() {
                if !ordered.is_empty()
                    && *n == BTreeSet::from([ordered[0].to_owned()])
                    && *h == BTreeSet::from([ordered[ordered.len() - 1].to_owned()])
                {
                    return Ok((
                        ordered[ordered.len() - 1].into(),
                        "historical_name_checksum_overwrite",
                    ));
                }
                return Err(
                    "legacy migration name and checksum identify an unknown mixed state".into(),
                );
            }
            intersection
        }
    };
    if matches.len() != 1 {
        return Err(format!(
            "legacy identity is ambiguous across [{}]",
            matches
                .iter()
                .map(|s| repr(s))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok((
        matches.into_iter().next().unwrap(),
        "exact_name_or_checksum",
    ))
}
async fn ledger_records(conn: &mut PgConnection) -> Result<Vec<NumericLedgerRecord>> {
    let columns:Vec<String>=sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'schema_migrations'").fetch_all(&mut *conn).await?;
    let names: BTreeSet<_> = columns.into_iter().collect();
    if !names.contains("version") {
        return Ok(vec![]);
    }
    let name = if names.contains("name") {
        "name::text"
    } else {
        "NULL::text"
    };
    let checksum = if names.contains("checksum") {
        "checksum::text"
    } else {
        "NULL::text"
    };
    let dirty = if names.contains("dirty") {
        "dirty"
    } else {
        "FALSE"
    };
    let sql = format!(
        "SELECT version::text AS version, {name} AS name, {checksum} AS checksum, {dirty} AS dirty FROM public.schema_migrations ORDER BY version::integer"
    );
    let rows = sqlx::query(&sql).fetch_all(&mut *conn).await?;
    let mut records = vec![];
    for row in rows {
        let raw: Option<String> = row.try_get("version")?;
        let version = raw
            .as_deref()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .ok_or_else(|| {
                AuthorityError::Blocked(format!(
                    "numeric legacy ledger contains invalid version {}",
                    raw.as_deref().map(repr).unwrap_or("None".into())
                ))
            })?;
        records.push(NumericLedgerRecord {
            version: format!("{version:03}"),
            name: row
                .try_get::<Option<String>, _>("name")?
                .map(|s| s.trim().to_owned()),
            checksum: row
                .try_get::<Option<String>, _>("checksum")?
                .map(|s| s.trim().to_lowercase()),
            dirty: row.try_get::<Option<bool>, _>("dirty")?.unwrap_or(false),
        });
    }
    Ok(records)
}
// Python asyncpg exception names retained in evidence instead of driver messages.
fn error_code(error: &sqlx::Error) -> &'static str {
    match error.as_database_error().and_then(|e| e.code()).as_deref() {
        Some("42P01") => "UndefinedTableError",
        Some("42703") => "UndefinedColumnError",
        Some("21000") => "CardinalityViolationError",
        Some("22P02") => "InvalidTextRepresentationError",
        Some("42883") => "UndefinedFunctionError",
        Some("42501") => "InsufficientPrivilegeError",
        Some("57014") => "QueryCanceledError",
        Some("25P02") => "InFailedSQLTransactionError",
        Some("42601") => "PostgresSyntaxError",
        Some(_) => "PostgresError",
        None => match error {
            sqlx::Error::PoolTimedOut => "TimeoutError",
            _ => "InterfaceError",
        },
    }
}
async fn evidence(conn: &mut PgConnection, key: &str) -> Value {
    let Some(sql) = sql_evidence::query(key) else {
        return json!({"check":key,"passed":false,"error_code":"KeyError"});
    };
    match sqlx::query_scalar::<_, Option<bool>>(sql)
        .fetch_one(&mut *conn)
        .await
    {
        Ok(value) => json!({"check":key,"passed":value.unwrap_or(false)}),
        Err(e) => json!({"check":key,"passed":false,"error_code":error_code(&e)}),
    }
}
fn ledger(record: &NumericLedgerRecord) -> Value {
    json!({"name":record.name,"checksum":record.checksum,"dirty":record.dirty})
}
/// Identity phase is pure, enabling adversarial fixtures without database mutation.
fn identify(
    records: Vec<NumericLedgerRecord>,
    manifest: &LegacyManifest,
) -> (Value, Vec<(String, String)>) {
    let mut receipt = json!({"ledger_kind":"numeric","verdict":"proven","versions":{},"notes":[]});
    let forward: BTreeMap<_, _> = manifest
        .changes
        .iter()
        .map(|s| (s.file.clone(), s))
        .collect();
    let mut identities: Identities = manifest
        .changes
        .iter()
        .map(|s| {
            (
                s.file.clone(),
                (s.sha256.clone(), s.legacy_checksum().to_owned()),
            )
        })
        .collect();
    identities.extend(manifest.historical_rollbacks.iter().map(|s| {
        (
            s.file.clone(),
            (s.sha256.clone(), s.sha256.chars().take(16).collect()),
        )
    }));
    let mut grouped: BTreeMap<String, Vec<NumericLedgerRecord>> = BTreeMap::new();
    for r in records {
        grouped.entry(r.version.clone()).or_default().push(r);
    }
    let mut by_version: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in forward.keys() {
        by_version
            .entry(file.chars().take(3).collect())
            .or_default()
            .push(file.clone());
    }
    for (version, records) in &grouped {
        if HISTORICAL.iter().any(|(v, _)| v == version) {
            continue;
        }
        receipt["versions"][version] = json!({});
        if records.len() != 1 {
            block(
                &mut receipt,
                version,
                format!("numeric ledger has {} rows for one version", records.len()),
            );
            continue;
        }
        let r = &records[0];
        receipt["versions"][version]["ledger"] = ledger(r);
        if r.dirty {
            block(&mut receipt, version, "numeric ledger row is dirty");
            continue;
        }
        let Some(files) = by_version.get(version).filter(|v| v.len() == 1) else {
            block(
                &mut receipt,
                version,
                "numeric ledger version has no unique immutable forward file",
            );
            continue;
        };
        let file = &files[0];
        let spec = forward[file];
        let mut accepted = BTreeSet::from([file.clone(), description(file)]);
        if let Some(a) = alias(file) {
            accepted.insert(description(&a));
            accepted.insert(a);
        }
        if let Some(name) = r.name.as_deref().filter(|s| !s.is_empty()) {
            if !accepted.contains(name) {
                block(
                    &mut receipt,
                    version,
                    format!("ledger name {} does not identify {file}", repr(name)),
                );
                continue;
            }
        }
        if let Some(sum) = r.checksum.as_deref().filter(|s| !s.is_empty()) {
            if !checksum_shape(sum) {
                block(
                    &mut receipt,
                    version,
                    format!("invalid legacy checksum shape {}", repr(sum)),
                );
                continue;
            }
            if sum != spec.sha256 && sum != spec.legacy_checksum() {
                block(
                    &mut receipt,
                    version,
                    format!("checksum {} does not match immutable {file}", repr(sum)),
                );
                continue;
            }
        }
        receipt["versions"][version]["identified_file"] = json!(file);
        receipt["versions"][version]["identity_basis"] =
            json!(if r.name.as_deref().unwrap_or("").is_empty()
                && r.checksum.as_deref().unwrap_or("").is_empty()
            {
                "unique_version_only"
            } else {
                "unique_version_with_matching_identity"
            });
        receipt["versions"][version]["verdict"] = json!("proven");
    }
    let mut checks = vec![];
    for &(version, ordered) in HISTORICAL {
        receipt["versions"][version] = json!({"ordered_historical_identities":ordered});
        let records = grouped.get(version).map(Vec::as_slice).unwrap_or_default();
        if records.is_empty() {
            if version == "031" {
                block(
                    &mut receipt,
                    version,
                    "version absent: the data-only administrator price migration cannot be proven and will not be replayed automatically",
                );
            } else {
                receipt["versions"][version]["verdict"] = json!("not_applied");
                receipt["versions"][version]["reason"] =
                    json!("version absent; immutable forward file(s) remain pending");
            }
            continue;
        }
        if records.len() != 1 {
            block(
                &mut receipt,
                version,
                format!("numeric ledger has {} rows for one version", records.len()),
            );
            continue;
        }
        let r = &records[0];
        receipt["versions"][version]["ledger"] = ledger(r);
        if r.dirty {
            block(&mut receipt, version, "numeric ledger row is dirty");
            continue;
        }
        let candidates = identities
            .iter()
            .filter(|(k, _)| ordered.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let (identity, basis) = match match_numeric_identity(r, &candidates, ordered) {
            Ok(v) => v,
            Err(reason) => {
                block(&mut receipt, version, reason);
                continue;
            }
        };
        receipt["versions"][version]["identified_file"] = json!(identity);
        receipt["versions"][version]["identity_basis"] = json!(basis);
        if version == "016" && identity != ordered[ordered.len() - 1] {
            block(
                &mut receipt,
                version,
                format!(
                    "ledger identifies {identity}; later sibling {} is not proven",
                    ordered[ordered.len() - 1]
                ),
            );
            continue;
        }
        if version == "031" && identity != ordered[ordered.len() - 1] {
            block(
                &mut receipt,
                version,
                "data-only price migration is not followed by a proven hierarchical sibling; administrator prices will not be replayed",
            );
            continue;
        }
        checks.push((version.into(), identity));
    }
    (receipt, checks)
}
fn apply_evidence(receipt: &mut Value, version: &str, identity: &str, evidence: Vec<Value>) {
    let passed = evidence.first().is_some_and(|v| v["passed"] == true);
    let rollback = evidence.get(1).is_some_and(|v| v["passed"] == true);
    receipt["versions"][version]["evidence"] = json!(evidence);
    let reason = match version {
        "016" => {
            if !passed {
                block(
                    receipt,
                    version,
                    "016 object/index/backfill evidence is incomplete",
                );
                return;
            }
            "last sibling identity plus complete effective-state evidence"
        }
        "030" => {
            let ordered = HISTORICAL[1].1;
            if identity == ordered[1] {
                block(
                    receipt,
                    version,
                    "historical rollback execution is proven; forward state must not be guessed or replayed",
                );
                return;
            }
            if identity != ordered[0] {
                block(
                    receipt,
                    version,
                    format!("unexpected 030 identity {identity}"),
                );
                return;
            }
            if !passed || rollback {
                block(
                    receipt,
                    version,
                    "030 schema/data evidence is rollback, mixed, or otherwise unproven",
                );
                return;
            }
            "forward identity and complete post-forward state agree"
        }
        "031" => {
            if !passed {
                block(
                    receipt,
                    version,
                    "031 hierarchy object/constraint/data evidence is incomplete",
                );
                return;
            }
            "last sibling ledger identity proves the old runner passed the earlier data-only change; mutable administrator prices are intentionally neither compared nor replayed"
        }
        _ => return,
    };
    receipt["versions"][version]["verdict"] = json!("proven");
    receipt["versions"][version]["reason"] = json!(reason);
}
pub async fn reconcile_numeric_legacy_history(
    conn: &mut PgConnection,
    manifest: &LegacyManifest,
) -> Result<Value> {
    let records = ledger_records(conn).await?;
    let (mut receipt, checks) = identify(records, manifest);
    for (version, identity) in checks {
        let keys: &[&str] = match version.as_str() {
            "016" => &["016_effective"],
            "030" => &["030_forward", "030_rollback"],
            "031" => &["031_hierarchy_effective"],
            _ => &[],
        };
        let mut observations = vec![];
        for key in keys {
            observations.push(evidence(conn, key).await);
        }
        apply_evidence(&mut receipt, &version, &identity, observations);
    }
    if receipt["verdict"] == "proven" {
        receipt["notes"] = json!([
            "numeric duplicate history is safe to continue; this receipt never mutates a legacy ledger"
        ]);
        Ok(receipt)
    } else {
        Err(AuthorityError::Blocked(
            serde_json::to_string_pretty(&receipt).expect("receipt JSON"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{LegacyChangeSpec, LegacyRollbackSpec, RollbackClass, TransactionMode};
    fn manifest() -> LegacyManifest {
        let mut changes = vec![];
        let mut historical_rollbacks = vec![];
        for (index, file) in HISTORICAL
            .iter()
            .flat_map(|(_, files)| files.iter())
            .chain(["089_agent_runtime_thread_store.sql"].iter())
            .enumerate()
        {
            let sha256 = format!("{index:064x}");
            if file.ends_with("_rollback.sql") {
                historical_rollbacks.push(LegacyRollbackSpec {
                    file: (*file).into(),
                    sha256,
                });
            } else {
                changes.push(LegacyChangeSpec {
                    file: (*file).into(),
                    sha256,
                    transaction_mode: TransactionMode::Transactional,
                    rollback_class: RollbackClass::ForwardFixOnly,
                });
            }
        }
        LegacyManifest {
            schema: "migration-authority/legacy-manifest/v1".into(),
            freeze_point: "089_agent_runtime_thread_store.sql".into(),
            changes,
            historical_rollbacks,
        }
    }
    fn record(version: &str, name: Option<&str>, checksum: Option<&str>) -> NumericLedgerRecord {
        NumericLedgerRecord {
            version: version.into(),
            name: name.map(str::to_owned),
            checksum: checksum.map(str::to_owned),
            dirty: false,
        }
    }
    fn ids() -> Identities {
        BTreeMap::from([
            (
                "016_confluence_multi_root_pages.sql".into(),
                ("a".repeat(64), "a".repeat(16)),
            ),
            (
                "016_usage_hourly_aggregates.sql".into(),
                ("b".repeat(64), "b".repeat(16)),
            ),
        ])
    }
    #[test]
    fn ordered_overwrite_is_only_accepted_mixed_identity() {
        let ordered = HISTORICAL[0].1;
        let r = record("016", Some(ordered[0]), Some(&"b".repeat(16)));
        assert_eq!(
            match_numeric_identity(&r, &ids(), ordered).unwrap(),
            (ordered[1].into(), "historical_name_checksum_overwrite")
        );
        let r = record("016", Some(ordered[1]), Some(&"a".repeat(16)));
        assert!(match_numeric_identity(&r, &ids(), ordered).is_err());
    }
    #[test]
    fn bare_ambiguous_unknown_and_invalid_checksum_block() {
        for r in [
            record("016", None, None),
            record("016", Some("unknown"), None),
            record("016", None, Some("not a hash")),
            record("016", None, Some(&"c".repeat(64))),
        ] {
            assert!(match_numeric_identity(&r, &ids(), HISTORICAL[0].1).is_err());
        }
    }
    #[test]
    fn title_description_matches_python() {
        assert_eq!(
            description("031_align_model_prices_20260211.sql"),
            "Align Model Prices 20260211"
        );
        assert_eq!(description("001_http2_api.sql"), "Http2 Api");
        let r = record("016", Some("Usage Hourly Aggregates"), None);
        assert_eq!(
            match_numeric_identity(&r, &ids(), HISTORICAL[0].1)
                .unwrap()
                .0,
            HISTORICAL[0].1[1]
        );
    }
    #[test]
    fn missing_price_migration_is_never_replayed() {
        let (r, checks) = identify(vec![], &manifest());
        assert_eq!(r["verdict"], "blocked");
        assert_eq!(r["versions"]["016"]["verdict"], "not_applied");
        assert_eq!(r["versions"]["030"]["verdict"], "not_applied");
        assert_eq!(r["versions"]["031"]["verdict"], "blocked");
        assert!(checks.is_empty());
    }
    #[test]
    fn unique_alias_and_version_only_are_supported() {
        for name in [
            None,
            Some("089_codex_runtime_thread_store.sql"),
            Some("Codex Runtime Thread Store"),
        ] {
            let (r, _) = identify(vec![record("089", name, None)], &manifest());
            assert_eq!(r["versions"]["089"]["verdict"], "proven");
        }
    }
    #[test]
    fn dirty_duplicate_and_unknown_rows_block() {
        let mut dirty = record("089", None, None);
        dirty.dirty = true;
        for records in [
            vec![dirty],
            vec![record("089", None, None), record("089", None, None)],
            vec![record("999", None, None)],
        ] {
            let (r, _) = identify(records, &manifest());
            assert_eq!(r["verdict"], "blocked");
            assert!(r["versions"].as_object().unwrap().values().any(|v| {
                v["reason"].as_str().is_some_and(|s| {
                    s.contains("dirty") || s.contains("2 rows") || s.contains("no unique")
                })
            }));
        }
    }
    #[test]
    fn rollback_and_mixed_effective_state_block() {
        for (identity, forward, rollback) in [
            (HISTORICAL[1].1[1], true, false),
            (HISTORICAL[1].1[0], true, true),
            (HISTORICAL[1].1[0], false, false),
        ] {
            let mut r = json!({"verdict":"proven","versions":{"030":{}}});
            apply_evidence(
                &mut r,
                "030",
                identity,
                vec![json!({"passed":forward}), json!({"passed":rollback})],
            );
            assert_eq!(r["verdict"], "blocked");
        }
    }
    #[test]
    fn complete_effective_state_proves_selected_identity() {
        let mut r = json!({"verdict":"proven","versions":{"016":{},"030":{},"031":{}}});
        for &(version, ordered) in HISTORICAL {
            let identity = if version == "030" {
                ordered[0]
            } else {
                ordered[1]
            };
            let mut checks = vec![json!({"passed":true})];
            if version == "030" {
                checks.push(json!({"passed":false}));
            }
            apply_evidence(&mut r, version, identity, checks);
            assert_eq!(r["versions"][version]["verdict"], "proven");
        }
        assert_eq!(r["verdict"], "proven");
    }
    #[test]
    fn all_sql_matches_immutable_source_digests() {
        use sha2::{Digest, Sha256};
        // Golden SHA-256 values captured from the retained source specification
        // before removal; tests do not depend on a Python source or interpreter.
        for (key, expected) in [
            (
                "016_effective",
                "6714b9f90a0a09a35544c40444ff1635b9fc6a3a1c5555dc9b1e60b0450127db",
            ),
            (
                "030_forward",
                "d3e1501b4c9069dc48aed3bbcea236b95aba4654bbf4b1b97990a2623b562005",
            ),
            (
                "030_rollback",
                "bcd8c4f6eb36be4df27012fc3090074400703bc92876b454cd208cf76657662c",
            ),
            (
                "031_hierarchy_effective",
                "96ad07abc4f8e64b0a34f238f6416f6776318f0b3a90800a21814785330788ee",
            ),
        ] {
            assert_eq!(
                format!(
                    "{:x}",
                    Sha256::digest(sql_evidence::query(key).unwrap().as_bytes())
                ),
                expected
            );
        }
    }
}
