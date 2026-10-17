//! Explicit local-only encrypted backup/PITR orchestration. Import has no effects.
#[path = "recovery_backup/drill.rs"]
mod drill;
#[path = "recovery_backup/objects.rs"]
mod objects;
#[cfg(test)]
#[path = "recovery_backup/tests.rs"]
mod policy_tests;
#[path = "recovery_backup/postgres.rs"]
mod postgres;
#[path = "recovery_backup/qdrant.rs"]
mod qdrant;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use std::str::FromStr;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub(super) type Result<T> = std::result::Result<T, String>;
pub(super) fn need(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
pub(super) fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing configuration field {k}"))
}
pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(super) fn hash(v: &[u8]) -> String {
    format!("{:x}", Sha256::digest(v))
}
pub(super) fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub(super) fn local_url(s: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(s).map_err(|_| "invalid endpoint")?;
    need(
        matches!(u.host_str(), Some("127.0.0.1") | Some("[::1]"))
            && matches!(u.scheme(), "http" | "https")
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && u.path() == "/",
        "endpoint must be an explicit loopback origin",
    )?;
    Ok(u)
}
pub(super) fn external(path: &Path, repo: &Path) -> Result<PathBuf> {
    need(path.is_absolute(), "external path must be absolute")?;
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or("invalid external path")?
                .to_owned(),
        );
        ancestor = ancestor.parent().ok_or("invalid external path")?;
    }
    let mut normalized = fs::canonicalize(ancestor).map_err(|_| "external parent unavailable")?;
    for part in suffix.into_iter().rev() {
        need(part != ".." && part != ".", "invalid path component")?;
        normalized.push(part);
    }
    need(
        !normalized.starts_with(repo) && normalized != Path::new("/"),
        "recovery data must be outside checkout",
    )?;
    Ok(normalized)
}
#[derive(Clone)]
pub(super) struct Config {
    pub v: Value,
    pub repo: PathBuf,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let v: Value =
            serde_json::from_slice(&fs::read(path).map_err(|_| "public configuration unreadable")?)
                .map_err(|_| "public configuration invalid JSON")?;
        let repo =
            fs::canonicalize(text(&v, "repo_root")?).map_err(|_| "repository root unavailable")?;
        let c = Self { v, repo };
        c.validate()?;
        Ok(c)
    }
    fn validate(&self) -> Result<()> {
        need(
            self.v["schema"] == "agentforge-recovery/v1",
            "unsupported recovery schema",
        )?;
        for key in [
            "target_id",
            "database",
            "role_prefix",
            "stanza",
            "baseline",
            "pg_backup_principal",
            "bucket",
            "replica_bucket",
            "drill_bucket",
        ] {
            need(identifier(text(&self.v, key)?), "invalid recovery identity")?;
        }
        need(
            text(&self.v, "role_prefix")?.ends_with('_'),
            "authority prefix invalid",
        )?;
        need(
            text(&self.v, "bucket")? != text(&self.v, "replica_bucket")?
                && text(&self.v, "drill_bucket")? != text(&self.v, "bucket")?
                && text(&self.v, "drill_bucket")? != text(&self.v, "replica_bucket")?,
            "buckets must be distinct",
        )?;
        for key in [
            "qdrant_source",
            "qdrant_drill",
            "s3_source",
            "s3_replica",
            "s3_drill",
        ] {
            local_url(text(&self.v, key)?)?;
        }
        need(
            self.v["qdrant_source"] != self.v["qdrant_drill"],
            "Qdrant drill cannot target source",
        )?;
        let source = self.v["pg_source_port"]
            .as_u64()
            .ok_or("source PG port missing")?;
        let target = self.v["pg_drill_port"]
            .as_u64()
            .ok_or("drill PG port missing")?;
        need(
            source > 1024
                && source <= 65535
                && target > 1024
                && target <= 65535
                && source != target,
            "distinct unprivileged PG ports required",
        )?;
        self.value("s3_region")?;
        if let Some(path) = self.v["s3_ca_certificate_file"].as_str() {
            external(Path::new(path), &self.repo)?;
        }
        for key in [
            "evidence_root",
            "drill_root",
            "pgbackrest_config",
            "pgbackrest_include_dir",
            "pg_source_path",
            "pg_source_dsn_file",
            "pg_drill_dsn_file",
            "s3_access_key_file",
            "s3_secret_key_file",
            "qdrant_source_key_file",
            "qdrant_drill_key_file",
        ] {
            external(Path::new(text(&self.v, key)?), &self.repo)?;
        }
        for key in ["pgbackrest_bin", "pg_ctl_bin"] {
            need(
                Path::new(text(&self.v, key)?).is_absolute(),
                "executable must be absolute",
            )?;
        }
        for key in [
            "pgbackrest_bin",
            "pgbackrest_config",
            "pgbackrest_include_dir",
            "pg_source_path",
            "drill_root",
        ] {
            need(
                !self
                    .value(key)?
                    .contains([' ', '\t', '\n', '\'', '"', '\\', ';', '$', '`']),
                "archive command path contains delimiters",
            )?;
        }
        need(
            self.value("drill_bucket")?.starts_with("af-drill-"),
            "isolated restore bucket must use af-drill- prefix",
        )?;
        let prefix = self.value("object_prefix")?;
        need(
            !prefix.is_empty()
                && prefix.ends_with('/')
                && !prefix.starts_with('/')
                && !prefix.split('/').any(|p| p == "." || p == ".."),
            "explicit safe object prefix required",
        )?;
        need(
            self.v["max_snapshot_bytes"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= 5 * 1024 * 1024 * 1024),
            "snapshot bound must be 1 byte..5 GiB",
        )?;
        need(
            self.v["timeout_seconds"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= 1800),
            "timeout must be 1..1800 seconds",
        )?;
        Ok(())
    }
    pub fn value(&self, k: &str) -> Result<&str> {
        text(&self.v, k)
    }
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.v["timeout_seconds"].as_u64().unwrap_or(1800))
    }
    pub fn secret(&self, key: &str) -> Result<String> {
        secret(Path::new(self.value(key)?), &self.repo)
    }
    pub async fn pg(&self, drill: bool) -> Result<PgConnection> {
        let raw = self.secret(if drill {
            "pg_drill_dsn_file"
        } else {
            "pg_source_dsn_file"
        })?;
        let opts = PgConnectOptions::from_str(raw.trim())
            .map_err(|_| "invalid private PG connection options")?;
        need(
            opts.get_host() == "127.0.0.1"
                && u64::from(opts.get_port())
                    == self.v[if drill {
                        "pg_drill_port"
                    } else {
                        "pg_source_port"
                    }]
                    .as_u64()
                    .unwrap_or(0)
                && opts.get_database() == Some(self.value("database")?)
                && opts.get_username() == self.value("pg_backup_principal")?,
            "PG DSN does not match explicit loopback/database allowlist",
        )?;
        let mut conn =
            tokio::time::timeout(Duration::from_secs(10), PgConnection::connect_with(&opts))
                .await
                .map_err(|_| "PG connection timeout")?
                .map_err(|_| "PG connection failed")?;
        sqlx::raw_sql("SET statement_timeout='30s'; SET lock_timeout='5s'; SET idle_in_transaction_session_timeout='1800s'").execute(&mut conn).await.map_err(|_|"PG session policy failed")?;
        let same: bool = sqlx::query_scalar("SELECT current_user=session_user")
            .fetch_one(&mut conn)
            .await
            .map_err(|_| "PG identity check failed")?;
        need(same, "PG role switching forbidden")?;
        Ok(conn)
    }
}
pub(super) fn secret(path: &Path, repo: &Path) -> Result<String> {
    let p = external(path, repo)?;
    let m = fs::symlink_metadata(path).map_err(|_| "external secret unavailable")?;
    need(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= 65536,
        "secret must be bounded regular file",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        need(
            m.mode() & 0o077 == 0 && m.uid() == unsafe { libc::geteuid() } && m.nlink() == 1,
            "secret ownership/mode invalid",
        )?;
    }
    fs::read_to_string(p)
        .map(|s| s.trim_end().into())
        .map_err(|_| "secret file unreadable".into())
}
pub(super) fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| "new private evidence/destination directory required")?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err("recovery execution requires Unix permission semantics".into());
    }
    Ok(())
}
pub(super) fn write_new(path: &Path, v: &Value) -> Result<()> {
    use std::io::Write;
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|_| "new evidence file required")?;
    f.write_all(&serde_json::to_vec_pretty(v).map_err(|_| "evidence serialization failed")?)
        .and_then(|_| f.sync_all())
        .map_err(|_| "evidence write failed".into())
}
pub(super) fn command(c: &Config, exe: &str, args: &[String], capture: bool) -> Result<Value> {
    let start = Instant::now();
    let mut cmd = Command::new(c.value(exe)?);
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // A bounded private temporary stdout file prevents pipe-buffer deadlocks.
    let output = std::env::temp_dir().join(format!("af-recovery-{}", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
        options.mode(0o600);
        cmd.process_group(0);
    }
    let file = options
        .open(&output)
        .map_err(|_| "command capture unavailable")?;
    cmd.stdout(Stdio::from(file));
    let mut child = cmd.spawn().map_err(|_| "recovery executable unavailable")?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| "command wait failed")? {
            break status;
        }
        if start.elapsed() > c.timeout() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            };
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&output);
            return Err("recovery command deadline exceeded; execution status unknown".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let bytes = if capture
        && fs::metadata(&output)
            .map_err(|_| "capture metadata failed")?
            .len()
            <= 16 * 1024 * 1024
    {
        fs::read(&output).map_err(|_| "capture read failed")?
    } else {
        Vec::new()
    };
    let _ = fs::remove_file(output);
    need(
        status.success(),
        "recovery command failed (private process output suppressed)",
    )?;
    if capture {
        serde_json::from_slice(&bytes).map_err(|_| "command JSON output invalid".into())
    } else {
        Ok(json!({"completed":true,"elapsed_ms":start.elapsed().as_millis()}))
    }
}
pub(super) fn pgbackrest(c: &Config, args: &[&str], capture: bool) -> Result<Value> {
    let mut argv = vec![
        format!("--config={}", c.value("pgbackrest_config")?),
        format!("--stanza={}", c.value("stanza")?),
        format!(
            "--config-include-path={}",
            c.value("pgbackrest_include_dir")?
        ),
        "--log-level-console=error".into(),
        "--log-level-file=off".into(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    command(c, "pgbackrest_bin", &argv, capture)
}
pub async fn run(args: Vec<String>) -> Result<Value> {
    need(
        args.len() >= 2,
        "usage: recovery-backup {plan|validate|backup|checkpoint|drill} PUBLIC.json [--execute --allow-local-target ID] [--manifest PATH]",
    )?;
    let c = Config::load(Path::new(&args[1]))?;
    let op = args[0].as_str();
    if matches!(op, "plan" | "validate") {
        need(args.len() == 2, "plan/validate accept no execution flags")?;
        return Ok(
            json!({"schema":"agentforge-recovery-plan/v1","target_id":c.value("target_id")?,"operation":"plan-only","effects":false,"mechanisms":["pgBackRest AES-256 base backup and archive-push WAL","PG publication locks + named restore point + Qdrant snapshots","S3 versioning + replica verification","isolated restore + authority verify + measured RPO/RTO"],"rpo_seconds":300,"rto_seconds":1800}),
        );
    }
    need(
        matches!(op, "backup" | "checkpoint" | "drill"),
        "unknown recovery operation",
    )?;
    need(
        args.get(2).map(String::as_str) == Some("--execute")
            && args.get(3).map(String::as_str) == Some("--allow-local-target")
            && args.get(4).map(String::as_str) == Some(c.value("target_id")?),
        "explicit execution and exact local target ID required",
    )?;
    let root = external(Path::new(c.value("evidence_root")?), &c.repo)?;
    need(
        root.is_dir(),
        "operator must prepare external evidence root",
    )?;
    let evidence = root.join(format!("{}-{}", op, uuid::Uuid::new_v4()));
    private_dir(&evidence)?;
    let start = now();
    write_new(
        &evidence.join("started.json"),
        &json!({"operation":op,"started_unix":start,"target_id":c.value("target_id")?,"accepted":false}),
    )?;
    let task = async {
        if op != "drill" {
            need(args.len() == 5, "unexpected backup arguments")?;
            postgres::backup(&c, &evidence, op == "backup").await
        } else {
            need(
                args.len() == 7 && args[5] == "--manifest",
                "drill requires --manifest PATH",
            )?;
            drill::restore(&c, &evidence, Path::new(&args[6])).await
        }
    };
    let result = tokio::time::timeout(c.timeout(), task)
        .await
        .unwrap_or_else(|_| {
            Err("recovery workflow deadline exceeded; partial isolated data retained".into())
        });
    match result {
        Ok(v) => {
            write_new(&evidence.join("completed.json"), &v)?;
            Ok(json!({"evidence":evidence,"result":v}))
        }
        Err(error) => {
            write_new(
                &evidence.join("failed.json"),
                &json!({"operation":op,"failed_unix":now(),"accepted":false,"reason":error}),
            )?;
            Err(error)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_cannot_escape_local_allowlist() {
        for u in [
            "http://localhost:6333",
            "http://example.org",
            "http://127.0.0.1.evil",
            "http://user:pass@127.0.0.1",
            "http://127.0.0.1/?token=x",
        ] {
            assert!(local_url(u).is_err());
        }
        assert!(local_url("http://127.0.0.1:16333").is_ok());
    }
    #[test]
    fn identifiers_reject_options_and_traversal() {
        for s in ["../main", "--reset", "a/b", "x;rm", "", "a\nb"] {
            assert!(!identifier(s));
        }
        assert!(identifier("drill_2026"));
    }
}
