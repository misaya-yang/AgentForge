mod contract;
mod engine;
#[path = "../../ai-platform-capability-contract/src/sdk_model_plane.rs"]
mod model_plane;
mod tools;
mod usage;
#[path = "../../ai-platform-capability-contract/src/workload_identity.rs"]
mod workload_identity;
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    request: Value,
    model_plane: model_plane::ModelPlane,
}
pub fn emit(value: &Value) -> Result<()> {
    let line = serde_json::to_vec(value)?;
    ensure!(line.len() <= 1048576, "sdk_event_too_large");
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(&line)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
const MAX_LAUNCH_FRAME: usize = 512 * 1024;
fn read_launch_frame(reader: impl Read) -> Result<Frame> {
    let mut raw = vec![];
    reader
        .take((MAX_LAUNCH_FRAME + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| anyhow::anyhow!("sdk_launch_frame_invalid"))?;
    ensure!(
        raw.len() <= MAX_LAUNCH_FRAME
            && raw.last() == Some(&b'\n')
            && !raw[..raw.len() - 1].contains(&b'\n'),
        "sdk_launch_frame_invalid"
    );
    serde_json::from_slice(&raw).map_err(|_| anyhow::anyhow!("sdk_launch_frame_invalid"))
}
async fn execute() -> Result<()> {
    ensure!(
        ![
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_BASE_URL",
            "ANTHROPIC_BASE_URL"
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some()),
        "sdk_provider_configuration_forbidden"
    );
    let frame = read_launch_frame(std::io::stdin())?;
    frame
        .model_plane
        .validate_launch(&frame.request)
        .map_err(anyhow::Error::msg)?;
    let mut launch = contract::parse_request(frame.request)?;
    launch.validate()?;
    let state = std::path::PathBuf::from(std::env::var("HOME")?);
    ensure!(
        state.is_absolute() && state.is_dir() && !state.is_symlink(),
        "sdk_state_invalid"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(launch.timeout()),
        engine::run(launch, frame.model_plane, &state),
    )
    .await??;
    Ok(())
}
#[tokio::main]
async fn main() {
    if execute().await.is_err() {
        let _ = emit(&json!({"event":"error","code":"sdk_execution_failed"}));
        std::process::exit(1);
    }
}
#[cfg(test)]
mod sdk_migration_parity_frame;
