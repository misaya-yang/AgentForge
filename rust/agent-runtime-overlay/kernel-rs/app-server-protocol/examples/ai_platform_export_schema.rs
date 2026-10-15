//! Build-time export through the same protocol library used by the hosted runtime.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("expected schema output directory"))?;
    codex_app_server_protocol::generate_types(&output, None)
}
