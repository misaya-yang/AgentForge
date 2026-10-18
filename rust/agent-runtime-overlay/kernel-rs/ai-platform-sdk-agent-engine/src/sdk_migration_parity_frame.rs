use super::*;
use std::io::Cursor;
const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn plane(lease: &str) -> Value {
    json!({"schema":"sdk-model-plane/v1","url":"https://gateway/internal/v1/sdk-tools/models/execute","revocation_url":"https://gateway/internal/v1/sdk-tools/models/revocations","lease":lease,"run_id":"00000000-0000-0000-0000-000000000001","expires_at_epoch_ms":model_plane::now_ms()+60000,"binding_sha256":"a".repeat(64)})
}
fn wire(request: Value, key: &str) -> Vec<u8> {
    let mut raw = serde_json::to_vec(&json!({"request":request,"model_plane":plane(key)})).unwrap();
    raw.push(b'\n');
    raw
}
#[test]
fn chinese_and_emoji_preserve_admitted_input() {
    for text in ["中".repeat(80_000), "😀".repeat(60_000)] {
        let body = json!({"input":text});
        let raw = wire(body.clone(), KEY);
        assert!(raw.len() < 393216);
        let frame = read_launch_frame(Cursor::new(raw)).unwrap();
        assert_eq!(frame.request, body);
        assert_eq!(frame.model_plane.lease, KEY);
    }
}
#[test]
fn exact_whole_envelope_boundary_and_max_plus_one() {
    let overhead = wire(json!({"padding":""}), KEY).len();
    let body = json!({"padding":"x".repeat(MAX_LAUNCH_FRAME-overhead)});
    let raw = wire(body.clone(), KEY);
    assert_eq!(raw.len(), MAX_LAUNCH_FRAME);
    assert_eq!(read_launch_frame(Cursor::new(&raw)).unwrap().request, body);
    let mut too_big = raw;
    too_big.push(b'x');
    assert!(read_launch_frame(Cursor::new(too_big)).is_err());
}
#[test]
fn lease_and_metadata_count_toward_budget() {
    let body = json!({"input":"中文😀","read_bridge":{"grant":"a".repeat(64),"dataset_ids":["00000000-0000-4000-8000-000000000001"]}});
    let key = "\\\"".repeat(2048);
    let raw = wire(body.clone(), &key);
    assert!(raw.len() > key.len() + serde_json::to_vec(&body).unwrap().len());
    let frame = read_launch_frame(Cursor::new(raw)).unwrap();
    assert_eq!(frame.request, body);
    assert_eq!(frame.model_plane.lease, key);
    assert!(
        read_launch_frame(Cursor::new(wire(json!({}), &"x".repeat(MAX_LAUNCH_FRAME)))).is_err()
    );
}
#[test]
fn malformed_frames_fail_closed_without_contents() {
    let valid = wire(json!({"input":"hello"}), KEY);
    let cases = vec![
        vec![],
        valid[..valid.len() - 1].to_vec(),
        valid[..25].to_vec(),
        [valid.clone(), valid.clone()].concat(),
        b"\xff\n".to_vec(),
        b"[]\n".to_vec(),
        b"{\"request\":{},\"provider_key\":\"x\",\"extra\":1}\n".to_vec(),
        b"{\"request\":{},\"provider_key\":null}\n".to_vec(),
    ];
    for raw in cases {
        let err = read_launch_frame(Cursor::new(raw))
            .err()
            .unwrap()
            .to_string();
        assert_eq!(err, "sdk_launch_frame_invalid");
        assert!(!err.contains(KEY));
    }
}
#[test]
fn nonfinite_and_malformed_unicode_are_rejected() {
    for value in ["NaN", "Infinity", "1e999", "\"\\ud800\""] {
        let raw = format!("{{\"request\":{{\"value\":{value}}},\"provider_key\":\"{KEY}\"}}\n");
        assert_eq!(
            read_launch_frame(Cursor::new(raw))
                .err()
                .unwrap()
                .to_string(),
            "sdk_launch_frame_invalid"
        );
    }
}
#[test]
fn reader_never_consumes_past_bounded_eof_probe() {
    struct Counting {
        read: usize,
    }
    impl Read for Counting {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            out.fill(b'x');
            self.read += out.len();
            Ok(out.len())
        }
    }
    let mut reader = Counting { read: 0 };
    assert!(read_launch_frame(&mut reader).is_err());
    assert_eq!(reader.read, MAX_LAUNCH_FRAME + 1);
}
