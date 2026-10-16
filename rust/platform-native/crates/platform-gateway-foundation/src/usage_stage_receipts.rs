//! Existing-table accounting receipts. A receipt commits in the same transaction as its
//! mutation, so an acknowledged receipt is authoritative for replay suppression.
//! This is not a durable intake outbox and never resolves an uncertain raw commit.
use super::*;
use sha2::{Digest, Sha256};
const OBSERVATION: &str = "_native_accounting_observation";
fn digest(record: &UsageRecord) -> String {
    let value = json!([
        record.tenant_id,
        record.user_id,
        record.request_id,
        record.service_id,
        record.request_type,
        record.model,
        record.provider,
        record.timestamp,
        record.input_tokens,
        record.output_tokens,
        record.input_cost_cents,
        record.output_cost_cents,
        record.status
    ]);
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
pub(super) fn bind_observation(accounting: &mut UsageRecord, original: &UsageRecord) {
    // Always overwrite caller metadata at the trusted recorder boundary.
    accounting
        .metadata
        .insert(OBSERVATION.into(), json!(digest(original)));
}
/// Stable identifier for this exact cumulative observation and accounting stage.
/// Request identity is mandatory: anonymous unkeyed records cannot be safely replayed.
pub fn receipt_id(
    stage: AccountingStage,
    record: &UsageRecord,
) -> Result<String, UsageRecorderError> {
    if record.identity().is_none() {
        return Err(UsageRecorderError::InvalidAccountingDelta);
    }
    let observation = record
        .metadata
        .get(OBSERVATION)
        .and_then(Value::as_str)
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .unwrap_or_else(|| digest(record));
    // Include the actual delta too: the observation binds its origin while this
    // digest prevents accidental reuse for a different mutation projection.
    let value = json!([
        stage.name(),
        observation,
        record.tenant_id,
        record.user_id,
        record.request_id,
        record.service_id,
        record.request_type,
        record.input_tokens,
        record.output_tokens,
        record.input_cost_cents,
        record.output_cost_cents,
        record.metadata.get("_accounting_request_delta"),
        record.metadata.get("_accounting_success_delta"),
        record.metadata.get("_accounting_error_delta")
    ]);
    Ok(format!(
        "usage-stage:{}:{:x}",
        stage.name(),
        Sha256::digest(value.to_string().as_bytes())
    ))
}
pub(super) async fn claim_in_transaction(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    stage: AccountingStage,
    records: &[UsageRecord],
) -> UsageFutureResult<Vec<UsageRecord>> {
    let mut accepted = vec![];
    for record in records {
        if record.identity().is_none() {
            // Preserve ordinary source accounting for unkeyed records. There is
            // no stable replay authority for these records, so no replay is offered.
            accepted.push(record.clone());
            continue;
        }
        let event = receipt_id(stage, record)?;
        let inserted=sqlx::query_scalar::<_,String>("INSERT INTO billing_events(event_id,request_id,service_id,user_id,tenant_id,input_tokens,output_tokens,total_tokens,model,assistant_id,status,cost_amount,cost_currency,metadata,event_time) VALUES($1,$2,$3,$4,$5,0,0,0,$6,$7,'accounting_applied',NULL,'USD',$8,$9) ON CONFLICT(event_id) DO NOTHING RETURNING event_id")
            .bind(&event).bind(&record.request_id).bind(if record.service_id.is_empty(){"unattributed_service"}else{&record.service_id}).bind(&record.user_id).bind(&record.tenant_id).bind(&record.model).bind(&record.assistant_id)
            .bind(json!({"event":"usage_accounting_stage_receipt_v1","stage":stage.name(),"observation":record.metadata.get(OBSERVATION)})).bind(record.timestamp).fetch_optional(&mut **tx).await?;
        if inserted.is_some() {
            accepted.push(record.clone());
        } else {
            // Do not treat an unrelated or malformed colliding event as proof.
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM billing_events WHERE event_id=$1 AND request_id=$2 AND tenant_id=$3 AND user_id=$4 AND status='accounting_applied' AND metadata->>'event'='usage_accounting_stage_receipt_v1' AND metadata->>'stage'=$5)").bind(&event).bind(&record.request_id).bind(&record.tenant_id).bind(&record.user_id).bind(stage.name()).fetch_one(&mut **tx).await?;
            if !valid {
                return Err(UsageRecorderError::InvalidAccountingDelta);
            }
        }
    }
    Ok(accepted)
}
type UsageFutureResult<T> = Result<T, UsageRecorderError>;

