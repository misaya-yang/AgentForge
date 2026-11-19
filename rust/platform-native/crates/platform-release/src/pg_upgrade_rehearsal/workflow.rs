use super::*;
use copy::Cloned;
fn owned_receipt(c: &Config) -> Result<Value> {
    let path = external(Path::new(c.get("source_copy_receipt")?), &c.repo)?;
    let m = fs::symlink_metadata(&path).map_err(|_| "inspect-copy receipt required")?;
    need(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= 65536,
        "copy receipt must be bounded regular file",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        need(
            m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
            "copy receipt ownership/mode invalid",
        )?;
    }
    let v: Value = serde_json::from_slice(&fs::read(path).map_err(|_| "copy receipt unreadable")?)
        .map_err(|_| "copy receipt JSON invalid")?;
    need(
        v["schema"] == "agentforge-pg-cold-copy/v1"
            && v["source"] == c.source.to_string_lossy().as_ref()
            && v["target_id"] == c.get("target_id")?,
        "cold-copy receipt belongs to another source/scope",
    )?;
    Ok(v)
}
fn tool_versions(c: &Config, work: &Path) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for (new, tool, major) in [
        (false, "postgres", 16),
        (true, "postgres", 18),
        (true, "pg_upgrade", 18),
        (true, "initdb", 18),
    ] {
        let v = process::command(c, new, tool, &["--version".into()], work)?;
        need(
            regex::Regex::new(&format!(r"\b{major}(?:\.|\s|$)"))
                .map_err(|_| "version parser failed")?
                .is_match(&v),
            "executable major version mismatch",
        )?;
        out.insert(
            format!("{}-{tool}", if new { "new" } else { "old" }),
            json!(v.trim()),
        );
    }
    Ok(Value::Object(out))
}
fn initdb(c: &Config, dir: &Path, locale: &Value, sums: bool, work: &Path) -> Result<()> {
    let provider = text(locale, "provider")?;
    need(
        matches!(provider, "c" | "i"),
        "PG16 locale provider not supported; no silent conversion",
    )?;
    let mut args = vec![
        format!("--pgdata={}", dir.display()),
        format!("--username={}", c.get("administrator")?),
        "--auth-local=peer".into(),
        "--auth-host=reject".into(),
        format!("--encoding={}", text(locale, "encoding")?),
        format!("--lc-collate={}", text(locale, "collate")?),
        format!("--lc-ctype={}", text(locale, "ctype")?),
        format!(
            "--locale-provider={}",
            if provider == "i" { "icu" } else { "libc" }
        ),
        if sums {
            "--data-checksums".into()
        } else {
            "--no-data-checksums".into()
        },
    ];
    if provider == "i" {
        args.push(format!("--icu-locale={}", text(locale, "icu")?));
    }
    process::command(c, true, "initdb", &args, work)?;
    need(
        fs::read_to_string(dir.join("PG_VERSION"))
            .map_err(|_| "new PG_VERSION unavailable")?
            .trim()
            == "18",
        "initdb did not create PG18",
    )?;
    process::configure(c, dir)?;
    process::checksums(c, true, dir, sums, work)
}
fn upgrade_args(c: &Config, old: &Path, new: &Path, socket: &Path) -> Result<Vec<String>> {
    Ok(vec![
        format!("--old-bindir={}", c.get("old_bin")?),
        format!("--new-bindir={}", c.get("new_bin")?),
        format!("--old-datadir={}", old.display()),
        format!("--new-datadir={}", new.display()),
        format!("--old-port={}", c.port("old_port")),
        format!("--new-port={}", c.port("new_port")),
        format!("--socketdir={}", socket.display()),
        format!("--username={}", c.get("administrator")?),
        format!("--old-options={}", process::options(old, socket)),
        format!("--new-options={}", process::options(new, socket)),
        "--jobs=1".into(),
        "--link".into(),
    ])
}
fn same_filesystem(a: &Path, b: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        need(
            fs::metadata(a)
                .map_err(|_| "old copy metadata failed")?
                .dev()
                == fs::metadata(b)
                    .map_err(|_| "new copy metadata failed")?
                    .dev(),
            "--link working copies must be on same filesystem",
        )?;
    }
    Ok(())
}
async fn upgraded(
    c: &Config,
    old: &Cloned,
    new: &Path,
    socket: &Path,
    work: &Path,
    sums: bool,
    downtime: &mut Option<u128>,
) -> Result<Value> {
    process::configure(c, &old.data)?;
    process::checksums(c, false, &old.data, sums, work)?;
    let mut before = process::start(c, false, &old.data, socket, c.port("old_port"), work)?;
    let locale = process::verify(c, false, socket, c.port("old_port"), work, "source16").await?;
    before.stop()?;
    private_dir(new)?;
    initdb(c, new, &locale, sums, work)?;
    same_filesystem(&old.data, new)?;
    // Guards cover servers pg_upgrade may launch before an error/timeout.
    let mut old_guard = process::Cluster {
        c,
        new: false,
        data: old.data.clone(),
        cwd: work.into(),
        armed: true,
    };
    let mut new_guard = process::Cluster {
        c,
        new: true,
        data: new.into(),
        cwd: work.into(),
        armed: true,
    };
    let args = upgrade_args(c, &old.data, new, socket)?;
    let mut check = args.clone();
    check.push("--check".into());
    process::command(c, true, "pg_upgrade", &check, work)?;
    old_guard.stop()?;
    new_guard.stop()?;
    old_guard.armed = true;
    new_guard.armed = true;
    let stopped = Instant::now();
    let result=async {
 process::command(c,true,"pg_upgrade",&args,work)?;need(!old.data.join("postmaster.pid").exists()&&!new.join("postmaster.pid").exists(),"pg_upgrade left a running copy; refuse uncertain handoff")?;
 process::checksums(c,true,new,sums,work)?;
 let target=Cloned{data:new.into(),maps:old.maps.clone(),tree:Value::Null};copy::assert_no_source_links(c,&target)?;
 let links=copy::linked_files(old,&target)?;need(links>0,"--link did not establish any copy-internal data-file links")?;
 let mut running=process::start(c,true,new,socket,c.port("new_port"),work)?;
 process::verify(c,true,socket,c.port("new_port"),work,"upgraded18").await?;
 *downtime=Some(stopped.elapsed().as_millis());running.stop()?;new_guard.armed=false;old_guard.armed=false;
 process::checksums(c,true,new,sums,work)?;
 Ok(json!({"upgraded_major":18,"native_authority_verify":true,"checksums_enabled":sums,"copy_internal_linked_files":links,"measured_downtime_ms":downtime,"locale_preserved":locale}))
 }.await;
    if downtime.is_none() {
        *downtime = Some(stopped.elapsed().as_millis());
    }
    result
}
async fn rollback(
    c: &Config,
    work: &Path,
    socket: &Path,
    expected: &Value,
    sums: bool,
) -> Result<Value> {
    need(
        copy::inspect(c)? == *expected,
        "preserved source changed before rollback",
    )?;
    let start = Instant::now();
    let restored = copy::clone(
        c,
        &work.join("rollback16"),
        &work.join("rollback-tablespaces"),
    )?;
    need(
        restored.tree == *expected,
        "rollback copy byte checksum mismatch",
    )?;
    copy::assert_no_source_links(c, &restored)?;
    process::checksums(c, false, &restored.data, sums, work)?;
    process::configure(c, &restored.data)?;
    let mut running = process::start(
        c,
        false,
        &restored.data,
        socket,
        c.port("rollback_port"),
        work,
    )?;
    process::verify(
        c,
        false,
        socket,
        c.port("rollback_port"),
        work,
        "rollback16",
    )
    .await?;
    running.stop()?;
    process::checksums(c, false, &restored.data, sums, work)?;
    Ok(
        json!({"rollback_verified":true,"source":"independent fresh clone of preserved PG16","data":restored.data,"elapsed_ms":start.elapsed().as_millis(),"native_authority_verify":true,"checksums_enabled":sums}),
    )
}
pub(super) async fn run(c: &Config) -> Result<Value> {
    let receipt = owned_receipt(c)?;
    let initial = copy::inspect(c)?;
    need(
        receipt["tree"] == initial,
        "preserved source changed since inspect-copy receipt",
    )?;
    let evidence = external(Path::new(c.get("evidence_root")?), &c.repo)?;
    need(evidence.is_dir(), "operator must prepare evidence root")?;
    let work = c.root.join(format!("rehearsal-{}", uuid::Uuid::new_v4()));
    private_dir(&work)?;
    let final_path = evidence.join(format!("pg16-to18-{}.json", uuid::Uuid::new_v4()));
    write_new(
        &work.join("started.json"),
        &json!({"source_receipt_sha256":digest(&fs::read(c.get("source_copy_receipt")?).map_err(|_|"source receipt unavailable")?),"started_unix":now(),"source_read_only":true,"accepted":false}),
    )?;
    let attempt=async {
    let authority_source=copy::authority_binding(c)?;
    write_new(&work.join("authority-source-binding.json"),&authority_source)?;
    let versions = tool_versions(c, &work)?;
    write_new(&work.join("tools.json"), &versions)?;
    let control = process::control(c, false, &c.source, &work)?;
    need(
        control == receipt["control"] && control["state"] == "shut down",
        "source control changed or not cleanly stopped",
    )?;
    let sums = control["checksum_version"] == 1;
    let old = copy::clone(c, &work.join("old16"), &work.join("old-tablespaces"))?;
    need(
        old.tree == initial,
        "initial copy differs from preserved source",
    )?;
    let socket = work.join("socket");
    private_dir(&socket)?;
    let mut downtime = None;
    let upgrade = upgraded(
        c,
        &old,
        &work.join("new18"),
        &socket,
        &work,
        sums,
        &mut downtime,
    )
    .await;
    // Rollback is deliberately attempted even when PG18 catalog fingerprints fail.
    let mut old_cleanup=process::Cluster{c,new:false,data:old.data.clone(),cwd:work.clone(),armed:true};
    let old_stop=old_cleanup.stop();
    let mut new_cleanup=process::Cluster{c,new:true,data:work.join("new18"),cwd:work.clone(),armed:true};
    let new_stop=new_cleanup.stop();
    let rollback = rollback(c, &work, &socket, &initial, sums).await;
    let mut rollback_cleanup=process::Cluster{c,new:false,data:work.join("rollback16"),cwd:work.clone(),armed:true};
    let rollback_stop=rollback_cleanup.stop();
    let all_stopped=old_stop.is_ok()&&new_stop.is_ok()&&rollback_stop.is_ok()&&["old16","new18","rollback16"].iter().all(|name|!work.join(name).join("postmaster.pid").exists());
    let final_source = copy::inspect(c);
    let source_unchanged = final_source.as_ref().is_ok_and(|v| *v == initial);
    let authority_after=copy::authority_binding(c);
    let authority_unchanged=authority_after.as_ref().is_ok_and(|v|*v==authority_source);
    let accepted = upgrade.is_ok() && rollback.is_ok() && source_unchanged && authority_unchanged && all_stopped;
    let result = json!({"schema":"agentforge-pg-major-rehearsal-receipt/v1","accepted":accepted,"upgrade":upgrade.as_ref().ok(),"upgrade_error":upgrade.as_ref().err(),"rollback":rollback.as_ref().ok(),"rollback_error":rollback.as_ref().err(),"downtime_ms":downtime,"downtime_scope":"isolated copy: pg_upgrade link through PG18 authority verify; excludes copy/preflight","preserved_source_unchanged":source_unchanged,"source_tree_before":initial,"source_tree_after":final_source.as_ref().ok(),"source_error":final_source.as_ref().err(),"all_copy_servers_stopped":all_stopped,"shutdown_errors":[old_stop.as_ref().err(),new_stop.as_ref().err(),rollback_stop.as_ref().err()],"source_control":control,"authority_source_before":authority_source,"authority_source_after":authority_after.as_ref().ok(),"authority_source_unchanged":authority_unchanged,"tools":versions,"work":work,"completed_unix":now(),"frozen_fingerprints_modified":false,"production_upgrade_performed":false});
    write_new(&work.join("result.json"), &result)?;
    write_new(&final_path, &result)?;
    need(
        accepted,
        &format!(
            "rehearsal failed; real upgrade/rollback and catalog mismatch evidence retained at {}",
            final_path.display()
        ),
    )?;
    Ok(json!({"receipt":final_path,"work":work,"accepted":true,"production_upgrade_performed":false}))
    }.await;
    if let Err(error) = &attempt {
        if !final_path.exists() {
            let failure = json!({"schema":"agentforge-pg-major-rehearsal-receipt/v1","accepted":false,"stage":"preflight_or_copy","error":error,"work":work,"completed_unix":now(),"production_upgrade_performed":false});
            write_new(&work.join("failed.json"), &failure)?;
            write_new(&final_path, &failure)?;
        }
        return Err(format!("{error}; receipt {}", final_path.display()));
    }
    attempt
}
