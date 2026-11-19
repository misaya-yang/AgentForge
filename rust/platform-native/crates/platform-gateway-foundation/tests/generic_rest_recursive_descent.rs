//! Expected values captured from the retained Python jsonpath-ng 1.8.0 owner.
use ai_platform_gateway_foundation::generic_rest_adapter::{RestMappingError, evaluate_expression};
use serde_json::Value;

#[test]
fn recursive_descent_matches_python_fixtures() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/generic_rest_recursive_descent.json")).unwrap();
    for case in cases {
        let actual = evaluate_expression(&case["path"], &case["data"]);
        if case["error"] == true {
            assert_eq!(
                actual,
                Err(RestMappingError::JsonPathEvaluation),
                "{}",
                case["path"]
            );
        } else {
            assert_eq!(actual.unwrap(), case["expected"], "{}", case["path"]);
        }
    }
}

#[test]
fn incomplete_recursive_descent_is_rejected() {
    for path in ["$..", "$...a", "$..[", "$..a.."] {
        assert_eq!(
            evaluate_expression(&Value::String(path.into()), &serde_json::json!({"a": 1})),
            Err(RestMappingError::UnsupportedJsonPath),
            "{path}"
        );
    }
}