pub(super) async fn apply_in_transaction(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    stage: AccountingStage,
    records: &[UsageRecord],
) -> UsageFutureResult<()> {
    if stage == AccountingStage::RequestTraces {
        return Err(UsageRecorderError::InvalidAccountingDelta);
    }
    let accepted = claim_in_transaction(tx, stage, records).await?;
    match stage {
        AccountingStage::QuotaCounters => {
            for ((tenant, user), totals) in quota_adjustments(&accepted)? {
                sqlx::query("UPDATE user_quotas SET current_daily_tokens=current_daily_tokens+$3,current_monthly_tokens=current_monthly_tokens+$3,current_monthly_cost_cents=current_monthly_cost_cents+$4,current_daily_requests=current_daily_requests+$5,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2")
                    .bind(tenant).bind(user).bind(totals.tokens).bind(totals.cost_cents).bind(totals.requests).execute(&mut **tx).await?;
            }
        }
        AccountingStage::DailyAggregates | AccountingStage::HourlyAggregates => {
            let records: Vec<_> = accepted
                .iter()
                .map(UsageRecord::accounting_record)
                .collect::<Result<_, _>>()?;
            let granularity = if stage == AccountingStage::DailyAggregates {
                Granularity::Day
            } else {
                Granularity::Hour
            };
            apply_usage_aggregates_for_granularity(tx, &records, granularity).await?;
        }
        AccountingStage::RequestTraces => unreachable!(),
    }
    Ok(())
}
/// Failure identity is deterministic even for an unkeyed, manual-only receipt.
pub fn failure_id(stage: AccountingStage, record: &UsageRecord) -> String {
    format!(
        "usage-failed:{:x}",
        Sha256::digest(
            receipt_id(stage, record)
                .unwrap_or_else(|_| format!("{}:{}", stage.name(), digest(record)))
                .as_bytes()
        )
    )
}
/// Persist only a bounded error classification, never driver messages or credentials.
/// Unknown commits and non-transactional trace writes remain manual reconciliation.
pub fn failure_payload(
    stage: AccountingStage,
    record: &UsageRecord,
    error: &UsageRecorderError,
) -> UsageFutureResult<Value> {
    let unknown_commit = matches!(error, UsageRecorderError::CommitUncertain(_));
    Ok(
        json!({"event":"usage_accounting_reconciliation_required","version":1,"stage":stage.name(),"error_type":error.kind(),"commit_unknown":unknown_commit,"replay_safe":!unknown_commit && stage != AccountingStage::RequestTraces && record.identity().is_some(),"record":record}),
    )
}

/// Merge only quarantine flags. The first receipt remains the payload authority;
/// uncertainty is sticky and a prohibited replay can never become permitted.
/// Missing or malformed flags fail closed rather than restoring replay authority.
pub(super) fn merge_failure_quarantine(
    stored: &Value,
    incoming: &Value,
) -> UsageFutureResult<Value> {
    if !stored.is_object()
        || !incoming.is_object()
        || stored["event"] != "usage_accounting_reconciliation_required"
        || incoming["event"] != stored["event"]
        || stored["version"] != 1
        || incoming["version"] != stored["version"]
        || incoming["stage"] != stored["stage"]
    {
        return Err(UsageRecorderError::InvalidAccountingDelta);
    }
    let unknown = stored["commit_unknown"] != false || incoming["commit_unknown"] != false;
    let safe = !unknown && stored["replay_safe"] == true && incoming["replay_safe"] == true;
    let mut result = stored.clone();
    result["commit_unknown"] = json!(unknown);
    result["replay_safe"] = json!(safe);
    Ok(result)
}

