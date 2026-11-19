//! Exact 029 replacements for constraints widened by the measured Batch migration.
//! Frozen historical files remain unchanged; every other invariant is retained.
use super::{CONTRACT_POSTCONDITION_SHA256, refused, sha256, unique_change};
use crate::{
    Result,
    manifest::{ChangeSpec, EpochManifest},
};
use std::collections::BTreeMap;

const SEQUENCE: i32 = 29;
const SQL_SHA256: &str = "8652f2b46e760a6c37947f5e56540219037a0b90eb456a73d438fc7f9ce96745";
const STATE_SHA256: &str = "5da07d04493679492d830a25086f3baf6c8af2758244d4096708672178aed1e0";
const METER: (&str, &str) = ("usage_records_meter_v1", "usage_records_batch_meter_v1");
const RECEIPT: (&str, &str) = (
    "price_book_v2_usage_receipts_request_v2_check",
    "price_book_v2_batch_receipt_check",
);

fn replace_exact(condition: &str, expected: &str, names: &[(&str, &str)]) -> Result<String> {
    if sha256(condition) != expected {
        return Err(refused(
            "029 supersession differs from its exact historical condition",
        ));
    }
    let mut replacement = condition.to_owned();
    for (old, new) in names {
        let old = format!("conname='{old}'");
        if replacement.matches(&old).count() != 1 {
            return Err(refused(
                "029 supersession constraint is not uniquely identified",
            ));
        }
        replacement = replacement.replace(&old, &format!("conname='{new}'"));
    }
    Ok(replacement)
}

pub(super) fn for_applied_prefix(
    epoch: &EpochManifest,
    applied: &BTreeMap<i32, String>,
    mut changes: Vec<ChangeSpec>,
) -> Result<Vec<ChangeSpec>> {
    let Some(checksum) = applied.get(&SEQUENCE) else {
        if applied.keys().any(|sequence| *sequence > SEQUENCE) {
            return Err(refused(
                "successor verification ledger skips the fixed 029 contract",
            ));
        }
        return Ok(changes);
    };
    let batch = unique_change(epoch, SEQUENCE)?;
    if checksum != SQL_SHA256
        || batch.sha256 != SQL_SHA256
        || epoch
            .changes
            .get(SEQUENCE as usize - 1)
            .is_none_or(|c| c.sequence != SEQUENCE)
    {
        return Err(refused(
            "029 supersession requires the exact ordered Batch migration",
        ));
    }
    // The 029 migration validates wider token/request/Batch constraints before
    // removing the narrow predecessors. No identity, trigger or pricing check
    // is removed: only the two exact validated constraint identities change.
    for (sequence, index, hash, names) in [
        (27, 2, STATE_SHA256, &[METER][..]),
        (27, 3, CONTRACT_POSTCONDITION_SHA256, &[METER, RECEIPT][..]),
        (28, 0, CONTRACT_POSTCONDITION_SHA256, &[METER, RECEIPT][..]),
    ] {
        let condition = changes
            .iter_mut()
            .find(|c| c.sequence == sequence)
            .and_then(|c| c.postconditions.get_mut(index))
            .ok_or_else(|| refused("029 supersession historical condition is missing"))?;
        *condition = replace_exact(condition, hash, names)?;
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn current_epoch() -> EpochManifest {
        crate::manifest::load_epoch_manifest(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../database/migrations/2026_08_post_kb_v1/manifest.yml"),
        )
        .unwrap()
    }
    #[test]
    fn exact_replacements_retain_every_other_historical_term() {
        let epoch = current_epoch();
        for (sequence, index, hash, names) in [
            (27, 2, STATE_SHA256, &[METER][..]),
            (28, 0, CONTRACT_POSTCONDITION_SHA256, &[METER, RECEIPT][..]),
        ] {
            let original = &epoch.changes[sequence - 1].postconditions[index];
            let mut result = replace_exact(original, hash, names).unwrap();
            for (old, new) in names {
                assert!(result.contains(&format!("conname='{new}' AND convalidated")));
                result = result.replace(&format!("conname='{new}'"), &format!("conname='{old}'"));
            }
            assert_eq!(&result, original);
            assert!(replace_exact(&format!("{original} "), hash, names).is_err());
        }
    }
    #[test]
    fn missing_batch_step_cannot_be_skipped() {
        let epoch = current_epoch();
        assert!(
            for_applied_prefix(
                &epoch,
                &BTreeMap::from([(30, "a".repeat(64))]),
                epoch.changes.clone()
            )
            .is_err()
        );
        assert!(
            for_applied_prefix(
                &epoch,
                &BTreeMap::from([(29, "a".repeat(64))]),
                epoch.changes.clone()
            )
            .is_err()
        );
    }
    #[test]
    fn actual029_prefix_keeps_all_terms_and_rejects_historical_or_candidate_drift() {
        let epoch = current_epoch();
        let applied = epoch
            .changes
            .iter()
            .map(|c| (c.sequence, c.sha256.clone()))
            .collect();
        let effective = super::super::for_applied_prefix(&epoch, &applied).unwrap();
        assert_eq!(effective.len(), epoch.changes.len());
        for (old, new) in epoch.changes.iter().zip(&effective) {
            assert_eq!(old.preconditions, new.preconditions);
            assert_eq!(old.postconditions.len(), new.postconditions.len());
            assert_eq!(old.sha256, new.sha256);
            assert_eq!(old.name, new.name);
            if !matches!(old.sequence, 27 | 28) {
                assert_eq!(old.postconditions, new.postconditions);
            }
        }
        for (sequence, index) in [(27, 2), (27, 3), (28, 0)] {
            let mut drift = epoch.clone();
            drift.changes[sequence - 1].postconditions[index].push(' ');
            assert!(super::super::for_applied_prefix(&drift, &applied).is_err());
        }
        let mut drift = epoch.clone();
        drift.changes[28].sha256 = "a".repeat(64);
        assert!(super::super::for_applied_prefix(&drift, &applied).is_err());
        let mut duplicate = epoch.clone();
        duplicate.changes.push(epoch.changes[28].clone());
        assert!(super::super::for_applied_prefix(&duplicate, &applied).is_err());
    }
}
