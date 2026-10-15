//! Secret-safe diagnostics port of local_node/cli.py. The caller owns stdout and
//! argument collection. No credential loading, network, listener or OS probing.
use super::{
    computer::{ComputerDriver, MacOSComputerDriver},
    service::OutboundControlPlane,
};
use serde_json::{Value, json};
pub struct DiagnosticOutput {
    pub exit_code: i32,
    pub report: Option<Value>,
    pub error: Option<&'static str>,
}
pub fn diagnostic_command(arguments: &[String]) -> DiagnosticOutput {
    let endpoint = match arguments {
        [command] if command == "doctor" => None,
        [command, flag, endpoint] if command == "doctor" && flag == "--endpoint" => {
            Some(endpoint.as_str())
        }
        _ => {
            return DiagnosticOutput {
                exit_code: 2,
                report: None,
                error: Some("local-node: error: invalid arguments"),
            };
        }
    };
    let mut endpoint_status = "needs_action";
    let mut transport = "unavailable";
    let mut reason = "outbound endpoint is not configured";
    if let Some(endpoint) = endpoint {
        if (OutboundControlPlane {
            url: endpoint.into(),
        })
        .validate()
        .is_ok()
        {
            endpoint_status = "ready";
            if endpoint.starts_with("https://") {
                transport = "available_not_started";
                reason = "secure credential store and platform verifier require native injection";
            } else {
                transport = "needs_adapter";
                reason = "WSS requires an explicitly injected WebSocket adapter";
            }
        } else {
            endpoint_status = "denied";
            reason = "outbound endpoint is invalid";
        }
    }
    let platform = if cfg!(target_os = "macos") {
        "Darwin"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        std::env::consts::OS
    };
    let driver = MacOSComputerDriver {
        backend: None,
        accessibility_probe: None,
        screen_recording_probe: None,
        platform_name: platform.into(),
    };
    let computer = driver.doctor();
    DiagnosticOutput {
        exit_code: 2,
        error: None,
        report: Some(
            json!({"status":"unavailable","operating_system":platform,"network_started":false,"listener":"disabled","endpoint":endpoint_status,"transport":transport,"secure_credential_storage":"unavailable","platform_signature_verifier":"unavailable","trusted_local_approval":"unavailable","computer":{"status":computer.status,"driver":computer.driver,"accessibility":computer.accessibility,"screen_recording":computer.screen_recording},"reason":reason}),
        ),
    }
}
