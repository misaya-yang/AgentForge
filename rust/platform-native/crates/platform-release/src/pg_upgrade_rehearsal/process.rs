use super::*;
use sqlx::{ConnectOptions, Connection, postgres::PgConnectOptions};
use std::{
    io::Write,
    process::{Command, Stdio},
};
pub(super) fn command(
    c: &Config,
    new: bool,
    name: &str,
    args: &[String],
    cwd: &Path,
) -> Result<String> {
    let log = cwd.join(format!("{}-{}.log", name, uuid::Uuid::new_v4()));
    let mut o = fs::OpenOptions::new();
    o.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let file = o
        .open(&log)
        .map_err(|_| "private command log unavailable")?;
    let mut cmd = Command::new(c.bin(new, name)?);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            file.try_clone().map_err(|_| "log handle failed")?,
        ))
        .stderr(Stdio::from(file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| format!("{name} executable unavailable"))?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| "native command wait failed")? {
            break status;
        }
        if start.elapsed() > c.timeout()
            || fs::metadata(&log).map_err(|_| "log metadata failed")?.len() > 16 * 1024 * 1024
        {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            };
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{name} deadline/output limit reached; owned cluster guards will stop copies"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    need(
        status.success(),
        &format!("{name} failed; inspect private command log"),
    )?;
    fs::read_to_string(&log).map_err(|_| "command log unreadable".into())
}
pub(super) fn parse_control(raw: &str) -> Result<Value> {
    let field = |key: &str| {
        raw.lines()
            .find_map(|s| s.strip_prefix(key))
            .map(str::trim)
            .ok_or_else(|| format!("control field missing: {key}"))
    };
    let state = field("Database cluster state:")?;
    let sum = field("Data page checksum version:")?
        .parse::<u32>()
        .map_err(|_| "checksum control invalid")?;
    need(sum <= 1, "unknown checksum version")?;
    let system = field("Database system identifier:")?;
    need(
        !system.is_empty() && system.bytes().all(|b| b.is_ascii_digit()),
        "system identifier invalid",
    )?;
    Ok(json!({"state":state,"checksum_version":sum,"system_identifier":system}))
}
pub(super) fn control(c: &Config, new: bool, data: &Path, cwd: &Path) -> Result<Value> {
    parse_control(&command(
        c,
        new,
        "pg_controldata",
        &[data.to_string_lossy().into()],
        cwd,
    )?)
}
fn owner() -> Result<String> {
    #[cfg(unix)]
    {
        unsafe {
            let p = libc::getpwuid(libc::geteuid());
            need(!p.is_null(), "OS owner unavailable")?;
            let s = std::ffi::CStr::from_ptr((*p).pw_name)
                .to_str()
                .map_err(|_| "OS owner invalid")?
                .to_string();
            need(id(&s), "OS owner unsupported by isolated peer map")?;
            return Ok(s);
        }
    }
    #[cfg(not(unix))]
    Err("Unix peer auth required".into())
}
pub(super) fn options(data: &Path, socket: &Path) -> String {
    format!(
        "-c config_file={}/postgresql.conf -c data_directory={} -c hba_file={}/rehearsal_hba.conf -c ident_file={}/rehearsal_ident.conf -c listen_addresses='' -c unix_socket_directories={} -c shared_preload_libraries='' -c max_logical_replication_workers=0 -c archive_mode=off -c archive_command='' -c archive_cleanup_command='' -c recovery_end_command='' -c primary_conninfo='' -c primary_slot_name='' -c restore_command='' -c ssl=off -c external_pid_file='' -c logging_collector=off",
        data.display(),
        data.display(),
        data.display(),
        data.display(),
        socket.display()
    )
}
pub(super) fn configure(c: &Config, data: &Path) -> Result<()> {
    let admin = c.get("administrator")?;
    for (name, body) in [
        (
            "rehearsal_hba.conf",
            format!(
                "local all {admin} peer map=rehearsal\nlocal all all reject\nhost all all 0.0.0.0/0 reject\nhost all all ::0/0 reject\n"
            ),
        ),
        (
            "rehearsal_ident.conf",
            format!("rehearsal {} {admin}\n", owner()?),
        ),
    ] {
        let p = data.join(name);
        let mut o = fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o
            .open(p)
            .map_err(|_| "new isolated peer-auth configuration required")?;
        f.write_all(body.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|_| "isolated authentication config write failed")?;
    }
    Ok(())
}
pub(super) struct Cluster<'a> {
    pub c: &'a Config,
    pub new: bool,
    pub data: PathBuf,
    pub cwd: PathBuf,
    pub armed: bool,
}
impl Cluster<'_> {
    pub fn stop(&mut self) -> Result<()> {
        if self.data.join("postmaster.pid").exists() {
            command(
                self.c,
                self.new,
                "pg_ctl",
                &[
                    "-D".into(),
                    self.data.to_string_lossy().into(),
                    "-w".into(),
                    "-t".into(),
                    "60".into(),
                    "-m".into(),
                    "fast".into(),
                    "stop".into(),
                ],
                &self.cwd,
            )?;
        }
        self.armed = false;
        Ok(())
    }
}
impl Drop for Cluster<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.stop();
        }
    }
}
pub(super) fn start<'a>(
    c: &'a Config,
    new: bool,
    data: &Path,
    socket: &Path,
    port: u16,
    cwd: &Path,
) -> Result<Cluster<'a>> {
    let cluster = Cluster {
        c,
        new,
        data: data.into(),
        cwd: cwd.into(),
        armed: true,
    };
    command(
        c,
        new,
        "pg_ctl",
        &[
            "-D".into(),
            data.to_string_lossy().into(),
            "-w".into(),
            "-t".into(),
            "60".into(),
            "-l".into(),
            cwd.join(format!("postgres-{}.log", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .into(),
            "-o".into(),
            format!("{} -p {port}", options(data, socket)),
            "start".into(),
        ],
        cwd,
    )?;
    Ok(cluster)
}
pub(super) fn checksums(
    c: &Config,
    new: bool,
    data: &Path,
    enabled: bool,
    cwd: &Path,
) -> Result<()> {
    let v = control(c, new, data, cwd)?;
    cold_checksum(&v, enabled)?;
    if enabled {
        command(
            c,
            new,
            "pg_checksums",
            &["--check".into(), format!("--pgdata={}", data.display())],
            cwd,
        )?;
    }
    Ok(())
}
async fn verify_inner(
    c: &Config,
    new: bool,
    socket: &Path,
    port: u16,
    evidence: &Path,
    phase: &str,
) -> Result<Value> {
    use ai_platform_migration_authority::{
        commands,
        fingerprint::compute_fingerprints,
        runner::{AuthorityPaths, MigrationAuthority, set_role},
    };
    let options = PgConnectOptions::new()
        .host(socket.to_str().ok_or("socket path invalid")?)
        .port(port)
        .username(c.get("administrator")?)
        .database(c.get("database")?);
    let mut conn = tokio::time::timeout(Duration::from_secs(10), options.connect())
        .await
        .map_err(|_| "copy connection timeout")?
        .map_err(|_| "copy peer-auth connection failed")?;
    sqlx::raw_sql("SET statement_timeout='60s'; SET lock_timeout='5s'")
        .execute(&mut conn)
        .await
        .map_err(|_| "copy SQL deadlines failed")?;
    let identity: (bool, i32) = sqlx::query_as(
        "SELECT current_user=session_user, current_setting('server_version_num')::int/10000",
    )
    .fetch_one(&mut conn)
    .await
    .map_err(|_| "copy identity check failed")?;
    need(
        identity.0 && identity.1 == if new { 18 } else { 16 },
        "copy server identity/major mismatch",
    )?;
    let locale: Value = if !new {
        sqlx::query_scalar("SELECT jsonb_build_object('provider',datlocprovider,'collate',datcollate,'ctype',datctype,'icu',daticulocale,'encoding',pg_encoding_to_char(encoding)) FROM pg_database WHERE datname='template1'").fetch_one(&mut conn).await.map_err(|_|"source locale inventory failed")?
    } else {
        Value::Null
    };
    conn.close()
        .await
        .map_err(|_| "copy connection close failed")?;
    let paths = AuthorityPaths {
        database_dir: c.repo.join("database"),
    };
    let authority = MigrationAuthority::new(
        options.to_url_lossy().to_string(),
        paths,
        c.get("role_prefix")?.into(),
    )
    .map_err(|_| "authority config rejected")?;
    let verified = tokio::time::timeout(
        c.timeout(),
        commands::verify(&authority, c.get("baseline")?),
    )
    .await
    .map_err(|_| "native authority timeout".to_string())
    .and_then(|r| r.map_err(|e| e.to_string()));
    let actual = async {
        let (baseline, _) = commands::load_baseline(&authority.paths, c.get("baseline")?)
            .map_err(|e| e.to_string())?;
        let mut conn = authority.connect(true).await.map_err(|e| e.to_string())?;
        let applied = authority
            .applied_changes(&mut conn, c.get("baseline")?)
            .await
            .map_err(|e| e.to_string())?;
        let expected = commands::expected_epoch_fingerprints(&authority.paths, &baseline, &applied)
            .map_err(|e| e.to_string())?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|_| "fingerprint transaction failed")?;
        set_role(&mut tx, &format!("{}owner", c.get("role_prefix")?))
            .await
            .map_err(|e| e.to_string())?;
        let values = compute_fingerprints(&mut tx, c.get("role_prefix")?, &baseline.reference_data)
            .await
            .map_err(|e| e.to_string())?;
        tx.rollback()
            .await
            .map_err(|_| "fingerprint rollback failed")?;
        Ok::<_, String>(
            json!({"actual":values,"frozen_expected":expected,"matches":values==expected}),
        )
    }
    .await;
    let receipt = json!({"phase":phase,"native_authority_verify":verified.is_ok(),"authority_error":verified.as_ref().err(),"fingerprints":actual.as_ref().ok(),"fingerprint_error":actual.as_ref().err(),"server_major":if new{18}else{16},"verified_unix":now(),"source_fingerprints_modified":false});
    write_new(&evidence.join(format!("authority-{phase}.json")), &receipt)?;
    verified?;
    let actual = actual?;
    need(
        actual["matches"] == true,
        "actual fingerprints differ from immutable authority",
    )?;
    Ok(locale)
}
pub(super) async fn verify(
    c: &Config,
    new: bool,
    socket: &Path,
    port: u16,
    evidence: &Path,
    phase: &str,
) -> Result<Value> {
    tokio::time::timeout(
        c.timeout(),
        verify_inner(c, new, socket, port, evidence, phase),
    )
    .await
    .map_err(|_| "copy verification deadline exceeded".to_string())?
}

fn cold_checksum(v: &Value, enabled: bool) -> Result<()> {
    need(
        v["state"] == "shut down"
            && v["checksum_version"] == if enabled { json!(1) } else { json!(0) },
        "checksum setting/state changed across copy/upgrade",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_checksum_and_dirty_control_fail() {
        let raw = "Database cluster state: shut down\nData page checksum version: 1\nDatabase system identifier: 12345\n";
        assert_eq!(parse_control(raw).unwrap()["checksum_version"], 1);
        assert!(parse_control(&raw.replace("version: 1", "version: 9")).is_err());
        assert!(parse_control("unrecognized control output").is_err());
        let clean = parse_control(raw).unwrap();
        cold_checksum(&clean, true).unwrap();
        assert!(cold_checksum(&clean, false).is_err());
        let dirty = parse_control(&raw.replace("shut down", "in production")).unwrap();
        assert!(cold_checksum(&dirty, true).is_err());
    }
}
