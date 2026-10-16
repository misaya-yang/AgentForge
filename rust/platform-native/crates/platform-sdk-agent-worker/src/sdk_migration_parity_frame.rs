use super::*;
#[test]
fn exact_whole_envelope_limit_includes_provider_key_and_newline() {
    let limit = 512 * 1024;
    let overhead = encode_launch_frame(&json!({"padding":""}), "synthetic-provider-key")
        .unwrap()
        .len();
    let value = json!({"padding":"x".repeat(limit-overhead)});
    let wire = encode_launch_frame(&value, "synthetic-provider-key").unwrap();
    assert_eq!(wire.len(), limit);
    assert_eq!(wire.last(), Some(&b'\n'));
    assert_eq!(
        serde_json::from_slice::<Value>(&wire).unwrap()["request"],
        value
    );
    assert_eq!(
        encode_launch_frame(
            &json!({"padding":"x".repeat(limit-overhead+1)}),
            "synthetic-provider-key"
        )
        .unwrap_err(),
        "sdk_request_too_large"
    );
    assert_eq!(
        encode_launch_frame(&json!({}), &"x".repeat(limit)).unwrap_err(),
        "sdk_request_too_large"
    );
}
#[test]
fn unicode_is_encoded_as_complete_compact_utf8() {
    for input in ["中".repeat(80000), "😀".repeat(60000)] {
        let request = json!({"input":input});
        let key = "\\\"".repeat(2048);
        let wire = encode_launch_frame(&request, &key).unwrap();
        assert!(wire.len() < 393216);
        assert!(!wire.windows(6).any(|x| x == b"\\u4e2d"));
        assert_eq!(
            serde_json::from_slice::<Value>(&wire).unwrap(),
            json!({"request":request,"provider_key":key})
        );
    }
}
