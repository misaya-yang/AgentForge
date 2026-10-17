//! PG16→18 cold-copy rehearsal. Source data is only read, never started or linked.
#[path = "pg_upgrade_rehearsal/copy.rs"]
mod copy;
#[path = "pg_upgrade_rehearsal/process.rs"]
mod process;
#[path = "pg_upgrade_rehearsal/workflow.rs"]
mod workflow;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
fn need(ok: bool, msg: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(msg.into()) }
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing {k}"))
}
fn option_path(v: &str) -> bool {
    v.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
}
fn id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.as_bytes()[0].is_ascii_alphabetic()
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn external(path: &Path, repo: &Path) -> Result<PathBuf> {
    need(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "absolute path without parent traversal required",
    )?;
    let mut p = path;
    let mut suffix = vec![];
    while !p.exists() {
        suffix.push(p.file_name().ok_or("invalid path")?.to_owned());
        p = p.parent().ok_or("invalid ancestor")?;
    }
    let mut p = fs::canonicalize(p).map_err(|_| "path ancestor unavailable")?;
    for s in suffix.into_iter().rev() {
        p.push(s);
    }
    need(
        !p.starts_with(repo) && p != Path::new("/"),
        "copies/evidence must be outside repository",
    )?;
    Ok(p)
}
struct Config {
    v: Value,
    repo: PathBuf,
    root: PathBuf,
    source: PathBuf,
}
impl Config {
    fn load(path: &Path) -> Result<Self> {
        let v: Value =
            serde_json::from_slice(&fs::read(path).map_err(|_| "public config unavailable")?)
                .map_err(|_| "invalid public JSON")?;
        let repo =
            fs::canonicalize(text(&v, "repo_root")?).map_err(|_| "repository unavailable")?;
        let root = external(Path::new(text(&v, "allowed_copy_root")?), &repo)?;
        let source = external(Path::new(text(&v, "preserved_source")?), &repo)?;
        let c = Self {
            v,
            repo,
            root,
            source,
        };
        c.validate()?;
        Ok(c)
    }
    fn get(&self, k: &str) -> Result<&str> {
        text(&self.v, k)
    }
    fn validate(&self) -> Result<()> {
        need(
            self.v["schema"] == "agentforge-pg-major-rehearsal/v1",
            "unsupported config",
        )?;
        for k in [
            "target_id",
            "database",
            "administrator",
            "role_prefix",
            "baseline",
        ] {
            let s = self.get(k)?;
            need(
                !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid PostgreSQL identity",
            )?;
        }
        need(
            id(self.get("target_id")?) && self.get("role_prefix")?.ends_with('_'),
            "invalid copy/prefix identity",
        )?;
        need(
            self.source.starts_with(&self.root) && self.source != self.root,
            "preserved source must be an explicit child of allowed copy root",
        )?;
        for k in ["source_copy_receipt", "evidence_root"] {
            let p = external(Path::new(self.get(k)?), &self.repo)?;
            need(
                !p.starts_with(&self.source),
                "evidence cannot be written inside preserved source",
            )?;
        }
        for k in ["old_bin", "new_bin"] {
            let p = Path::new(self.get(k)?);
            need(
                p.is_absolute() && option_path(&p.to_string_lossy()),
                "absolute tool directory without option delimiters required",
            )?;
        }
        need(
            option_path(&self.root.to_string_lossy()),
            "copy root cannot contain PostgreSQL option delimiters",
        )?;
        let mut ports = std::collections::BTreeSet::new();
        for k in ["old_port", "new_port", "rollback_port"] {
            let n = self.v[k].as_u64().ok_or("port missing")?;
            need(
                n > 1024 && n <= 65535 && n != 5432 && ports.insert(n),
                "three distinct isolated non-default ports required",
            )?;
        }
        need(
            self.v["timeout_seconds"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= 7200),
            "bounded timeout 1..7200 required",
        )?;
        let maps = self.v["tablespaces"]
            .as_object()
            .ok_or("explicit tablespaces map required")?;
        for (oid, path) in maps {
            need(
                !oid.is_empty() && oid.bytes().all(|b| b.is_ascii_digit()),
                "tablespace OID invalid",
            )?;
            let p = external(
                Path::new(path.as_str().ok_or("tablespace path invalid")?),
                &self.repo,
            )?;
            need(
                p.starts_with(&self.root) && !p.starts_with(&self.source),
                "tablespace must be a separately preserved copy under allowed root",
            )?;
        }
        Ok(())
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.v["timeout_seconds"].as_u64().unwrap_or(3600))
    }
    fn bin(&self, new: bool, name: &str) -> Result<PathBuf> {
        Ok(Path::new(self.get(if new { "new_bin" } else { "old_bin" })?).join(name))
    }
    fn port(&self, key: &str) -> u16 {
        self.v[key].as_u64().unwrap_or(0) as u16
    }
}
fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| "new private directory required")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("Unix copy/peer-auth semantics required".into())
    }
}
fn write_new(path: &Path, v: &Value) -> Result<()> {
    use std::io::Write;
    let mut o = fs::OpenOptions::new();
    o.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|_| "new evidence file required")?;
    f.write_all(&serde_json::to_vec_pretty(v).map_err(|_| "JSON encoding failed")?)
        .and_then(|_| f.sync_all())
        .map_err(|_| "receipt persistence failed".into())
}
pub async fn run(args: Vec<String>) -> Result<Value> {
    need(
        args.len() >= 2,
        "usage: pg-major-rehearsal {plan|inspect-copy|run} PUBLIC.json [--execute --allow-copy ID]",
    )?;
    let c = Config::load(Path::new(&args[1]))?;
    if args[0] == "plan" {
        need(args.len() == 2, "plan does not execute commands")?;
        return Ok(
            json!({"schema":"agentforge-pg-major-plan/v1","effects":false,"from":16,"to":18,"target_id":c.get("target_id")?,"source_never_started":true,"checksum_policy":"preserve source setting, never silently enable/disable","link_scope":"new isolated working copies only","rollback":"independent fresh clone of preserved source, actual PG16 authority verify"}),
        );
    }
    need(
        matches!(args[0].as_str(), "inspect-copy" | "run")
            && args.len() == 5
            && args[2] == "--execute"
            && args[3] == "--allow-copy"
            && args[4] == c.get("target_id")?,
        "execution requires exact --execute --allow-copy ID",
    )?;
    #[cfg(unix)]
    need(
        unsafe { libc::geteuid() } != 0,
        "run as the non-root PostgreSQL copy owner",
    )?;
    if args[0] == "inspect-copy" {
        let source = copy::inspect(&c)?;
        let control = process::control(&c, false, &c.source, &c.root)?;
        need(
            control["state"] == "shut down",
            "preserved PG16 copy must be cleanly stopped",
        )?;
        let receipt = json!({"schema":"agentforge-pg-cold-copy/v1","source":c.source,"target_id":c.get("target_id")?,"tree":source,"control":control,"observed_unix":now()});
        let p = external(Path::new(c.get("source_copy_receipt")?), &c.repo)?;
        write_new(&p, &receipt)?;
        return Ok(json!({"copy_receipt":p,"source_read_only":true}));
    }
    workflow::run(&c).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copy_ids_cannot_be_options() {
        assert!(id("copy_16"));
        for s in ["--link", "../main", "x;y", "x y", ""] {
            assert!(!id(s));
        }
    }
    #[test]
    fn parent_and_repository_destinations_are_refused() {
        let repo =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")).unwrap();
        assert!(external(&repo.join("data"), &repo).is_err());
        assert!(external(Path::new("/tmp/a/../main"), &repo).is_err());
    }
    fn fixture() -> Config {
        let repo =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")).unwrap();
        let v = serde_json::from_slice(
            &fs::read(repo.join("deploy/backup/pg-upgrade/rehearsal.example.json")).unwrap(),
        )
        .unwrap();
        Config {
            v,
            repo,
            root: "/tmp/af-pg-copy-tests".into(),
            source: "/tmp/af-pg-copy-tests/preserved16".into(),
        }
    }
    #[test]
    fn foreign_source_and_shared_ports_are_refused() {
        let mut c = fixture();
        c.validate().unwrap();
        c.source = "/tmp/production".into();
        assert!(c.validate().is_err());
        let mut c = fixture();
        c.v["new_port"] = c.v["old_port"].clone();
        assert!(c.validate().is_err());
    }
    #[test]
    fn shell_expansion_in_copy_or_tool_paths_is_refused() {
        let mut c = fixture();
        c.root = "/tmp/x$(id)".into();
        c.source = c.root.join("preserved16");
        assert!(c.validate().is_err());
        let mut c = fixture();
        c.v["old_bin"] = json!("/tmp/bin;id");
        assert!(c.validate().is_err());
    }
}
