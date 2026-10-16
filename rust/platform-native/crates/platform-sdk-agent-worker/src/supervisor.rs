//! An owned task retains the session lock through process-group cleanup and commit.
//! Disconnects and shutdown notify that task; they never abort cleanup.
use crate::{
    App,
    contract::{Launch, MAX_EVENT, MAX_OUTPUT, Result},
    lifecycle::{self, Session},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};
pub struct Owner {
    pub uncertain: AtomicBool,
    pub session: Session,
}
pub fn start(
    app: Arc<App>,
    session: Session,
    launch: Launch,
    receipt: PathBuf,
    fingerprint: String,
    tx: mpsc::Sender<Value>,
) -> Result<()> {
    let owner = Arc::new(Owner {
        uncertain: AtomicBool::new(false),
        session,
    });
    {
        let mut owners = app.owners.lock().unwrap();
        if app.closing.load(Ordering::SeqCst) || owners.contains_key(&launch.scope) {
            return Err("sdk_state_unavailable");
        }
        // Register before writing the marker so readiness never sees a false orphan.
        owners.insert(launch.scope.clone(), owner.clone());
        if lifecycle::begin_cleanup(&owner.session).is_err() {
            owner.uncertain.store(true, Ordering::SeqCst);
            return Err("sdk_state_unavailable");
        }
    }
    tokio::spawn(async move {
        use futures_util::FutureExt;
        let retained = owner.clone();
        if std::panic::AssertUnwindSafe(own(app, owner, launch, receipt, fingerprint, tx))
            .catch_unwind()
            .await
            .is_err()
        {
            retained.uncertain.store(true, Ordering::SeqCst);
        }
    });
    Ok(())
}
async fn line(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let chunk = reader
            .fill_buf()
            .await
            .map_err(|_| "sdk_execution_failed")?;
        if chunk.is_empty() {
            return if line.is_empty() {
                Err("sdk_stream_incomplete")
            } else {
                Err("sdk_event_invalid")
            };
        }
        let n = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(chunk.len());
        if line.len() + n > MAX_EVENT {
            return Err("sdk_output_limit");
        }
        let done = chunk[n - 1] == b'\n';
        line.extend_from_slice(&chunk[..n]);
        reader.consume(n);
        if done {
            return Ok(line);
        }
    }
}
fn encode_launch_frame(body: &Value, key: &str) -> Result<Vec<u8>> {
    let mut frame = serde_json::to_vec(&json!({"request":body,"provider_key":key}))
        .map_err(|_| "sdk_request_invalid")?;
    frame.push(b'\n');
    if frame.len() > 512 * 1024 {
        return Err("sdk_request_too_large");
    }
    Ok(frame)
}
async fn execute(child: &mut Child, launch: &Launch, tx: &mpsc::Sender<Value>) -> Result<Value> {
    let frame = encode_launch_frame(&launch.body, &launch.key)?;
    let mut stdin = child.stdin.take().ok_or("sdk_execution_failed")?;
    stdin
        .write_all(&frame)
        .await
        .map_err(|_| "sdk_execution_failed")?;
    stdin.shutdown().await.map_err(|_| "sdk_execution_failed")?;
    drop(stdin);
    drop(frame);
    let mut stdout = BufReader::new(child.stdout.take().ok_or("sdk_execution_failed")?);
    let mut total = 0usize;
    loop {
        let bytes = line(&mut stdout).await?;
        total = total.checked_add(bytes.len()).ok_or("sdk_output_limit")?;
        if total > MAX_OUTPUT {
            return Err("sdk_output_limit");
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "sdk_event_invalid")?;
        match value["event"].as_str() {
            Some("completed") => {
                if !value["text"].is_string() || !value["usage"].is_object() {
                    return Err("sdk_terminal_invalid");
                }
                if !child
                    .wait()
                    .await
                    .map_err(|_| "sdk_child_failed")?
                    .success()
                {
                    return Err("sdk_child_failed");
                }
                return Ok(value);
            }
            Some("delta") => {
                if !value["text"].is_string() {
                    return Err("sdk_event_invalid");
                }
            }
            Some("approval_required" | "approval_result" | "tool_result") => {
                let Some(b) = launch.bridge("capability_bridge") else {
                    return Err("sdk_event_invalid");
                };
                if b["tools"].as_array().is_none_or(|t| t.is_empty())
                    || value["run_id"] != b["run_id"]
                    || !value["tool_call_id"].is_string()
                {
                    return Err("sdk_event_invalid");
                }
            }
            _ => return Err("sdk_child_failed"),
        }
        tx.send(value)
            .await
            .map_err(|_| "sdk_execution_cancelled")?;
    }
}
fn signal_group(pid: u32, signal: i32) -> Result<bool> {
    if unsafe { libc::kill(-(pid as i32), signal) } == 0 {
        return Ok(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        // macOS reports EPERM for a process group containing only its zombie
        // leader. Reap that leader and probe the original group before deciding
        // whether cleanup is unresolved.
        Some(libc::EPERM) => Err("sdk_cleanup_permission_denied"),
        _ => Err("sdk_cleanup_unresolved"),
    }
}
async fn confirm_group_gone_after_leader_exit(child: &mut Child, pid: u32) -> Result<()> {
    if child.try_wait().map_err(|_| "sdk_cleanup_unresolved")?.is_none() {
        // A process-group probe can report EPERM while the leader is still
        // exiting. Stop and reap the owned leader before deciding whether the
        // original group still contains descendants.
        if child.start_kill().is_err()
            && child.try_wait().map_err(|_| "sdk_cleanup_unresolved")?.is_none()
        {
            return Err("sdk_cleanup_unresolved");
        }
    }
    tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .map_err(|_| "sdk_cleanup_unresolved")?
        .map_err(|_| "sdk_cleanup_unresolved")?;
    match signal_group(pid, 0) {
        Ok(false) => Ok(()),
        _ => Err("sdk_cleanup_unresolved"),
    }
}
async fn cleanup(child: &mut Child, pid: u32) -> Result<()> {
    // A reaped leader does not prove its descendants exited. Always address group.
    match signal_group(pid, libc::SIGTERM) {
        Ok(true) => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            match signal_group(pid, libc::SIGKILL) {
                Ok(_) => {}
                Err("sdk_cleanup_permission_denied") => {
                    confirm_group_gone_after_leader_exit(child, pid).await?;
                }
                Err(_) => return Err("sdk_cleanup_unresolved"),
            }
        }
        Ok(false) => {}
        // A zombie leader may make the initial group signal return EPERM.
        // Continue to the group SIGKILL and then reap/probe before releasing
        // the session; do not mistake this for proof that the group is gone.
        Err("sdk_cleanup_permission_denied") => {}
        Err(_) => return Err("sdk_cleanup_unresolved"),
    }
    confirm_group_gone_after_leader_exit(child, pid).await
}
async fn own(
    app: Arc<App>,
    owner: Arc<Owner>,
    launch: Launch,
    receipt: PathBuf,
    fingerprint: String,
    tx: mpsc::Sender<Value>,
) {
    let failure = json!({"event":"error","code":"sdk_execution_failed"});
    let request_id = launch.body["request_id"].as_str().expect("validated launch request ID").to_owned();
    let mut shutdown = app.stop.subscribe();
    if *shutdown.borrow() || tx.is_closed() {
        finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, false).await;
        return;
    }
    let mut command = Command::new(&app.config.engine);
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &owner.session.path)
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(gateway) = &app.config.gateway {
        command.env("SDK_AGENT_GATEWAY_URL", gateway);
    }
    if let Some(base_url) = &app.config.openai_base_url {
        command.env("OPENAI_BASE_URL", base_url);
    }
    if let Some(base_url) = &app.config.anthropic_base_url {
        command.env("ANTHROPIC_BASE_URL", base_url);
    }
    // Unix process group is established before exec, so descendants share cleanup.
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let uncertain = e.kind() != std::io::ErrorKind::NotFound;
            finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, uncertain).await;
            return;
        }
    };
    let Some(pid) = child.id() else {
        finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, true).await;
        return;
    };
    let result = {
        let execution = execute(&mut child, &launch, &tx);
        tokio::pin!(execution);
        let authority = async {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                app.authority.capability(&launch).await?;
            }
            #[allow(unreachable_code)]
            Ok::<(), &'static str>(())
        };
        tokio::pin!(authority);
        tokio::select! {
            result=&mut execution=>result,
            _=tx.closed()=>Err("sdk_execution_cancelled"),
            _=shutdown.changed()=>Err("sdk_execution_cancelled"),
            _=tokio::time::sleep(Duration::from_secs(launch.timeout()))=>Err("sdk_execution_timeout"),
            _=&mut authority=>Err("sdk_execution_cancelled"),
        }
    };
    if cleanup(&mut child, pid).await.is_err() {
        finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, true).await;
        return;
    }
    let value = match result {
        Ok(value) if !tx.is_closed() && !*shutdown.borrow() => value,
        _ => {
            finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, false).await;
            return;
        }
    };
    // Revalidate cancellation authority at commit after physical child cleanup.
    let admitted = tokio::select! {r=app.authority.capability(&launch)=>r,_=tx.closed()=>Err("sdk_execution_cancelled"),_=shutdown.changed()=>Err("sdk_execution_cancelled")};
    if admitted.is_err() || tx.is_closed() || *shutdown.borrow() {
        finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, false).await;
        return;
    }
    // Synchronous durable publication is indivisible with respect to task cancellation.
    let committed = lifecycle::atomic(&receipt, &json!({"fingerprint":fingerprint,"result":value}))
        .and_then(|_| {
            std::fs::remove_file(owner.session.path.join("pending.json"))
                .map_err(|_| "sdk_state_unavailable")
        })
        .and_then(|_| lifecycle::sync(&owner.session.path));
    if committed.is_err() {
        finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, failure, true).await;
        return;
    }
    finish(&app, &owner, &launch.scope, &request_id, &fingerprint, &tx, value, false).await;
}
async fn finish(
    app: &Arc<App>,
    owner: &Arc<Owner>,
    scope: &str,
    request_id: &str,
    fingerprint: &str,
    tx: &mpsc::Sender<Value>,
    mut value: Value,
    uncertain: bool,
) {
    if uncertain || lifecycle::complete_cleanup(&owner.session, request_id, fingerprint).is_err() {
        owner.uncertain.store(true, Ordering::SeqCst);
        value = json!({"event":"error","code":"sdk_execution_failed"});
    } else {
        app.owners.lock().unwrap().remove(scope);
    }
    // Terminal delivery cannot extend lock lifetime. A slow reader is still bounded.
    let _ = tokio::time::timeout(Duration::from_secs(5), tx.send(value)).await;
}

#[cfg(test)]
#[path = "sdk_migration_parity_frame.rs"]
mod sdk_migration_parity_frame;
