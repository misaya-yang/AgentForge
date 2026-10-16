use ai_platform_gateway_foundation::generic_rest_adapter::{RestMappingError, evaluate_expression};
use serde_json::Value;
#[test]
fn pinned_base_jsonpath_expression_and_context_goldens() {
    let cases: Value = serde_json::from_str(include_str!(
        "fixtures/generic_rest_expression_context.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let actual = evaluate_expression(&case["path"], &case["data"]);
        if let Some(error) = case["error"].as_str() {
            let expected = if error.starts_with("JsonPath") {
                RestMappingError::UnsupportedJsonPath
            } else {
                RestMappingError::JsonPathEvaluation
            };
            assert_eq!(actual, Err(expected), "{}", case["path"]);
        } else {
            assert_eq!(
                actual.unwrap_or_else(|e| panic!("{}: {e:?}", case["path"])),
                case["expected"],
                "{}",
                case["path"]
            );
        }
    }
}
