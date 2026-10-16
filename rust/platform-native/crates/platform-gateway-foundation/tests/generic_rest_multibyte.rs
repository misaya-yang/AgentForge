use ai_platform_gateway_foundation::generic_rest_adapter::{
    RestResponse, decode_http_response_bytes,
};
use serde_json::{Value, json};
#[test]
fn multibyte_text_matches_source_incremental_codec_replacement() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/generic_rest_multibyte.json")).unwrap();
    assert_eq!(cases.len(), 1190);
    for case in cases {
        let bytes: Vec<u8> = case["bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        let response = decode_http_response_bytes(
            200,
            &format!("text/plain; charset={}", case["encoding"].as_str().unwrap()),
            &bytes,
        )
        .unwrap();
        let expected = if let Some(value) = case.get("json") {
            RestResponse::Json(value.clone())
        } else {
            RestResponse::Json(json!({"raw":case["expected"]}))
        };
        assert_eq!(response, expected, "{} {:?}", case["encoding"], bytes);
    }
}
