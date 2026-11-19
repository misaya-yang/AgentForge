//! reports/repro_eval_label.py updated to the current source's existing fix.
use ai_platform_gateway::migration_compat::eval_scoring::canonical_judge_label;
use serde_json::json;
#[test]
fn excellent_numeric_judgment_retains_a_scoreable_canonical_label() {
    let judge = json!({"numeric_value":0.95,"label":"excellent","explanation":"Correct and well grounded.","confidence":0.9});
    assert_eq!(canonical_judge_label(&judge, 0.95), "pass");
    assert_eq!(
        canonical_judge_label(&json!({"label":" FAIL "}), 0.95),
        "fail"
    );
    assert_eq!(
        canonical_judge_label(&json!({"label":"uncertain"}), 0.5),
        "review"
    );
}
