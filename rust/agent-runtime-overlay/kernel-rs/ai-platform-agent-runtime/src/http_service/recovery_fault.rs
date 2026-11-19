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

/// Controlled process-crash boundary for the exact production terminal path.
/// Only debug builds consult this operator-owned file. No public request can
/// select a run or a boundary; the marker contains no prompt/tool payload.
pub(crate) async fn terminal_pause(stage: &str, run_id: uuid::Uuid) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(home) = std::env::var("AI_PLATFORM_AGENT_HOME") else { return; };
    terminal_pause_at(std::path::Path::new(&home), stage, run_id).await;
}

async fn terminal_pause_at(home: &std::path::Path, stage: &str, run_id: uuid::Uuid) {
    let config_path = home.join("terminal-acceptance-fault.json");
    let Ok(metadata) = tokio::fs::symlink_metadata(&config_path).await else { return; };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return;
    }
    let Ok(bytes) = tokio::fs::read(&config_path).await else { return; };
    let Ok(config) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return; };
    if !terminal_fault_matches(&config, stage, run_id) { return; }
    if tokio::fs::rename(&config_path, home.join("terminal-acceptance-consumed.json")).await.is_err() {
        return;
    }
    let marker = home.join("terminal-acceptance-hit.json");
    if tokio::fs::write(&marker, json!({"stage":stage,"run_id":run_id}).to_string()).await.is_err() {
        return;
    }
    tracing::warn!(stage, %run_id, "controlled local terminal acceptance pause");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    while tokio::time::Instant::now() < deadline
        && tokio::fs::try_exists(&marker).await.unwrap_or(false)
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn terminal_fault_matches(config: &serde_json::Value, stage: &str, run_id: uuid::Uuid) -> bool {
    matches!(stage, "terminal_before_transaction" | "terminal_after_row_update"
        | "terminal_after_event" | "terminal_after_commit" | "terminal_core_saved")
        && config["stage"].as_str() == Some(stage)
        && config["run_id"].as_str().and_then(|value| uuid::Uuid::parse_str(value).ok()) == Some(run_id)
}

#[cfg(test)]
mod terminal_fault_tests {
    use super::*;

    #[tokio::test]
    async fn terminal_pause_is_one_shot_and_releases_when_marker_is_removed() {
        let home = tempfile::tempdir().unwrap();
        let run = uuid::Uuid::from_u128(42);
        let stage = "terminal_after_event";
        let config = home.path().join("terminal-acceptance-fault.json");
        tokio::fs::write(&config, json!({"stage":stage,"run_id":run}).to_string()).await.unwrap();
        // Wrong run must neither consume the configuration nor block.
        terminal_pause_at(home.path(), stage, uuid::Uuid::from_u128(43)).await;
        assert!(config.exists());
        let path = home.path().to_path_buf();
        let task = tokio::spawn(async move { terminal_pause_at(&path, stage, run).await; });
        let marker = home.path().join("terminal-acceptance-hit.json");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        assert!(!task.is_finished());
        assert!(!config.exists());
        assert!(home.path().join("terminal-acceptance-consumed.json").exists());
        let hit: serde_json::Value = serde_json::from_slice(&tokio::fs::read(&marker).await.unwrap()).unwrap();
        assert_eq!(hit, json!({"stage":stage,"run_id":run}));
        tokio::fs::remove_file(&marker).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(1), terminal_pause_at(home.path(), stage, run)).await.unwrap();
    }

    #[test]
    fn terminal_fault_requires_exact_run_and_known_boundary() {
        let run = uuid::Uuid::from_u128(1);
        for stage in ["terminal_before_transaction", "terminal_after_row_update",
            "terminal_after_event", "terminal_after_commit", "terminal_core_saved"] {
            let config = json!({"stage":stage,"run_id":run});
            assert!(terminal_fault_matches(&config, stage, run));
            assert!(!terminal_fault_matches(&config, stage, uuid::Uuid::from_u128(2)));
            assert!(!terminal_fault_matches(&config, "unknown", run));
        }
        for config in [json!({}), json!({"stage":"terminal_after_commit"}),
            json!({"stage":"terminal_after_commit","run_id":"*"}),
            json!({"stage":"unknown","run_id":run})] {
            assert!(!terminal_fault_matches(&config, "terminal_after_commit", run));
        }
    }
}
