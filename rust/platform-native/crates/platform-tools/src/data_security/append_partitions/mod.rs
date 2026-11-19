//! Native, offline authority operation plans. Every mutation is an installed,
//! owner-only SQL function or an explicitly nontransactional DETACH/index step.
//! This crate never bypasses the migration authority or opens a database itself.
pub mod catalog;
use super::{Result, ensure};
use catalog::{Cadence, Family};
use chrono::{DateTime, Datelike, Duration, Months, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: String,
    pub authority_snapshot_sha256: String,
    pub catalog_sha256: String,
    pub catalog: Value,
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "input unavailable")?;
    ensure(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= limit,
        "bounded regular input required",
    )?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|_| "input unavailable")?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input unreadable")?;
    ensure(bytes.len() as u64 <= limit, "input budget exceeded")?;
    Ok(bytes)
}
pub fn read_snapshot(path: &Path) -> Result<Snapshot> {
    let snapshot: Snapshot = serde_json::from_slice(&read_bounded(path, 4 * 1024 * 1024)?)
        .map_err(|_| "invalid authority snapshot")?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}
pub fn validate_snapshot(snapshot: &Snapshot) -> Result<Family> {
    ensure(
        snapshot.schema == "agentforge-append-catalog/v1"
            && digest(&snapshot.authority_snapshot_sha256),
        "authority snapshot identity required",
    )?;
    ensure(
        digest(&snapshot.catalog_sha256)
            && snapshot.catalog_sha256
                == sha256(
                    &serde_json::to_vec(&snapshot.catalog)
                        .map_err(|_| "catalog encoding failed")?,
                ),
        "catalog checksum mismatch",
    )?;
    let name = snapshot.catalog["family"]
        .as_str()
        .ok_or("catalog family missing")?;
    let spec = catalog::family(name)?;
    ensure(
        snapshot.catalog["clock_column"].as_str() == Some(spec.clock),
        "catalog clock mismatch",
    )?;
    ensure(
        snapshot.catalog["cadence"].as_str()
            == Some(match spec.cadence {
                Cadence::Day => "day",
                Cadence::Month => "month",
            }),
        "catalog cadence mismatch",
    )?;
    ensure(
        matches!(
            snapshot.catalog["phase"].as_str(),
            Some("registered" | "expanded" | "backfilled" | "validated" | "active")
        ),
        "unknown migration phase",
    )?;
    ensure(
        snapshot.catalog["revision"].as_u64().is_some(),
        "migration revision absent",
    )?;
    Ok(spec)
}
pub fn boundary(family: Family, lower: DateTime<Utc>) -> Result<DateTime<Utc>> {
    ensure(
        lower.hour() == 0 && lower.minute() == 0 && lower.second() == 0 && lower.nanosecond() == 0,
        "UTC midnight required",
    )?;
    match family.cadence {
        Cadence::Day => lower
            .checked_add_signed(Duration::days(1))
            .ok_or_else(|| "date overflow".into()),
        Cadence::Month => {
            ensure(lower.day() == 1, "UTC month start required")?;
            lower
                .checked_add_months(Months::new(1))
                .ok_or_else(|| "date overflow".into())
        }
    }
}
pub fn partition_name(family: Family, lower: DateTime<Utc>) -> Result<String> {
    boundary(family, lower)?;
    Ok(format!(
        "{}_p{}",
        family.name,
        lower.format(match family.cadence {
            Cadence::Day => "%Y%m%d",
            Cadence::Month => "%Y%m",
        })
    ))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceFile {
    pub path: std::path::PathBuf,
    pub sha256: String,
}
pub fn verify_evidence(
    files: &BTreeMap<String, EvidenceFile>,
    required: &[&str],
) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for kind in required {
        let record = files
            .get(*kind)
            .ok_or_else(|| format!("missing {kind} evidence"))?;
        ensure(
            record.path.is_absolute() && digest(&record.sha256),
            "absolute evidence path and digest required",
        )?;
        let bytes = read_bounded(&record.path, 16 * 1024 * 1024)?;
        ensure(
            sha256(&bytes) == record.sha256,
            "evidence artifact checksum mismatch",
        )?;
        result.insert((*kind).to_string(), record.sha256.clone());
    }
    Ok(result)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Expand {
        boundary: DateTime<Utc>,
    },
    Backfill {
        limit: u32,
    },
    ValidateLegacy,
    CaptureForeignKeys,
    RebindForeignKey {
        oid: u32,
        step: String,
    },
    LegacyIndexPlan,
    Prepare {
        first: DateTime<Utc>,
        count: u32,
    },
    Cutover {
        evidence: BTreeMap<String, EvidenceFile>,
    },
    RetentionBegin {
        relation: String,
        cutoff: DateTime<Utc>,
        evidence: BTreeMap<String, EvidenceFile>,
    },
    DetachResume {
        relation: String,
    },
    Archive {
        relation: String,
        receipt: ArchiveReceipt,
        manifest: EvidenceFile,
    },
    DropArchived {
        relation: String,
        archive_manifest_sha256: String,
    },
    Reattach {
        relation: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveReceipt {
    pub family: String,
    pub relation: String,
    pub relation_oid: u32,
    pub rows: u64,
    pub archive_sha256: String,
    pub archive_manifest_sha256: String,
    pub restore_validation_sha256: String,
    pub source_snapshot_sha256: String,
    pub verified: bool,
}
fn partition<'a>(snapshot: &'a Snapshot, name: &str) -> Result<&'a Value> {
    snapshot.catalog["partitions"]
        .as_array()
        .ok_or("partition snapshot absent")?
        .iter()
        .find(|p| p["relation_name"].as_str() == Some(name))
        .ok_or_else(|| "partition not in authority snapshot".into())
}
fn valid_relation(spec: Family, value: &str) -> Result<()> {
    ensure(
        value == format!("{}_af_legacy", spec.name)
            || value
                .strip_prefix(&format!("{}_p", spec.name))
                .is_some_and(|suffix| {
                    suffix.len()
                        == match spec.cadence {
                            Cadence::Day => 8,
                            Cadence::Month => 6,
                        }
                        && suffix.bytes().all(|b| b.is_ascii_digit())
                }),
        "relation outside exact family",
    )
}
fn stamp(value: DateTime<Utc>) -> String {
    literal(&value.to_rfc3339()) + "::timestamptz"
}
fn json_literal(value: &impl Serialize) -> Result<String> {
    Ok(
        literal(&serde_json::to_string(value).map_err(|_| "operation encoding failed")?)
            + "::jsonb",
    )
}
pub fn plan(snapshot: &Snapshot, operation: &Operation, now: DateTime<Utc>) -> Result<Value> {
    let family = validate_snapshot(snapshot)?;
    let name = literal(family.name);
    let revision = snapshot.catalog["revision"].as_u64().unwrap();
    let phase = snapshot.catalog["phase"].as_str().unwrap();
    let mut sql = Vec::<String>::new();
    let mut nontransactional = false;
    if matches!(
        operation,
        Operation::DetachResume { .. }
            | Operation::Archive { .. }
            | Operation::DropArchived { .. }
            | Operation::Reattach { .. }
    ) {
        ensure(
            phase == "active",
            "active family required for retention recovery",
        )?;
    }
    match operation {
        Operation::Expand { boundary: at } => {
            boundary(family, *at)?;
            ensure(
                phase == "registered",
                "already expanded; resume from catalog state",
            )?;
            sql.push(format!(
                "SELECT partition_ops.expand({name},{},{})",
                stamp(*at),
                literal(&snapshot.authority_snapshot_sha256)
            ));
        }
        Operation::Backfill { limit } => {
            ensure(
                (1..=10000).contains(limit) && matches!(phase, "expanded" | "backfilled"),
                "bounded backfill phase required",
            )?;
            sql.push(format!(
                "SELECT partition_ops.backfill({name},{revision},{limit})"
            ));
        }
        Operation::ValidateLegacy => {
            ensure(
                matches!(phase, "backfilled" | "validated"),
                "complete backfill required",
            )?;
            sql.push(format!(
                "SELECT partition_ops.validate_legacy({name},{revision})"
            ));
        }
        Operation::CaptureForeignKeys => {
            ensure(
                matches!(phase, "expanded" | "backfilled" | "validated"),
                "expanded FK phase required",
            )?;
            sql.push(format!("SELECT partition_ops.capture_foreign_keys({name})"));
        }
        Operation::RebindForeignKey { oid, step } => {
            ensure(
                *oid > 0 && matches!(step.as_str(), "expand" | "validate" | "contract"),
                "known FK step required",
            )?;
            ensure(
                snapshot.catalog["foreign_keys"]
                    .as_array()
                    .is_some_and(|keys| {
                        keys.iter().any(|k| {
                            k["original_oid"].as_u64() == Some(*oid as u64)
                                && k["target_family"].as_str() == Some(family.name)
                        })
                    }),
                "FK absent from scoped snapshot",
            )?;
            sql.push(format!(
                "SELECT partition_ops.rebind_fk({oid},{})",
                literal(step)
            ));
        }
        Operation::LegacyIndexPlan => {
            ensure(
                matches!(phase, "expanded" | "backfilled" | "validated"),
                "expanded index phase required",
            )?;
            sql.push(format!("SELECT partition_ops.legacy_index_sql({name})"));
        }
        Operation::Prepare { first, count } => {
            boundary(family, *first)?;
            ensure(
                phase != "registered"
                    && *count >= 1
                    && *count
                        <= match family.cadence {
                            Cadence::Day => 90,
                            Cadence::Month => 24,
                        },
                "bounded partition preparation required",
            )?;
            sql.push(format!(
                "SELECT partition_ops.prepare({name},{},{count})",
                stamp(*first)
            ));
        }
        Operation::Cutover { evidence } => {
            ensure(phase == "validated", "validated family required")?;
            let checked = verify_evidence(
                evidence,
                &[
                    "pitr_restore_drill",
                    "role_rls_acceptance",
                    "n_minus_one_compatibility",
                    "dependency_rebind",
                    "authority_approval",
                ],
            )?;
            let gate =
                sha256(&serde_json::to_vec(&checked).map_err(|_| "evidence encoding failed")?);
            sql.push(format!(
                "SELECT partition_ops.cutover({name},{revision},{})",
                literal(&gate)
            ));
        }
        Operation::RetentionBegin {
            relation,
            cutoff,
            evidence,
        } => {
            ensure(phase == "active", "active partition family required")?;
            valid_relation(family, relation)?;
            let p = partition(snapshot, relation)?;
            ensure(
                p["relation_oid"]
                    .as_u64()
                    .is_some_and(|n| n > 0 && n <= u32::MAX as u64),
                "retention relation OID missing",
            )?;
            ensure(
                p["state"] == "active",
                "partition already sealed; use detach_resume",
            )?;
            let upper: DateTime<Utc> = p["upper_at"]
                .as_str()
                .ok_or("upper bound absent")?
                .parse()
                .map_err(|_| "invalid upper bound")?;
            ensure(
                upper <= *cutoff && upper < now,
                "open or retained partition refused",
            )?;
            let mut checked = verify_evidence(
                evidence,
                &[
                    "pitr_restore_drill",
                    "role_rls_acceptance",
                    "retention_approval",
                ],
            )?;
            checked.insert(
                "authority_snapshot".into(),
                snapshot.authority_snapshot_sha256.clone(),
            );
            checked.insert(
                "compatibility".into(),
                snapshot.catalog["compatibility_sha256"]
                    .as_str()
                    .filter(|s| digest(s))
                    .ok_or("compatibility receipt missing")?
                    .into(),
            );
            let mut guard = serde_json::Map::new();
            for (k, v) in checked {
                guard.insert(format!("{k}_sha256"), Value::String(v));
            }
            guard.insert("family".into(), json!(family.name));
            guard.insert("relation".into(), json!(relation));
            guard.insert("relation_oid".into(), p["relation_oid"].clone());
            sql.push(format!(
                "SELECT partition_ops.retention_begin({name},{},{},{})",
                literal(relation),
                stamp(*cutoff),
                json_literal(&guard)?
            ));
        }
        Operation::DetachResume { relation } => {
            valid_relation(family, relation)?;
            let p = partition(snapshot, relation)?;
            ensure(
                matches!(
                    p["state"].as_str(),
                    Some("detaching" | "detached" | "archived")
                ),
                "retention barrier required",
            )?;
            // Export a fresh owner catalog after every statement. An explicit
            // catalog-observed pending detach chooses FINALIZE. Unknown/missing
            // observation yields only the synchronization query, never a guess.
            if p["state"] == "detaching" && p["attached"].as_bool() == Some(true) {
                match p["detach_pending"].as_bool() {
                    Some(true) => {
                        sql.push(format!(
                            "ALTER TABLE {} DETACH PARTITION {} FINALIZE",
                            family.name, relation
                        ));
                        nontransactional = true;
                    }
                    Some(false) => {
                        sql.push(format!(
                            "ALTER TABLE {} DETACH PARTITION {} CONCURRENTLY",
                            family.name, relation
                        ));
                        nontransactional = true;
                    }
                    None => sql.push(format!(
                        "SELECT partition_ops.detach_status({name},{})",
                        literal(relation)
                    )),
                }
            } else {
                sql.push(format!(
                    "SELECT partition_ops.detach_status({name},{})",
                    literal(relation)
                ));
            }
        }
        Operation::Archive {
            relation,
            receipt,
            manifest,
        } => {
            valid_relation(family, relation)?;
            let p = partition(snapshot, relation)?;
            ensure(
                matches!(p["state"].as_str(), Some("detached" | "archived")),
                "detached archive required",
            )?;
            ensure(
                receipt.verified
                    && receipt.family == family.name
                    && receipt.relation == *relation
                    && p["relation_oid"].as_u64() == Some(receipt.relation_oid as u64)
                    && p["retention_evidence"]["authority_snapshot_sha256"].as_str()
                        == Some(receipt.source_snapshot_sha256.as_str()),
                "archive identity/source mismatch",
            )?;
            ensure(
                [
                    &receipt.archive_sha256,
                    &receipt.archive_manifest_sha256,
                    &receipt.restore_validation_sha256,
                ]
                .iter()
                .all(|v| digest(v)),
                "archive proof digests required",
            )?;
            ensure(
                manifest.sha256 == receipt.archive_manifest_sha256,
                "archive manifest receipt mismatch",
            )?;
            let bytes = read_bounded(&manifest.path, 16 * 1024 * 1024)?;
            ensure(
                sha256(&bytes) == manifest.sha256,
                "archive manifest checksum mismatch",
            )?;
            let archived: ArchiveManifest =
                serde_json::from_slice(&bytes).map_err(|_| "archive manifest invalid")?;
            archived.verify(receipt)?;
            sql.push(format!(
                "SELECT partition_ops.record_archive({name},{},{})",
                literal(relation),
                json_literal(receipt)?
            ));
        }
        Operation::DropArchived {
            relation,
            archive_manifest_sha256,
        } => {
            valid_relation(family, relation)?;
            let p = partition(snapshot, relation)?;
            ensure(
                p["state"] == "archived"
                    && p["archive_receipt"]["archive_manifest_sha256"].as_str()
                        == Some(archive_manifest_sha256.as_str())
                    && digest(archive_manifest_sha256),
                "exact verified archive required",
            )?;
            sql.push(format!(
                "SELECT partition_ops.drop_archived({name},{},{})",
                literal(relation),
                literal(archive_manifest_sha256)
            ));
        }
        Operation::Reattach { relation } => {
            valid_relation(family, relation)?;
            let p = partition(snapshot, relation)?;
            ensure(
                matches!(p["state"].as_str(), Some("detached" | "archived")),
                "preserved detached partition required",
            )?;
            sql.push(format!(
                "SELECT partition_ops.reattach({name},{})",
                literal(relation)
            ));
        }
    }
    Ok(
        json!({"schema":"agentforge-append-operation-plan/v1","operation":operation,"family":family.name,"expected_revision":revision,"authority_snapshot_sha256":snapshot.authority_snapshot_sha256,"catalog_sha256":snapshot.catalog_sha256,"sql":sql,"nontransactional_steps_present":nontransactional,"required_connection_settings":{"lock_timeout":"2s","statement_timeout":"60s","TimeZone":"UTC"},"effects":false,"execution_owner":"native-migration-authority","requires_fresh_owner_snapshot_after_each_step":true,"ga_acceptance":false,"identity_directory_retained_after_payload_drop":true}),
    )
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveManifest {
    pub schema: String,
    pub family: String,
    pub relation: String,
    pub relation_oid: u32,
    pub rows: u64,
    pub source_snapshot_sha256: String,
    pub archive: EvidenceFile,
    pub restore_validation: EvidenceFile,
}
impl ArchiveManifest {
    pub fn verify(&self, receipt: &ArchiveReceipt) -> Result<()> {
        ensure(
            self.schema == "agentforge-partition-archive/v1"
                && self.family == receipt.family
                && self.relation == receipt.relation
                && self.relation_oid == receipt.relation_oid
                && self.rows == receipt.rows
                && self.source_snapshot_sha256 == receipt.source_snapshot_sha256,
            "archive metadata consistency mismatch",
        )?;
        ensure(
            self.archive.sha256 == receipt.archive_sha256
                && self.restore_validation.sha256 == receipt.restore_validation_sha256,
            "archive/restore receipt mismatch",
        )?;
        let files = BTreeMap::from([
            ("archive".to_string(), self.archive.clone()),
            (
                "restore_validation".to_string(),
                self.restore_validation.clone(),
            ),
        ]);
        // Restore evidence is bounded metadata. Archive bytes can be large: hash
        // streaming, never load a partition archive into the process heap.
        for (kind, record) in files {
            ensure(
                record.path.is_absolute() && digest(&record.sha256),
                "absolute archive evidence required",
            )?;
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            let mut file = options
                .open(&record.path)
                .map_err(|_| "archive evidence unavailable")?;
            ensure(
                file.metadata()
                    .map_err(|_| "archive evidence unavailable")?
                    .is_file(),
                "regular archive evidence required",
            )?;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = file
                    .read(&mut buffer)
                    .map_err(|_| "archive checksum read failed")?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            ensure(
                format!("{:x}", hash.finalize()) == record.sha256,
                &format!("{kind} checksum mismatch"),
            )?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;

/// Bounded scheduled preparation for the complete declared family inventory.
/// Inactive families remain explicit blockers; this never admits a cutover or
/// grants a service schema DDL. Execute individual outputs through authority.
pub fn maintenance(
    snapshots: &[Snapshot],
    now: DateTime<Utc>,
    daily_ahead: u32,
    monthly_ahead: u32,
) -> Result<Value> {
    ensure(
        daily_ahead <= 89 && monthly_ahead <= 23,
        "bounded preparation horizon required",
    )?;
    ensure(
        snapshots.len() == catalog::ALL.len(),
        "complete 22-family maintenance snapshot required",
    )?;
    let mut by_name = BTreeMap::new();
    let mut authority = None;
    for snapshot in snapshots {
        let family = validate_snapshot(snapshot)?;
        ensure(
            by_name.insert(family.name, snapshot).is_none(),
            "duplicate maintenance family",
        )?;
        if let Some(expected) = authority {
            ensure(
                expected == snapshot.authority_snapshot_sha256,
                "mixed authority observations refused",
            )?;
        } else {
            authority = Some(snapshot.authority_snapshot_sha256.as_str());
        }
    }
    let mut plans = Vec::new();
    let mut blocked = Vec::new();
    for family in catalog::ALL {
        let snapshot = by_name
            .get(family.name)
            .ok_or("maintenance family missing")?;
        if snapshot.catalog["phase"] != "active" {
            blocked.push(family.name);
            continue;
        }
        let day = now.date_naive();
        let first = match family.cadence {
            Cadence::Day => day,
            Cadence::Month => day.with_day(1).ok_or("month boundary")?,
        }
        .and_hms_opt(0, 0, 0)
        .ok_or("UTC boundary")?
        .and_utc();
        let count = 1 + match family.cadence {
            Cadence::Day => daily_ahead,
            Cadence::Month => monthly_ahead,
        };
        plans.push(plan(snapshot, &Operation::Prepare { first, count }, now)?);
    }
    Ok(
        json!({"schema":"agentforge-append-maintenance/v1","status":if blocked.is_empty(){"planned_not_executed"}else if plans.is_empty(){"blocked"}else{"partial_inactive_families"},"observed_at":now,"family_count":catalog::ALL.len(),"plans":plans,"blocked_inactive_families":blocked,"authority_snapshot_sha256":authority,"effects":false,"execution_owner":"native-migration-authority","retention_cutover_or_capacity_acceptance":false}),
    )
}