impl PgUsagePersistence {
    /// Trusted reconciliation operation scoped to the caller's verified tenant.
    /// The failed receipt, authoritative applied receipt, mutation and replay mark
    /// share one transaction. There is no automatic retry on commit uncertainty.
    pub async fn replay_accounting_failure(
        &self,
        event_id: &str,
        expected_tenant: &str,
    ) -> UsageFutureResult<StageStatus> {
        if expected_tenant.is_empty() || !event_id.starts_with("usage-failed:") {
            return Err(UsageRecorderError::InvalidAccountingDelta);
        }
        let mut tx = self.pool.begin().await?;
        let row: Option<(String,Value)> = sqlx::query_as("SELECT status,metadata FROM billing_events WHERE event_id=$1 AND tenant_id=$2 FOR UPDATE")
            .bind(event_id).bind(expected_tenant).fetch_optional(&mut *tx).await?;
        let Some((status, payload)) = row else {
            return Err(UsageRecorderError::InvalidAccountingDelta);
        };
        if status == "reconciliation_replayed" {
            return Ok(StageStatus::Skipped);
        }
        if status != "reconciliation_required"
            || payload["event"] != "usage_accounting_reconciliation_required"
            || payload["version"] != 1
            || payload["replay_safe"] != true
            || payload["commit_unknown"] != false
        {
            return Err(UsageRecorderError::InvalidAccountingDelta);
        }
        let stage = match payload["stage"].as_str() {
            Some("quota_counters") => AccountingStage::QuotaCounters,
            Some("daily_aggregates") => AccountingStage::DailyAggregates,
            Some("hourly_aggregates") => AccountingStage::HourlyAggregates,
            _ => return Err(UsageRecorderError::InvalidAccountingDelta),
        };
        let record: UsageRecord = serde_json::from_value(payload["record"].clone())
            .map_err(|_| UsageRecorderError::InvalidAccountingDelta)?;
        if record.tenant_id != expected_tenant
            || record.identity().is_none()
            || failure_id(stage, &record) != event_id
        {
            return Err(UsageRecorderError::InvalidAccountingDelta);
        }
        apply_in_transaction(&mut tx, stage, &[record]).await?;
        sqlx::query("UPDATE billing_events SET status='reconciliation_replayed',metadata=metadata||jsonb_build_object('replayed_at',CURRENT_TIMESTAMP) WHERE event_id=$1 AND tenant_id=$2")
            .bind(event_id).bind(expected_tenant).execute(&mut *tx).await?;
        tx.commit()
            .await
            .map_err(UsageRecorderError::CommitUncertain)?;
        Ok(StageStatus::Applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> UsageRecord {
        let mut r = UsageRecord::new(
            "tenant-a",
            "user-a",
            "model-a",
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        r.request_id = "request-a".into();
        r.service_id = "service-a".into();
        r.input_tokens = 10;
        r
    }
    #[test]
    fn exact_observation_stage_and_tenant_are_bound_to_receipt() {
        let r = record();
        let stage = AccountingStage::QuotaCounters;
        assert_eq!(
            receipt_id(stage, &r).unwrap(),
            receipt_id(stage, &r.clone()).unwrap()
        );
        assert_ne!(
            receipt_id(stage, &r).unwrap(),
            receipt_id(AccountingStage::DailyAggregates, &r).unwrap()
        );
        let mut other = r.clone();
        other.tenant_id = "tenant-b".into();
        assert_ne!(
            receipt_id(stage, &r).unwrap(),
            receipt_id(stage, &other).unwrap()
        );
        let restored: UsageRecord =
            serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(r, restored);
    }
    #[test]
    fn equal_deltas_from_distinct_cumulative_observations_do_not_collide() {
        let first = record();
        let mut second = first.clone();
        second.input_tokens += 1;
        let mut a = first.clone();
        let mut b = second.clone();
        a.metadata
            .insert(OBSERVATION.into(), json!("attacker-selected"));
        bind_observation(&mut a, &first);
        bind_observation(&mut b, &second);
        a.input_tokens = 1;
        b.input_tokens = 1;
        assert_ne!(
            receipt_id(AccountingStage::QuotaCounters, &a).unwrap(),
            receipt_id(AccountingStage::QuotaCounters, &b).unwrap()
        );
        assert_eq!(a.metadata[OBSERVATION], digest(&first));
    }
    #[test]
    fn uncertain_trace_and_unkeyed_failures_are_manual_and_errors_are_sanitized() {
        let r = record();
        let stage = AccountingStage::QuotaCounters;
        let failure = UsageRecorderError::Collaborator("private-backend-credential".into());
        let safe = failure_payload(stage, &r, &failure).unwrap();
        assert_eq!(safe["replay_safe"], true);
        assert!(!safe.to_string().contains("private-backend-credential"));
        let uncertain = failure_payload(
            stage,
            &r,
            &UsageRecorderError::CommitUncertain(sqlx::Error::PoolClosed),
        )
        .unwrap();
        assert_eq!(uncertain["commit_unknown"], true);
        assert_eq!(uncertain["replay_safe"], false);
        assert_eq!(
            failure_payload(AccountingStage::RequestTraces, &r, &failure).unwrap()["replay_safe"],
            false
        );
        let mut unkeyed = r.clone();
        unkeyed.request_id.clear();
        assert_eq!(
            failure_payload(stage, &unkeyed, &failure).unwrap()["replay_safe"],
            false
        );
        assert_eq!(failure_id(stage, &r), failure_id(stage, &r));
    }
}

#[cfg(test)]
mod quarantine_conflict_tests {
    use super::*;
    fn payload(unknown: bool, safe: bool) -> Value {
        json!({"event":"usage_accounting_reconciliation_required","version":1,"stage":"quota_counters","commit_unknown":unknown,"replay_safe":safe,"record":{"tenant_id":"tenant-a","request_id":"same-observation"},"error_type":"original-classification"})
    }
    #[test]
    fn safe_then_uncertain_then_safe_never_restores_replay_authority() {
        let safe = payload(false, true);
        let uncertain = payload(true, false);
        // Old SQL retained `safe` verbatim on conflict, so the persisted authority
        // stayed replay_safe=true. Exercise the actual production merge instead.
        let merged = merge_failure_quarantine(&safe, &uncertain).unwrap();
        assert_eq!(merged["commit_unknown"], true);
        assert_eq!(merged["replay_safe"], false);
        assert_eq!(merge_failure_quarantine(&merged, &safe).unwrap(), merged);
        assert_eq!(merged["record"], safe["record"]);
        assert_eq!(merged["error_type"], safe["error_type"]);
    }
    #[test]
    fn merge_is_monotonic_idempotent_and_order_independent_for_all_flag_pairs() {
        for old_unknown in [false, true] {
            for old_safe in [false, true] {
                for new_unknown in [false, true] {
                    for new_safe in [false, true] {
                        let old = payload(old_unknown, old_safe);
                        let new = payload(new_unknown, new_safe);
                        let actual = merge_failure_quarantine(&old, &new).unwrap();
                        assert_eq!(actual["commit_unknown"], old_unknown || new_unknown);
                        assert_eq!(
                            actual["replay_safe"],
                            !old_unknown && !new_unknown && old_safe && new_safe
                        );
                        assert_eq!(actual, merge_failure_quarantine(&new, &old).unwrap());
                        assert_eq!(actual, merge_failure_quarantine(&actual, &new).unwrap());
                    }
                }
            }
        }
    }
    #[test]
    fn malformed_flags_fail_closed_and_unrelated_receipts_are_rejected() {
        let safe = payload(false, true);
        for bad in [Value::Null, json!("false"), json!(0)] {
            let mut unknown = safe.clone();
            unknown["commit_unknown"] = bad.clone();
            let merged = merge_failure_quarantine(&safe, &unknown).unwrap();
            assert_eq!(merged["commit_unknown"], true);
            assert_eq!(merged["replay_safe"], false);
            let mut unsafe_payload = safe.clone();
            unsafe_payload["replay_safe"] = bad;
            assert_eq!(
                merge_failure_quarantine(&safe, &unsafe_payload).unwrap()["replay_safe"],
                false
            );
        }
        let mut wrong = safe.clone();
        wrong["stage"] = json!("hourly_aggregates");
        assert!(merge_failure_quarantine(&safe, &wrong).is_err());
        assert!(merge_failure_quarantine(&Value::Null, &safe).is_err());
    }
}
