//! Generates authority-reviewable SQL only. No connection, detach, archive or drop.
use super::{Result, ensure};
use chrono::{DateTime, Datelike, Months, Utc};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Partition {
    pub relation: String,
    pub lower: DateTime<Utc>,
    pub upper: DateTime<Utc>,
    pub authority_snapshot_sha256: String,
    pub archive_sha256: Option<String>,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn plan(partition: &Partition, cutoff: DateTime<Utc>) -> Result<serde_json::Value> {
    ensure(
        digest(&partition.authority_snapshot_sha256),
        "authority catalog snapshot digest required",
    )?;
    ensure(
        partition.lower.day() == 1
            && partition.lower
                == partition
                    .lower
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
            && partition.lower.checked_add_months(Months::new(1)) == Some(partition.upper),
        "exact UTC monthly bounds required",
    )?;
    let expected = format!("gateway.audit_logs_p{}", partition.lower.format("%Y%m"));
    ensure(
        partition.relation == expected,
        "only authority-owned audit monthly partitions are supported",
    )?;
    ensure(
        partition.upper <= cutoff && partition.upper < Utc::now(),
        "open or retained partition cannot be detached",
    )?;
    if let Some(archive) = &partition.archive_sha256 {
        ensure(digest(archive), "invalid archive digest")?;
    }
    let detach = format!(
        "SET lock_timeout='2s'; SET statement_timeout='60s'; ALTER TABLE gateway.audit_logs DETACH PARTITION {} CONCURRENTLY;",
        partition.relation
    );
    let drop = partition.archive_sha256.as_ref().map(|_| {
        format!(
            "SET LOCAL lock_timeout='2s'; SET LOCAL statement_timeout='60s'; DROP TABLE {};",
            partition.relation
        )
    });
    Ok(
        serde_json::json!({"schema":"agentforge-partition-retention/v1","operation":"plan-only","effects":false,"authority_snapshot_sha256":partition.authority_snapshot_sha256,"relation":partition.relation,"detach_nontransactional_sql":detach,"archive_sha256":partition.archive_sha256,"drop_sql_requires_separate_approval":drop,"drop_ready":partition.archive_sha256.is_some(),"identity_registry_retained":true,"rollback":"restore-required-after-drop"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Partition {
        Partition {
            relation: "gateway.audit_logs_p202601".into(),
            lower: "2026-01-01T00:00:00Z".parse().unwrap(),
            upper: "2026-02-01T00:00:00Z".parse().unwrap(),
            authority_snapshot_sha256: "a".repeat(64),
            archive_sha256: None,
        }
    }
    #[test]
    fn archive_confirmation_is_required_for_drop_plan() {
        let mut p = fixture();
        let cutoff = "2026-03-01T00:00:00Z".parse().unwrap();
        assert_eq!(plan(&p, cutoff).unwrap()["drop_ready"], false);
        p.archive_sha256 = Some("b".repeat(64));
        assert_eq!(plan(&p, cutoff).unwrap()["drop_ready"], true);
    }
    #[test]
    fn injection_legacy_live_and_boundary_errors_are_refused() {
        let cutoff = "2026-03-01T00:00:00Z".parse().unwrap();
        for relation in [
            "gateway.audit_logs_legacy",
            "gateway.usage_records",
            "gateway.audit_logs_p202601;DROP SCHEMA gateway CASCADE",
        ] {
            let mut p = fixture();
            p.relation = relation.into();
            assert!(plan(&p, cutoff).is_err());
        }
        let mut p = fixture();
        p.upper = "2026-02-02T00:00:00Z".parse().unwrap();
        assert!(plan(&p, cutoff).is_err());
        assert!(plan(&fixture(), "2026-01-15T00:00:00Z".parse().unwrap()).is_err());
    }
}
