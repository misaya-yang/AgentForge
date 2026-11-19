//! Explicit interactive Calculator acceptance. Never called by unit tests or daemon startup.
use crate::{
    acceptance::{
        AcceptanceResult, Temporary, approval_payload, events, generated_at, require, sign,
    },
    approvals::OneUseApproval,
    canonical,
    computer::{ComputerAction, ComputerController},
    digest,
    envelope::{Action, Approval, PlatformVerifier},
    ledger::Ledger,
    macos::{MacComputer, NativeHelper},
    sha256,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const APP: &str = "com.apple.calculator";
const SESSION: &str = "session-macos-native-live-acceptance";
const RUN: &str = "run-macos-native-live-acceptance";
const DEVICE: &str = "device-macos-native-live-acceptance";
const KEY_ID: &str = "macos-live-acceptance-platform-test-key";
const KEY: &[u8] = b"local-node-macos-live-platform-signature-test-only";
const SERVICE: &str = "ai-platform.local-node.trusted-approval.v1";
struct Platform;
impl PlatformVerifier for Platform {
    fn verify(&self, id: &str, payload: &[u8], signature: &str) -> bool {
        let expected = sign(KEY, payload);
        id == KEY_ID
            && expected.len() == signature.len()
            && expected
                .bytes()
                .zip(signature.bytes())
                .fold(0, |n, (a, b)| n | (a ^ b))
                == 0
    }
}
fn command(mut c: Command, timeout: Duration) -> AcceptanceResult<()> {
    let mut child = c
        .env_clear()
        .envs([
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ("LANG", "C"),
            ("LC_ALL", "C"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            require(status.success(), "native acceptance command failed")?;
            return Ok(());
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("native acceptance command timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn build_helper(dir: &Path) -> AcceptanceResult<PathBuf> {
    // The binary remains relocatable; source is embedded, not read from its build checkout.
    let source = dir.join("LocalNodeMacOSHelper.swift");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&source)?;
    std::io::Write::write_all(
        &mut file,
        include_str!("../../../../../apps/local-node/native/macos/LocalNodeMacOSHelper.swift")
            .as_bytes(),
    )?;
    file.sync_all()?;
    let helper = dir.join("local-node-macos-helper");
    let mut c = Command::new("/usr/bin/xcrun");
    c.args([
        "swiftc",
        "-O",
        "-framework",
        "AppKit",
        "-framework",
        "ApplicationServices",
        "-framework",
        "CoreGraphics",
        "-framework",
        "Security",
    ])
    .arg(source)
    .arg("-o")
    .arg(&helper);
    command(c, Duration::from_secs(60))?;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
    Ok(helper)
}
pub fn calculator_value(lines: &[Value]) -> Option<String> {
    for line in lines.iter().filter_map(Value::as_str) {
        if !line.contains("AXStaticText") && !line.contains("AXTextField") {
            continue;
        }
        for c in line.split('|').map(str::trim).rev() {
            if ["AXStaticText", "AXTextField", "AXValue"].contains(&c) {
                continue;
            }
            if c.replace(',', "").parse::<f64>().is_ok() {
                return Some(c.into());
            }
        }
    }
    None
}
pub fn button_center(lines: &[Value], label: &str, wx: f64, wy: f64) -> Option<(f64, f64)> {
    for line in lines.iter().filter_map(Value::as_str) {
        let fields = line.split('|').map(str::trim).collect::<Vec<_>>();
        if fields.first() != Some(&"AXButton")
            || !fields
                .get(..fields.len().saturating_sub(1))?
                .contains(&label)
        {
            continue;
        }
        let tail = line.split_once("frame=")?.1;
        let coords = tail
            .split(',')
            .take(4)
            .map(|v| v.trim().parse::<f64>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()?;
        if coords.len() == 4
            && coords.iter().all(|n| n.is_finite())
            && coords[2] >= 0.
            && coords[3] >= 0.
        {
            return Some((
                coords[0] + coords[2] / 2. - wx,
                coords[1] + coords[3] / 2. - wy,
            ));
        }
    }
    None
}
pub fn run(report_dir: &Path) -> AcceptanceResult<Value> {
    require(
        cfg!(target_os = "macos"),
        "native live acceptance is macOS-only",
    )?;
    require(
        !report_dir.is_symlink(),
        "report directory symlink rejected",
    )?;
    std::fs::create_dir_all(report_dir)?;
    std::fs::set_permissions(report_dir, std::fs::Permissions::from_mode(0o700))?;
    let report_dir = report_dir.canonicalize()?;
    let temp = Temporary::new("ai-platform-macos-native-live")?;
    let helper = build_helper(&temp.0)?;
    let signer = NativeHelper::new(helper.clone())?;
    let verification_helper = NativeHelper::new(helper.clone())?;
    let local = OneUseApproval::open(
        DEVICE.into(),
        &temp.0.join("trusted-local-approvals.sqlite"),
        move |payload, signature| {
            verification_helper.verify_approval(payload, signature, SERVICE, DEVICE)
        },
    )?;
    let ledger_path = temp.0.join("action-ledger.sqlite");
    let mut ledger = Ledger::open(&ledger_path)?;
    let apps = HashSet::from([APP.to_string()]);
    let driver = MacComputer::new(NativeHelper::new(helper)?, apps.clone(), HashSet::new())?
        .with_screen_capture(temp.0.join("screenshots"), true)?;
    let mut controller = ComputerController::new(driver, apps, HashSet::new());
    let mut open = Command::new("/usr/bin/open");
    open.args(["-na", "Calculator"]);
    command(open, Duration::from_secs(10))?;
    std::thread::sleep(Duration::from_millis(500));
    let lease = controller.acquire(APP, SESSION, None, 180)?;
    let before = controller.observe(lease.token())?;
    let details = controller
        .driver
        .last_details()
        .ok_or("native pre-action details unavailable")?;
    require(details["frontmost"] == true, "Calculator not frontmost")?;
    require(
        details["risk_flags"].as_array().is_some_and(Vec::is_empty),
        "sensitive/modal Calculator UI",
    )?;
    let (x, y) = button_center(
        details["accessibility_lines"]
            .as_array()
            .ok_or("accessibility lines absent")?,
        "7",
        details["x"].as_f64().ok_or("window x absent")?,
        details["y"].as_f64().ok_or("window y absent")?,
    )
    .ok_or("Calculator 7 target unavailable")?;
    let primitive = ComputerAction {
        kind: "click".into(),
        observation_id: before.observation_id.clone(),
        arguments: json!({"x":x,"y":y}),
    };
    let arguments = json!({"lease_id":lease.lease_id,"kind":primitive.kind,"observation_id":primitive.observation_id,"arguments":primitive.arguments});
    let target = before.digest()?;
    let at = crate::now();
    let mut approval = Approval {
        approval_id: "macos-local-approval-calculator-seven".into(),
        action_id: "macos-live-calculator-seven".into(),
        device_id: DEVICE.into(),
        arguments_digest: digest(&arguments),
        target_snapshot_digest: target.clone(),
        policy_snapshot_digest: "macos-native-live-policy-v1".into(),
        nonce: "macos-local-approval-nonce-calculator-seven".into(),
        expires_at: at + 180.,
        local_signature: String::new(),
    };
    let summary = format!(
        "Run: {RUN}\nDevice: {DEVICE}\nApplication: {}\nWindow: {}\nOrigin: {}\nOperation: app.control (click)\nArguments SHA-256: {}\nTarget SHA-256: {target}\nRisk: native mouse input to the exact freshly observed Calculator window\n\nApprove this exact action once, or take over and deny.",
        before.app_id,
        before.window_id,
        before
            .origin
            .as_deref()
            .unwrap_or("local desktop application"),
        approval.arguments_digest
    );
    approval.local_signature = signer
        .prompt_and_sign(
            &approval_payload(&approval)?,
            &summary,
            SERVICE,
            DEVICE,
            Duration::from_secs(120),
        )?
        .ok_or("trusted local user denied or timed out")?;
    let mut action: Action = serde_json::from_value(
        json!({"action_id":approval.action_id,"idempotency_key":"macos-live-idempotency-calculator-seven","tenant_id":"tenant-macos-live-acceptance","user_id":"user-macos-live-acceptance","session_id":SESSION,"run_id":RUN,"agent_id":"canonical-agent-loop","agent_version":"macos-live-v1","call_id":"call-macos-live-calculator-seven","device_id":DEVICE,"envelope_version":1,"capability":"app.control","tool_name":"local_app_control","operation":"app.control","capability_lease_id":lease.lease_id,"resource_refs":[lease.lease_id,before.app_id,before.window_id],"arguments_digest":digest(&arguments),"target_snapshot_digest":target,"policy_snapshot_digest":"macos-native-live-policy-v1","nonce":"macos-platform-action-nonce-calculator-seven","issued_at":at,"expires_at":at+180.,"platform_key_id":KEY_ID,"platform_signature":"","approval":approval,"trace_context":null}),
    )?;
    action.platform_signature = sign(KEY, canonical(&action.signed_payload()).as_bytes());
    let result = controller.execute(
        lease.token(),
        &primitive,
        &action,
        &mut ledger,
        &Platform,
        &local,
    )?;
    let after = controller
        .driver
        .last_details()
        .ok_or("native post-action details unavailable")?;
    let readback = calculator_value(
        after["accessibility_lines"]
            .as_array()
            .ok_or("accessibility lines absent")?,
    )
    .ok_or("Calculator readback unavailable")?;
    require(
        readback == "7" && result.status == "succeeded",
        "Calculator readback mismatch",
    )?;
    let screenshot = controller
        .driver
        .last_observation()
        .and_then(|o| o.screenshot_ref.as_ref())
        .ok_or("post-action screenshot missing")?;
    let artifact = report_dir.join("calculator-local-node-native-live.png");
    require(
        !artifact.is_symlink(),
        "screenshot destination symlink rejected",
    )?;
    std::fs::copy(screenshot, &artifact)?;
    std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o600))?;
    let artifact_hash = sha256(&std::fs::read(&artifact)?);
    let stopped = Instant::now();
    require(controller.stop(None)?, "emergency stop did not close lease")?;
    let latency = stopped.elapsed().as_secs_f64() * 1000.;
    require(
        matches!(
            controller.execute(
                lease.token(),
                &primitive,
                &action,
                &mut ledger,
                &Platform,
                &local
            ),
            Err(crate::Error::Denied(_))
        ),
        "post-stop input accepted",
    )?;
    require(latency < 2000., "emergency stop exceeded two seconds")?;
    ledger.verify()?;
    let action_events = events(&ledger_path)?
        .iter()
        .filter(|e| e["action_id"] == action.action_id)
        .map(|e| e["event_type"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    require(
        action_events
            == [
                "policy_check",
                "awaiting_approval",
                "dispatched",
                "running",
                "observed",
                "succeeded",
            ],
        "native ledger order differs",
    )?;
    Ok(
        json!({"schema_version":1,"generated_at":generated_at(),"overall_status":"passed","host":{"system":"Darwin","machine":std::env::consts::ARCH,"implementation":"Rust"},"driver":"macos-native-ax-cgevent-v1","permissions":{"accessibility":true,"screen_recording":true},"scope":{"session_id":SESSION,"app_id":APP,"window_id":before.window_id,"origin":before.origin},"action":{"action_id":action.action_id,"primitive":"click","arguments_digest":action.arguments_digest,"target_snapshot_digest":target,"approval_device_id":DEVICE,"approval_nonce":approval.nonce,"terminal":"succeeded","ledger_events":action_events,"before_observation_id":result.before_observation_id,"after_observation_id":result.after_observation_id,"readback":readback},"screenshot":{"path":artifact,"sha256":artifact_hash},"takeover_stop":{"latency_ms":latency,"post_stop_input_denied":true,"post_stop_denial_code":"capability_denied"},"evidence_tier":{"E2_local_live":["real Local Node Swift Accessibility/CoreGraphics helper","real Calculator window screenshot/action/accessibility read-back","native Keychain-backed exact-intent local approval","session/window/latest-observation binding and emergency stop"],"E1_only":["test-only platform action-envelope HMAC signer"],"not_claimed":["browser Computer Use","TextEdit file save","OpenAI provider path"]}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calculator_accessibility_projection() {
        assert_eq!(
            calculator_value(&[json!("AXStaticText | AXValue | 7")]),
            Some("7".into())
        );
        assert_eq!(
            button_center(
                &[json!("AXButton | 7 | frame=100,200,40,60")],
                "7",
                80.,
                150.
            ),
            Some((40., 80.))
        );
    }
}
