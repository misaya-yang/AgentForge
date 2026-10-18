use super::*;
fn plane() -> crate::model_plane::ModelPlane {
    crate::model_plane::ModelPlane {
        schema: "sdk-model-plane/v1".into(),
        url: "https://gateway/internal/v1/sdk-tools/models/execute".into(),
        revocation_url: "https://gateway/internal/v1/sdk-tools/models/revocations".into(),
        lease: "a".repeat(64),
        run_id: "00000000-0000-0000-0000-000000000001".into(),
        binding_sha256: "a".repeat(64),
        expires_at_epoch_ms: crate::model_plane::now_ms() + 60_000,
    }
}
#[test]
fn exact_whole_envelope_limit_includes_model_lease_and_newline() {
    let limit = 512 * 1024;
    let plane = plane();
    let overhead = encode_launch_frame(&json!({"padding":""}), &plane)
        .unwrap()
        .len();
    let value = json!({"padding":"x".repeat(limit-overhead)});
    let wire = encode_launch_frame(&value, &plane).unwrap();
    assert_eq!(wire.len(), limit);
    assert_eq!(wire.last(), Some(&b'\n'));
    assert_eq!(
        serde_json::from_slice::<Value>(&wire).unwrap()["request"],
        value
    );
    assert_eq!(
        encode_launch_frame(&json!({"padding":"x".repeat(limit-overhead+1)}), &plane).unwrap_err(),
        "sdk_request_too_large"
    );
    assert!(!String::from_utf8(wire).unwrap().contains("provider_key"));
}
#[test]
fn unicode_is_encoded_as_complete_compact_utf8() {
    for input in ["中".repeat(80000), "😀".repeat(60000)] {
        let request = json!({"input":input});
        let plane = plane();
        let wire = encode_launch_frame(&request, &plane).unwrap();
        assert!(wire.len() < 393216);
        assert!(!wire.windows(6).any(|x| x == b"\\u4e2d"));
        assert_eq!(
            serde_json::from_slice::<Value>(&wire).unwrap(),
            json!({"request":request,"model_plane":plane})
        );
    }
}
