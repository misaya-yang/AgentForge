//! Explicit local acceptance. Constructors are inert; run() touches only its fresh workspace.
//! HMAC keys below are public test fixtures, never production device credentials.
use crate::{
    approvals::OneUseApproval,
    canonical,
    computer::{ComputerAction, ComputerController, ComputerDriver, Observation},
    digest,
    envelope::{Action, Approval, PlatformVerifier},
    files::FileService,
    ledger::Ledger,
    processes::{ProcessControl, ProcessPolicy, ProcessRequest, ProcessRunner},
    service::{LocalNodeRuntime, LocalServiceBinding, OutboundControlPlane},
    sha256,
    watcher::Watcher,
    workspace::Grant,
};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, HashSet},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
pub type AcceptanceResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const TENANT: &str = "tenant-live-acceptance";
pub const USER: &str = "user-live-acceptance";
pub const DEVICE: &str = "device-live-acceptance";
const PLATFORM_KEY: &[u8] = b"ai-platform-local-node-live-acceptance-test-only";
const LOCAL_KEY: &[u8] = b"ai-platform-local-node-live-local-approval-test-only";
pub fn require(ok: bool, message: &str) -> AcceptanceResult<()> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}
pub fn sign(key: &[u8], bytes: &[u8]) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC arbitrary key");
    m.update(bytes);
    m.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn signature_matches(expected: &str, got: &str) -> bool {
    expected.len() == got.len()
        && expected
            .bytes()
            .zip(got.bytes())
            .fold(0, |v, (a, b)| v | (a ^ b))
            == 0
}
pub struct TestPlatform;
impl PlatformVerifier for TestPlatform {
    fn verify(&self, key: &str, bytes: &[u8], signature: &str) -> bool {
        key == "live-acceptance-test-key"
            && signature_matches(&sign(PLATFORM_KEY, bytes), signature)
    }
}
pub struct Temporary(pub PathBuf);
impl Temporary {
    pub fn new(prefix: &str) -> AcceptanceResult<Self> {
        let p = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p)?;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self(p))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn generated_at() -> String {
    let seconds = crate::now() as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    unsafe {
        libc::gmtime_r(&seconds, tm.as_mut_ptr());
        let t = tm.assume_init();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            t.tm_year + 1900,
            t.tm_mon + 1,
            t.tm_mday,
            t.tm_hour,
            t.tm_min,
            t.tm_sec
        )
    }
}
pub fn action(
    name: &str,
    capability: &str,
    tool: &str,
    operation: &str,
    args: Value,
    target: &str,
    lease: &str,
    refs: Vec<String>,
    approved: bool,
) -> AcceptanceResult<Action> {
    let at = crate::now();
    let mut a: Action = serde_json::from_value(
        json!({"action_id":format!("live-{name}"),"idempotency_key":format!("live-idempotency-{name}"),"tenant_id":TENANT,"user_id":USER,"session_id":"session-live-acceptance","run_id":"run-live-acceptance","agent_id":"canonical-agent-loop","agent_version":"local-live-v1","call_id":format!("call-{name}"),"device_id":DEVICE,"envelope_version":1,"capability":capability,"tool_name":tool,"operation":operation,"capability_lease_id":lease,"resource_refs":refs,"arguments_digest":digest(&args),"target_snapshot_digest":target,"policy_snapshot_digest":"local-live-policy-v1","nonce":format!("nonce-{name}"),"issued_at":at,"expires_at":at+120.,"platform_key_id":"live-acceptance-test-key","platform_signature":"","approval":null,"trace_context":null}),
    )?;
    if approved {
        let mut p = Approval {
            approval_id: format!("approval-{name}"),
            action_id: a.action_id.clone(),
            device_id: DEVICE.into(),
            arguments_digest: a.arguments_digest.clone(),
            target_snapshot_digest: target.into(),
            policy_snapshot_digest: a.policy_snapshot_digest.clone(),
            nonce: format!("local-approval-{name}"),
            expires_at: a.expires_at,
            local_signature: String::new(),
        };
        p.local_signature = sign(LOCAL_KEY, &approval_payload(&p)?);
        a.approval = Some(p);
    }
    a.platform_signature = sign(PLATFORM_KEY, canonical(&a.signed_payload()).as_bytes());
    Ok(a)
}
pub fn approval_payload(p: &Approval) -> AcceptanceResult<Vec<u8>> {
    let mut v = serde_json::to_value(p)?;
    v.as_object_mut().unwrap().remove("local_signature");
    Ok(canonical(&v).into_bytes())
}
pub fn events(path: &Path) -> AcceptanceResult<Vec<Value>> {
    let db = rusqlite::Connection::open(path)?;
    let mut q =
        db.prepare("SELECT seq,action_id,event_type,payload_json FROM events ORDER BY seq")?;
    let rows=q.query_map([],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"action_id":r.get::<_,String>(1)?,"event_type":r.get::<_,String>(2)?,"payload_json":r.get::<_,String>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(rows)
}
fn grant(path: &Path, id: &str) -> crate::Result<Grant> {
    Grant::issue(
        id.into(),
        path,
        TENANT.into(),
        USER.into(),
        &["list", "read", "search", "watch", "write", "rollback"],
        None,
    )
}
fn watch(root: &Path, g: Arc<Grant>) -> AcceptanceResult<Value> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = stop.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut w = Watcher::default();
    w.scan(&g, TENANT, USER)?;
    let worker = std::thread::spawn(move || {
        while !stop_worker.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
            match w.scan(&g, TENANT, USER) {
                Ok(events) => {
                    for e in events {
                        if tx.send(Ok((Instant::now(), e))).is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    let result = (|| {
        let source = root.join("watch-source.txt");
        let renamed = root.join("watch-renamed.txt");
        let mut observed = vec![];
        let mut latencies = vec![];
        for index in 0..4 {
            let started = Instant::now();
            let (kind, path, previous) = match index {
                0 => {
                    std::fs::write(&source, b"LOCAL_NODE_WATCH_BODY_MUST_NOT_BE_EMITTED")?;
                    ("create", "watch-source.txt", None)
                }
                1 => {
                    std::fs::write(&source, b"changed metadata body")?;
                    ("modify", "watch-source.txt", None)
                }
                2 => {
                    std::fs::rename(&source, &renamed)?;
                    ("rename", "watch-renamed.txt", Some("watch-source.txt"))
                }
                _ => {
                    std::fs::remove_file(&renamed)?;
                    ("delete", "watch-renamed.txt", None)
                }
            };
            loop {
                let remaining = Duration::from_secs(2)
                    .checked_sub(started.elapsed())
                    .ok_or("watch event exceeded two seconds")?;
                let (arrived, event) = rx.recv_timeout(remaining)??;
                if event.kind == kind
                    && event.relative_path == path
                    && event.previous_path.as_deref() == previous
                {
                    latencies.push(arrived.duration_since(started).as_secs_f64() * 1000.);
                    observed.push(event);
                    break;
                }
            }
        }
        require(
            !serde_json::to_string(&observed)?
                .contains("LOCAL_NODE_WATCH_BODY_MUST_NOT_BE_EMITTED"),
            "watch content leaked",
        )?;
        let p95 = latencies.iter().copied().fold(0., f64::max);
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
            json!({"status":"passed","event_kinds":observed.iter().map(|e|&e.kind).collect::<Vec<_>>(),"event_sequences":observed.iter().map(|e|e.sequence).collect::<Vec<_>>(),"latency_ms":latencies,"sample_p95_ms":p95,"metadata_only":true,"stopped":true,"scope_note":"four-event live smoke; not the required 15-minute OS-A24 soak"}),
        )
    })();
    stop.store(true, Ordering::SeqCst);
    worker.join().map_err(|_| "watcher thread panicked")?;
    result
}
fn process_action(name: &str, r: &ProcessRequest, g: &Grant) -> AcceptanceResult<Action> {
    let info = g.open_dir(&r.cwd)?.metadata()?;
    let target =
        digest(&json!({"grant_id":g.id,"cwd":r.cwd,"device":info.dev(),"inode":info.ino()}));
    action(
        name,
        "process.run",
        "local_process_run",
        "process.run",
        r.arguments(),
        &target,
        &g.id,
        vec![g.id.clone(), r.cwd.clone()],
        true,
    )
}
async fn wait_running(path: &Path, id: &str) -> AcceptanceResult<()> {
    let db = rusqlite::Connection::open(path)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let status: Option<String> = db
            .query_row("SELECT status FROM actions WHERE action_id=?", [id], |r| {
                r.get(0)
            })
            .ok();
        if status.as_deref() == Some("running") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("timed out waiting for real local state transition".into())
}
struct NoComputer;
impl ComputerDriver for NoComputer {
    fn ready(&mut self) -> bool {
        false
    }
    fn observe(&mut self, _: &str, _: Option<&str>) -> crate::Result<Observation> {
        Err(crate::Error::Denied("computer not claimed"))
    }
    fn execute(&mut self, _: &ComputerAction) -> crate::Result<()> {
        Err(crate::Error::Denied("computer not claimed"))
    }
    fn stop(&mut self) -> crate::Result<()> {
        Ok(())
    }
}
pub async fn run() -> AcceptanceResult<Value> {
    let temp = Temporary::new("ai-platform-local-node-live")?;
    let root = temp.0.join("workspace");
    let state = temp.0.join("state");
    std::fs::create_dir_all(root.join("docs/nested"))?;
    std::fs::create_dir(&state)?;
    let g = Arc::new(grant(&root, "grant-live")?);
    let ledger_path = state.join("ledger.sqlite");
    let mut ledger = Ledger::open(&ledger_path)?;
    let approval = OneUseApproval::open(
        DEVICE.into(),
        &state.join("trusted-local-approvals.sqlite"),
        |payload, sig| signature_matches(&sign(LOCAL_KEY, payload), sig),
    )?;
    let files = FileService::new(state.join("rollback"))?;
    let fixtures: [(&str, &[u8]); 4] = [
        (
            "docs/alpha.txt",
            b"alpha\nOpenClaw Hermes local node\nomega\n",
        ),
        ("docs/beta.md", b"first line\nHermes second citation\n"),
        ("docs/nested/gamma.txt", b"Hermes nested citation\nlast\n"),
        ("docs/nested/binary.dat", b"\xff\x00Hermes\xfe"),
    ];
    for (path, bytes) in fixtures {
        std::fs::write(root.join(path), bytes)?;
    }
    let listed = crate::file_dispatch::list(&g, ".", true)?;
    let mut listed = listed
        .iter()
        .map(|e| (e.relative_path.clone(), e.kind.clone()))
        .collect::<Vec<_>>();
    listed.sort();
    require(
        listed
            == vec![
                ("docs".into(), "directory".into()),
                ("docs/alpha.txt".into(), "file".into()),
                ("docs/beta.md".into(), "file".into()),
                ("docs/nested".into(), "directory".into()),
                ("docs/nested/binary.dat".into(), "file".into()),
                ("docs/nested/gamma.txt".into(), "file".into()),
            ],
        "recursive listing differs",
    )?;
    let analyzed = files.analyze_files(
        &g,
        &fixtures
            .iter()
            .map(|(p, _)| p.to_string())
            .collect::<Vec<_>>(),
        Some("Hermes"),
        100,
        100,
        TENANT,
        USER,
    )?;
    let mut citations = vec![];
    let mut read_files = vec![];
    for item in &analyzed {
        let bytes = fixtures
            .iter()
            .find(|(p, _)| *p == item.read.relative_path)
            .unwrap()
            .1;
        require(
            item.read.content == bytes && item.read.sha256 == sha256(bytes),
            "exact read/hash mismatch",
        )?;
        for m in &item.matches {
            require(
                m.file_sha256 == item.read.sha256,
                "citation not version-bound",
            )?;
            citations.push(json!({"relative_path":m.relative_path,"line":m.line,"column":m.column,"file_sha256":m.file_sha256}));
        }
        read_files.push(json!({"relative_path":item.read.relative_path,"bytes":item.read.size,"sha256":item.read.sha256,"encoding":item.read.encoding}));
    }
    let positions = citations
        .iter()
        .map(|c| {
            (
                c["relative_path"].as_str().unwrap(),
                c["line"].as_u64().unwrap(),
                c["column"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    require(
        positions
            == vec![
                ("docs/alpha.txt", 2, 10),
                ("docs/beta.md", 2, 1),
                ("docs/nested/gamma.txt", 1, 1),
            ],
        "search citations differ",
    )?;
    let watch_evidence = watch(&root, g.clone())?;
    let escape = files
        .analyze_files(
            &g,
            &["docs/alpha.txt".into(), "../outside.txt".into()],
            Some("Hermes"),
            100,
            100,
            TENANT,
            USER,
        )
        .is_err();
    require(escape, "path escape accepted")?;
    let mut revoked = grant(&root, "revoked-grant")?;
    let mut watcher = Watcher::default();
    require(
        watcher.scan(&revoked, TENANT, USER)?.is_empty(),
        "watch baseline failed",
    )?;
    revoked.revoke();
    let denial = [
        crate::file_dispatch::list(&revoked, ".", true).is_err(),
        revoked.read("docs/alpha.txt", 1000).is_err(),
        revoked.authorize("search", TENANT, USER).is_err(),
        files
            .analyze_files(
                &revoked,
                &["docs/alpha.txt".into()],
                Some("Hermes"),
                100,
                100,
                TENANT,
                USER,
            )
            .is_err(),
        watcher.scan(&revoked, TENANT, USER).is_err(),
    ];
    require(denial.iter().all(|b| *b), "revoked grant accepted")?;
    let file_evidence = json!({"status":"passed","listed":listed,"files":read_files,"search_query_sha256":sha256(b"Hermes"),"citations":citations,"binary_file_skipped_by_text_search":analyzed.last().unwrap().matches.is_empty(),"content_in_receipt":false,"boundary":{"path_escape_denied":escape,"list_after_revoke_denied":denial[0],"read_after_revoke_denied":denial[1],"search_after_revoke_denied":denial[2],"analysis_after_revoke_denied":denial[3],"watch_after_revoke_denied":denial[4]}});
    let before = b"original accepted bytes\n";
    let after = b"approved replacement bytes\n";
    std::fs::write(root.join("atomic.txt"), before)?;
    let before_hash = sha256(before);
    let after_hash = sha256(after);
    let write = action(
        "atomic-write",
        "file.write",
        "local_file_write",
        "file.write",
        json!({"grant_id":g.id,"relative_path":"atomic.txt","content_sha256":after_hash,"expected_hash":before_hash}),
        &before_hash,
        &g.id,
        vec![g.id.clone(), "atomic.txt".into()],
        true,
    )?;
    let receipt = files.write(
        &g,
        "atomic.txt",
        after,
        Some(&before_hash),
        &write,
        &mut ledger,
        &TestPlatform,
        &approval,
    )?;
    require(
        std::fs::read(root.join("atomic.txt"))? == after,
        "write did not reach disk",
    )?;
    let count = events(&ledger_path)?.len();
    let replay = files.write(
        &g,
        "atomic.txt",
        after,
        Some(&before_hash),
        &write,
        &mut ledger,
        &TestPlatform,
        &approval,
    )?;
    require(
        serde_json::to_value(&receipt)? == serde_json::to_value(&replay)?
            && events(&ledger_path)?.len() == count,
        "write replay changed receipt/events",
    )?;
    let rollback = action(
        "atomic-rollback",
        "file.write",
        "local_file_rollback",
        "file.rollback",
        json!({"rollback_ref":receipt.rollback_ref,"grant_id":g.id,"relative_path":"atomic.txt","expected_current_hash":after_hash}),
        &after_hash,
        &g.id,
        vec![
            receipt.rollback_ref.clone(),
            g.id.clone(),
            "atomic.txt".into(),
        ],
        true,
    )?;
    let restored = files.rollback(
        &g,
        &receipt.rollback_ref,
        &rollback,
        &mut ledger,
        &TestPlatform,
        &approval,
    )?;
    require(
        std::fs::read(root.join("atomic.txt"))? == before && restored.after_sha256 == before_hash,
        "rollback mismatch",
    )?;
    let approved = sha256(b"version shown for approval\n");
    std::fs::write(root.join("stale.txt"), b"new human edit after approval\n")?;
    let stale = action(
        "stale-write",
        "file.write",
        "local_file_write",
        "file.write",
        json!({"grant_id":g.id,"relative_path":"stale.txt","content_sha256":sha256(b"agent replacement\n"),"expected_hash":approved}),
        &approved,
        &g.id,
        vec![g.id.clone(), "stale.txt".into()],
        true,
    )?;
    require(
        matches!(
            files.write(
                &g,
                "stale.txt",
                b"agent replacement\n",
                Some(&approved),
                &stale,
                &mut ledger,
                &TestPlatform,
                &approval
            ),
            Err(crate::Error::Stale)
        ),
        "stale write accepted",
    )?;
    require(
        std::fs::read(root.join("stale.txt"))? == b"new human edit after approval\n",
        "human edit lost",
    )?;
    let write_evidence = json!({"status":"passed","before_sha256":receipt.before_sha256,"after_sha256":receipt.after_sha256,"disk_after_write_sha256":after_hash,"idempotent_replay_same_receipt":true,"idempotent_replay_added_events":false,"rollback_restored_sha256":restored.after_sha256,"rollback_matches_original":true,"stale_target_denied":true,"human_edit_preserved":true});
    let control = ProcessControl::default();
    let runner = ProcessRunner {
        policy: ProcessPolicy {
            allowed_executables: HashSet::from([
                PathBuf::from("/usr/bin/env"),
                PathBuf::from("/bin/sleep"),
            ]),
            allowed_env_names: HashSet::from(["LOCAL_NODE_ALLOWED".into()]),
            max_timeout_seconds: 10.,
            allow_inherited_network: true,
            ..Default::default()
        },
        control: control.clone(),
    };
    let env_request = ProcessRequest {
        argv: vec!["/usr/bin/env".into()],
        grant_id: g.id.clone(),
        cwd: ".".into(),
        timeout_seconds: 5.,
        env: BTreeMap::from([("LOCAL_NODE_ALLOWED".into(), "yes".into())]),
        network_policy: "inherit".into(),
    };
    let env_action = process_action("clean-environment", &env_request, &g)?;
    let chunks = Arc::new(AtomicUsize::new(0));
    let observed = chunks.clone();
    let result = runner
        .run_with_output(
            &env_request,
            &g,
            &env_action,
            &mut ledger,
            &TestPlatform,
            &approval,
            Some(Arc::new(move |_, _| {
                observed.fetch_add(1, Ordering::SeqCst);
            })),
        )
        .await?;
    let names = result
        .stdout
        .lines()
        .filter_map(|s| s.split_once('=').map(|(name, _)| name.to_owned()))
        .collect::<std::collections::BTreeSet<_>>();
    require(
        result.exit_code == Some(0)
            && names
                == ["PATH", "LANG", "LC_ALL", "LOCAL_NODE_ALLOWED"]
                    .map(str::to_owned)
                    .into_iter()
                    .collect()
            && chunks.load(Ordering::SeqCst) > 0,
        "clean environment or streaming failed",
    )?;
    let deny = ProcessRequest {
        network_policy: "deny".into(),
        env: BTreeMap::new(),
        ..env_request
    };
    let deny_action = process_action("network-deny", &deny, &g)?;
    require(
        runner
            .run(
                &deny,
                &g,
                &deny_action,
                &mut ledger,
                &TestPlatform,
                &approval,
            )
            .await
            .is_err()
            && ledger.get(&deny_action.action_id)?.is_none(),
        "network-deny dispatched",
    )?;
    let sleep = ProcessRequest {
        argv: vec!["/bin/sleep".into(), "5".into()],
        grant_id: g.id.clone(),
        cwd: ".".into(),
        timeout_seconds: 8.,
        env: BTreeMap::new(),
        network_policy: "inherit".into(),
    };
    let cancel = process_action("cancel-process", &sleep, &g)?;
    let cancel_task = async {
        wait_running(&ledger_path, &cancel.action_id).await?;
        let start = Instant::now();
        require(
            control.cancel(&cancel.action_id),
            "running child not cancellable",
        )?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(start)
    };
    let (result, started) = tokio::join!(
        runner.run(&sleep, &g, &cancel, &mut ledger, &TestPlatform, &approval),
        cancel_task
    );
    let result = result?;
    let cancel_latency = started?.elapsed().as_secs_f64() * 1000.;
    require(
        result.status == "cancelled" && cancel_latency < 2000.,
        "cancel failed two-second bound",
    )?;
    let process_evidence = json!({"status":"passed","executable":"/usr/bin/env","argv_vector":["/usr/bin/env"],"cwd_is_granted_root":true,"exit_code":0,"stream_chunk_count":chunks.load(Ordering::SeqCst),"child_environment_names":names,"provider_key_absent":!names.contains("OPENAI_API_KEY"),"network_deny_without_sandbox":"failed_closed","cancel_status":result.status,"cancel_latency_ms":cancel_latency});
    let mut runtime = LocalNodeRuntime::new(
        Ledger::open(&ledger_path)?,
        control.clone(),
        ComputerController::new(NoComputer, HashSet::new(), HashSet::new()),
        LocalServiceBinding::default(),
        Some(OutboundControlPlane {
            url: "wss://control.example.test/node".into(),
        }),
    )?;
    runtime.connect()?;
    let orphan = action(
        "disconnect-unknown",
        "test.disconnect",
        "test_disconnect",
        "test.disconnect",
        json!({"operation":"already-dispatched-side-effect"}),
        "disconnect-target",
        "disconnect-lease",
        vec!["disconnect-resource".into()],
        false,
    )?;
    ledger.begin(&orphan, &TestPlatform)?;
    ledger.transition(&orphan.action_id, "dispatched", None)?;
    ledger.transition(&orphan.action_id, "running", None)?;
    let disconnect = process_action("disconnect-process", &sleep, &g)?;
    let task = async {
        wait_running(&ledger_path, &disconnect.action_id).await?;
        let start = Instant::now();
        let ids = runtime.disconnect()?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((start, ids))
    };
    let (result, stopped) = tokio::join!(
        runner.run(
            &sleep,
            &g,
            &disconnect,
            &mut ledger,
            &TestPlatform,
            &approval
        ),
        task
    );
    let result = result?;
    let (start, ids) = stopped?;
    require(
        result.status == "cancelled"
            && ids == vec![orphan.action_id.clone()]
            && start.elapsed() < Duration::from_secs(2),
        "disconnect failed",
    )?;
    let (created, orphan_record) = ledger.begin(&orphan, &TestPlatform)?;
    require(
        !created && orphan_record.status == "unknown" && !runtime.online(),
        "unknown replay admitted",
    )?;
    let disconnect_evidence = json!({"status":"passed","runtime_offline":true,"real_child_status":result.status,"stop_latency_ms":start.elapsed().as_secs_f64()*1000.,"unresolved_terminal":orphan_record.status,"automatic_replay":false,"transport_scope":"LocalNodeRuntime disconnect entrypoint exercised in-process; no real WSS socket/reconnect was exercised"});
    ledger.verify()?;
    let entries = events(&ledger_path)?;
    let mut sequences = BTreeMap::<String, Vec<String>>::new();
    for (i, e) in entries.iter().enumerate() {
        require(e["seq"] == json!(i + 1), "ledger sequence gap")?;
        sequences
            .entry(e["action_id"].as_str().unwrap().into())
            .or_default()
            .push(e["event_type"].as_str().unwrap().into());
    }
    require(
        sequences
            .values()
            .all(|s| s.iter().filter(|e| crate::ledger::terminal(e)).count() <= 1),
        "duplicate terminal",
    )?;
    require(
        sequences[&write.action_id]
            == [
                "policy_check",
                "awaiting_approval",
                "dispatched",
                "running",
                "succeeded",
            ],
        "write order differs",
    )?;
    require(
        sequences[&cancel.action_id]
            .last()
            .is_some_and(|s| s == "cancelled"),
        "cancel terminal absent",
    )?;
    rusqlite::Connection::open(&ledger_path)?
        .execute("UPDATE events SET event_type='tampered' WHERE seq=1", [])?;
    require(
        matches!(ledger.verify(), Err(crate::Error::Integrity)),
        "ledger tampering undetected",
    )?;
    let ledger_evidence = json!({"status":"passed","sqlite_file_created":true,"event_count_before_tamper":entries.len(),"sequences_contiguous":true,"unique_terminal_per_action":true,"atomic_write_event_order":sequences[&write.action_id],"cancel_event_order":sequences[&cancel.action_id],"tamper_detected":true,"raw_process_output_stored_in_event_metadata":false});
    Ok(
        json!({"schema_version":1,"generated_at":generated_at(),"harness":"local_live_acceptance","host":{"system":std::env::consts::OS,"machine":std::env::consts::ARCH,"implementation":"Rust"},"overall_status":"passed","evidence":{"exact_file_list_read_search_hash":file_evidence,"watch":watch_evidence,"atomic_write_stale_rollback_replay":write_evidence,"structured_process":process_evidence,"runtime_disconnect":disconnect_evidence,"ledger":ledger_evidence},"evidence_tiers":{"E2_local_live":["real temporary-disk list/read/search/hash","real background watcher create/modify/rename/delete smoke","real atomic replace, stale-target preservation, rollback and replay","real allowlisted argv process, clean environment, streaming and cancellation","real disconnect-triggered child termination and SQLite ledger tamper detection"],"E1_contract_only":["platform signature uses an explicit test-only HMAC verifier","outbound control-plane URL is validated but no WSS socket/reconnect is opened"],"not_claimed":["native secure credential storage or production device identity","browser Computer Use","desktop App Computer Use","OpenAI Responses provider execution","trusted local approval UI","15-minute OS-A24 soak"]}}),
    )
}
