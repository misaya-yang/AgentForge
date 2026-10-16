use std::io::Write;
use std::process::{Command, Stdio};
#[test]
fn invalid_frame_fails_before_state_access_or_engine_and_redacts() {
    let home = tempfile::tempdir().unwrap();
    let absent = home.path().join("must-not-be-created");
    for frame in [
        b"{\"request\":{},\"provider_key\":\"synthetic-provider-key\"}".as_slice(),
        b"[]\n",
        b"\xff\n",
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ai-platform-sdk-agent-engine"))
            .env("HOME", &absent)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(frame).unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(!result.status.success());
        let body: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"event":"error","code":"sdk_execution_failed"})
        );
        assert!(result.stderr.is_empty());
        assert!(!absent.exists());
    }
}
