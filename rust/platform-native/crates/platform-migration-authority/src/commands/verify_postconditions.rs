use crate::{
    AuthorityError, Result,
    manifest::{ChangeSpec, EpochManifest},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const EXPANSION_SEQUENCE: i32 = 27;
const CONTRACT_SEQUENCE: i32 = 28;
const EXPANSION_SQL_SHA256: &str =
    "e22a46ef53f48f7839d6b06f121f8c57e3fa18a8065ffcc4e6b2af8760c9ed7f";
const CONTRACT_SQL_SHA256: &str =
    "d6b47a4abd6427f2a788f9447750a0201787dc9e64bcb574dca2deaecf2ca3f4";
const EXPANSION_POSTCONDITION_INDEX: usize = 3;
// Complete UTF-8 string hashes from the canonical 027 manifest and measured 028 preview.
const EXPANSION_POSTCONDITION_SHA256: &str =
    "9045318a15b1712b486416e32493e1e36433c12c3fc7e62868881bc56a1efa19";
const CONTRACT_POSTCONDITION_SHA256: &str =
    "5d4e5bb1730a606232d329d166ab23c8e76114361c78f98ec17889fb1d22807b";

fn refused(message: &'static str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn unique_change(epoch: &EpochManifest, sequence: i32) -> Result<&ChangeSpec> {
    let mut matches = epoch
        .changes
        .iter()
        .filter(|change| change.sequence == sequence);
    let change = matches
        .next()
        .ok_or_else(|| refused("successor verification manifest is missing a pinned change"))?;
    if matches.next().is_some() {
        return Err(refused(
            "successor verification manifest duplicates a pinned sequence",
        ));
    }
    Ok(change)
}

/// Returns unchanged historical specs except for the one 027 invariant superseded by 028.
/// The caller must run require_ledger_state first; this adapter only selects the exact
/// verified postcondition string and never omits a condition.
pub(super) fn for_applied_prefix(
    epoch: &EpochManifest,
    applied: &BTreeMap<i32, String>,
) -> Result<Vec<ChangeSpec>> {
    let Some(contract_checksum) = applied.get(&CONTRACT_SEQUENCE) else {
        if applied.keys().any(|sequence| *sequence > CONTRACT_SEQUENCE) {
            return Err(refused(
                "successor verification ledger skips the fixed 028 contract",
            ));
        }
        return Ok(epoch.changes.clone());
    };
    if contract_checksum != CONTRACT_SQL_SHA256 {
        return Err(refused(
            "successor verification ledger has an unexpected 028 checksum",
        ));
    }
    if applied.get(&EXPANSION_SEQUENCE).map(String::as_str) != Some(EXPANSION_SQL_SHA256) {
        return Err(refused(
            "028 supersession requires the exact applied 027 expansion",
        ));
    }

    let expansion = unique_change(epoch, EXPANSION_SEQUENCE)?;
    let contract = unique_change(epoch, CONTRACT_SEQUENCE)?;
    if epoch.baseline_id != crate::DEFAULT_BASELINE_ID
        || epoch
            .changes
            .get(EXPANSION_SEQUENCE as usize - 1)
            .is_none_or(|change| change.sequence != EXPANSION_SEQUENCE)
        || epoch
            .changes
            .get(CONTRACT_SEQUENCE as usize - 1)
            .is_none_or(|change| change.sequence != CONTRACT_SEQUENCE)
        || expansion.sequence != EXPANSION_SEQUENCE
        || expansion.sha256 != EXPANSION_SQL_SHA256
        || contract.sequence != CONTRACT_SEQUENCE
        || contract.sha256 != CONTRACT_SQL_SHA256
    {
        return Err(refused(
            "028 supersession does not match the fixed 027/028 migrations",
        ));
    }
    let old_condition = expansion
        .postconditions
        .get(EXPANSION_POSTCONDITION_INDEX)
        .ok_or_else(|| refused("027 expansion postcondition is missing"))?;
    if sha256(old_condition) != EXPANSION_POSTCONDITION_SHA256 {
        return Err(refused(
            "027 expansion postcondition differs from the frozen supersession pin",
        ));
    }
    let replacement = contract
        .postconditions
        .first()
        .ok_or_else(|| refused("028 contract postcondition is missing"))?;
    if sha256(replacement) != CONTRACT_POSTCONDITION_SHA256 {
        return Err(refused(
            "028 contract postcondition differs from the measured preview pin",
        ));
    }

    let mut changes = epoch.changes.clone();
    let expansion = changes
        .iter_mut()
        .find(|change| change.sequence == EXPANSION_SEQUENCE)
        .ok_or_else(|| refused("027 expansion change disappeared"))?;
    expansion.postconditions[EXPANSION_POSTCONDITION_INDEX] = replacement.clone();
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{RollbackClass, TransactionMode};
    use serde_json::Value;

    const LIVE_027_MANIFEST: &str =
        include_str!("../../../../../../database/migrations/2026_08_post_kb_v1/manifest.yml");

    fn change(sequence: i32, sha256: &str, postconditions: Vec<String>) -> ChangeSpec {
        ChangeSpec {
            sequence,
            name: format!("change_{sequence}"),
            file: format!("{sequence:03}.sql"),
            sha256: sha256.into(),
            owner: "owner".into(),
            transaction_mode: TransactionMode::Transactional,
            rollback_class: RollbackClass::ForwardFixOnly,
            preconditions: Vec::new(),
            postconditions,
            timeout_seconds: 600,
            lock_budget_seconds: 5,
            resume_handler: None,
            repair_handler: None,
            notes: String::new(),
        }
    }

    fn fixed_epoch() -> EpochManifest {
        let mut changes = (1..EXPANSION_SEQUENCE)
            .map(|sequence| change(sequence, &format!("{sequence:064x}"), vec![]))
            .collect::<Vec<_>>();
        let actual_manifest: Value = serde_json::from_str(LIVE_027_MANIFEST).unwrap();
        let actual_expansion: ChangeSpec = serde_json::from_value(
            actual_manifest["changes"][EXPANSION_SEQUENCE as usize - 1].clone(),
        )
        .unwrap();
        assert_eq!(
            sha256(&actual_expansion.postconditions[EXPANSION_POSTCONDITION_INDEX]),
            EXPANSION_POSTCONDITION_SHA256
        );
        changes.push(actual_expansion);
        let replacement = crate::source_tools::next_successor_028::test_contraction_postcondition();
        assert_eq!(sha256(replacement), CONTRACT_POSTCONDITION_SHA256);
        changes.push(change(
            CONTRACT_SEQUENCE,
            CONTRACT_SQL_SHA256,
            vec![replacement.into(), "028 postcondition 2".into()],
        ));
        EpochManifest {
            baseline_id: crate::DEFAULT_BASELINE_ID.into(),
            epoch: CONTRACT_SEQUENCE,
            changes,
            extra: Value::Null,
        }
    }

    fn prefix(epoch: &EpochManifest, through: i32) -> BTreeMap<i32, String> {
        epoch
            .changes
            .iter()
            .filter(|change| change.sequence <= through)
            .map(|change| (change.sequence, change.sha256.clone()))
            .collect()
    }

    #[test]
    fn exact_028_supersedes_only_the_pinned_027_condition() {
        let epoch = fixed_epoch();
        let before = epoch.changes.clone();
        let applied = prefix(&epoch, CONTRACT_SEQUENCE);
        let after = for_applied_prefix(&epoch, &applied).unwrap();
        let expansion_index = EXPANSION_SEQUENCE as usize - 1;
        let contract_index = CONTRACT_SEQUENCE as usize - 1;
        assert_eq!(
            after[expansion_index].postconditions[EXPANSION_POSTCONDITION_INDEX],
            after[contract_index].postconditions[0]
        );
        assert_eq!(
            after[contract_index].postconditions,
            before[contract_index].postconditions
        );
        for (index, (old, new)) in before.iter().zip(&after).enumerate() {
            if index == expansion_index {
                for condition in 0..old.postconditions.len() {
                    if condition != EXPANSION_POSTCONDITION_INDEX {
                        assert_eq!(old.postconditions[condition], new.postconditions[condition]);
                    }
                }
            } else {
                assert_eq!(old.postconditions, new.postconditions);
            }
        }
    }

    #[test]
    fn missing_028_keeps_every_027_condition_unchanged() {
        let epoch = fixed_epoch();
        let changes = for_applied_prefix(&epoch, &prefix(&epoch, EXPANSION_SEQUENCE)).unwrap();
        assert_eq!(
            changes[EXPANSION_SEQUENCE as usize - 1].postconditions[EXPANSION_POSTCONDITION_INDEX],
            epoch.changes[EXPANSION_SEQUENCE as usize - 1].postconditions
                [EXPANSION_POSTCONDITION_INDEX]
        );
    }

    #[test]
    fn wrong_sequence_or_sql_never_activates_the_adapter() {
        let mut epoch = fixed_epoch();
        let mut applied = prefix(&epoch, CONTRACT_SEQUENCE);
        applied.insert(CONTRACT_SEQUENCE, "f".repeat(64));
        assert!(for_applied_prefix(&epoch, &applied).is_err());
        applied.insert(CONTRACT_SEQUENCE, CONTRACT_SQL_SHA256.into());
        applied.insert(EXPANSION_SEQUENCE, "f".repeat(64));
        assert!(for_applied_prefix(&epoch, &applied).is_err());
        applied.insert(EXPANSION_SEQUENCE, EXPANSION_SQL_SHA256.into());
        applied.remove(&EXPANSION_SEQUENCE);
        assert!(for_applied_prefix(&epoch, &applied).is_err());
        applied = prefix(&epoch, CONTRACT_SEQUENCE);
        epoch.changes[CONTRACT_SEQUENCE as usize - 1].sequence = CONTRACT_SEQUENCE + 1;
        assert!(for_applied_prefix(&epoch, &applied).is_err());
        let mut wrong_order = fixed_epoch();
        wrong_order.changes.swap(
            EXPANSION_SEQUENCE as usize - 1,
            CONTRACT_SEQUENCE as usize - 1,
        );
        assert!(
            for_applied_prefix(&wrong_order, &prefix(&wrong_order, CONTRACT_SEQUENCE)).is_err()
        );
        let mut wrong_manifest_sql = fixed_epoch();
        let matching_ledger = prefix(&wrong_manifest_sql, CONTRACT_SEQUENCE);
        wrong_manifest_sql.changes[EXPANSION_SEQUENCE as usize - 1].sha256 = "a".repeat(64);
        assert!(for_applied_prefix(&wrong_manifest_sql, &matching_ledger).is_err());
        let mut wrong_contract_sql = fixed_epoch();
        let matching_ledger = prefix(&wrong_contract_sql, CONTRACT_SEQUENCE);
        wrong_contract_sql.changes[CONTRACT_SEQUENCE as usize - 1].sha256 = "b".repeat(64);
        assert!(for_applied_prefix(&wrong_contract_sql, &matching_ledger).is_err());
        let mut gap = prefix(&epoch, EXPANSION_SEQUENCE);
        gap.insert(CONTRACT_SEQUENCE + 1, "a".repeat(64));
        assert!(for_applied_prefix(&epoch, &gap).is_err());
    }

    #[test]
    fn altered_027_or_028_condition_hashes_are_refused() {
        let applied_epoch = fixed_epoch();
        let applied = prefix(&applied_epoch, CONTRACT_SEQUENCE);
        let mut changed_027 = applied_epoch.clone();
        changed_027.changes[EXPANSION_SEQUENCE as usize - 1].postconditions
            [EXPANSION_POSTCONDITION_INDEX]
            .push(' ');
        assert!(for_applied_prefix(&changed_027, &applied).is_err());
        let mut changed_028 = applied_epoch;
        changed_028.changes[CONTRACT_SEQUENCE as usize - 1].postconditions[0].push(' ');
        assert!(for_applied_prefix(&changed_028, &applied).is_err());
    }

    #[test]
    fn pinned_replacement_requires_both_old_checks_absent_and_both_expansions_validated() {
        let replacement = crate::source_tools::next_successor_028::test_contraction_postcondition();
        assert_eq!(sha256(replacement), CONTRACT_POSTCONDITION_SHA256);
        for (table, old) in [
            (
                "gateway.price_book_v2_terms",
                "price_book_v2_terms_component_check",
            ),
            (
                "gateway.price_book_v2_usage_receipts",
                "price_book_v2_usage_receipts_check",
            ),
        ] {
            assert!(replacement.contains(&format!(
                "NOT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='{table}'::pg_catalog.regclass AND conname='{old}')"
            )));
        }
        for (table, expanded) in [
            (
                "gateway.price_book_v2_terms",
                "price_book_v2_terms_component_request_v2_check",
            ),
            (
                "gateway.price_book_v2_usage_receipts",
                "price_book_v2_usage_receipts_request_v2_check",
            ),
        ] {
            assert!(replacement.contains(&format!(
                "conrelid='{table}'::pg_catalog.regclass AND conname='{expanded}' AND convalidated"
            )));
        }
    }
}
