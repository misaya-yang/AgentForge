//! Local dev-small acceptance pauses. Release binaries never honor these files.
//! Operator-owned, exact Quiz title, one-shot configuration; no public API.

use codex_app_server_protocol::DynamicToolCallParams;
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) async fn pause(stage: &str, params: &DynamicToolCallParams, cancel: &CancellationToken) {
    if !cfg!(debug_assertions) || params.tool != "generate_quiz" {
        return;
    }
    let Some(title) = params.arguments["title"].as_str() else {
        return;
    };
    if !title.starts_with("DR-CRASH-") {
        return;
    }
    let Ok(home) = std::env::var("AI_PLATFORM_AGENT_HOME") else {
        return;
    };
    let config_path = std::path::Path::new(&home).join("recovery-acceptance-fault.json");
    let Ok(metadata) = tokio::fs::symlink_metadata(&config_path).await else {
        return;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return;
    }
    let Ok(bytes) = tokio::fs::read(&config_path).await else {
        return;
    };
    let Ok(config) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    if config["stage"].as_str() != Some(stage) || config["quiz_title"].as_str() != Some(title) {
        return;
    }
    let consumed = std::path::Path::new(&home).join("recovery-acceptance-consumed.json");
    if tokio::fs::rename(&config_path, &consumed).await.is_err() {
        return;
    }
    let marker = std::path::Path::new(&home).join("recovery-acceptance-hit.json");
    let hit = json!({"stage":stage,"run_id":params.turn_id,"call_id":params.call_id});
    if tokio::fs::write(&marker, hit.to_string()).await.is_err() {
        return;
    }
    tracing::warn!(stage, run_id=%params.turn_id, "controlled local recovery acceptance pause");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
        if tokio::time::Instant::now() >= deadline
            || !tokio::fs::try_exists(&marker).await.unwrap_or(false)
        {
            break;
        }
    }
}

pub(crate) async fn armed(stage: &str) -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    let Ok(home) = std::env::var("AI_PLATFORM_AGENT_HOME") else {
        return false;
    };
    let path = std::path::Path::new(&home).join("recovery-acceptance-fault.json");
    let Ok(bytes) = tokio::fs::read(path).await else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .is_some_and(|config| config["stage"].as_str() == Some(stage))
}
