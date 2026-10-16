//! Shared Git-common-dir singleton locks. Unknown cleanup retains the fence.
use crate::*;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    process::{Child, Command},
    sync::atomic::{AtomicI32, Ordering},
    time::{Duration, Instant},
};
const RESOURCES: [&str; 2] = ["integration-runtime", "rust-build"];
static SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn signal_handler(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::SeqCst);
}
fn sensitive(name: &str) -> bool {
    let expanded = regex::Regex::new(r"([a-z0-9])([A-Z])")
        .unwrap()
        .replace_all(name, "${1}_${2}");
    let normalized = regex::Regex::new(r"[^A-Za-z0-9]+")
        .unwrap()
        .replace_all(&expanded, "_")
        .to_uppercase();
    [
        "KEY",
        "KEYS",
        "TOKEN",
        "TOKENS",
        "SECRET",
        "SECRETS",
        "PASSWORD",
        "PASSWORDS",
        "DSN",
        "CREDENTIAL",
        "CREDENTIALS",
    ]
    .iter()
    .any(|s| normalized.trim_matches('_').ends_with(s) || normalized.split('_').any(|p| p == *s))
}
pub(crate) fn redact_text(s: &str) -> String {
    let urls = regex::Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://)([^/@\s]+)@")
        .unwrap()
        .replace_all(s, "${1}<redacted>@");
    let assignment = regex::Regex::new(r"(\b[A-Za-z_][A-Za-z0-9_]*)=([^\s,;]+)")
        .unwrap()
        .replace_all(&urls, |m: &regex::Captures| {
            if sensitive(&m[1]) {
                format!("{}=<redacted>", &m[1])
            } else {
                m[0].into()
            }
        })
        .into_owned();
    regex::Regex::new(r"(--[A-Za-z0-9_-]+)(=|\s+)([^\s,;]+)")
        .unwrap()
        .replace_all(&assignment, |m: &regex::Captures| {
            if sensitive(m[1].trim_start_matches('-')) {
                format!("{}{}<redacted>", &m[1], &m[2])
            } else {
                m[0].into()
            }
        })
        .into_owned()
}
fn redact(command: &[String]) -> Vec<String> {
    let mut next = false;
    command
        .iter()
        .map(|a| {
            if next {
                next = false;
                return "<redacted>".into();
            }
            if a.starts_with('-') {
                let (flag, value) = a
                    .split_once('=')
                    .map(|(a, b)| (a, Some(b)))
                    .unwrap_or((a, None));
                if sensitive(flag.trim_start_matches('-')) {
                    if value.is_some() {
                        return format!("{flag}=<redacted>");
                    }
                    next = true;
                    return flag.into();
                }
            }
            if !a.starts_with(['-']) && !a.starts_with("http://") && !a.starts_with("https://") {
                if let Some((name, _)) = a.split_once('=') {
                    if regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$")
                        .unwrap()
                        .is_match(name)
                        && sensitive(name)
                    {
                        return format!("{name}=<redacted>");
                    }
                }
            }
            redact_text(a)
        })
        .collect()
}
fn sanitize(v: &Value, key: Option<&str>, depth: usize) -> Result<Value> {
    require(depth <= 8, "runtime identity nesting exceeds bound")?;
    if key.is_some_and(sensitive) {
        return Ok(json!("<redacted>"));
    }
    Ok(match v {
        Value::Object(map) => {
            require(map.len() <= 256, "runtime identity object too large")?;
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                require(
                    !k.is_empty() && k.chars().count() <= 128 && !k.chars().any(|c| c < ' '),
                    "invalid runtime identity key",
                )?;
                out.insert(k.clone(), sanitize(v, Some(k), depth + 1)?);
            }
            Value::Object(out)
        }
        Value::Array(a) => {
            require(a.len() <= 256, "runtime identity array too large")?;
            Value::Array(
                a.iter()
                    .map(|v| sanitize(v, None, depth + 1))
                    .collect::<Result<_>>()?,
            )
        }
        Value::String(s) => {
            require(
                s.chars().count() <= 4096 && !s.contains('\0'),
                "invalid runtime identity string",
            )?;
            json!(redact_text(s))
        }
        _ => v.clone(),
    })
}
fn host() -> Result<String> {
    let mut buffer = [0u8; 256];
    require(
        unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0,
        "hostname unavailable",
    )?;
    let n = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Ok(std::str::from_utf8(&buffer[..n])?.into())
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
fn pid_dead(pid: i32) -> bool {
    (unsafe { libc::kill(pid, 0) }) < 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
fn identity(root: &Path) -> Result<(PathBuf, PathBuf, String)> {
    let git = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()?;
        require(out.status.success(), "Git lock identity unavailable")?;
        Ok(String::from_utf8(out.stdout)?.trim().into())
    };
    let tree = PathBuf::from(git(&["rev-parse", "--show-toplevel"])?).canonicalize()?;
    let common = resolve_path(
        tree.join(git(&["rev-parse", "--git-common-dir"])?)
            .to_str()
            .ok_or("invalid Git path")?,
    )?;
    Ok((tree, common, git(&["rev-parse", "HEAD"])?))
}
fn load(path: &Path) -> Result<Value> {
    require(
        path.is_file() && !path.is_symlink(),
        "owner receipt must be real file",
    )?;
    let owner = object(path)?;
    for key in [
        "schema_version",
        "owner_token",
        "resource",
        "hostname",
        "worktree_path",
        "git_head",
        "start_time",
        "heartbeat_time",
        "expected_end_condition",
    ] {
        require(owner[key].is_string(), "invalid owner string field")?;
    }
    require(
        owner["schema_version"] == "ai-gateway/resource-lock/v1"
            && RESOURCES.iter().any(|r| owner["resource"] == *r),
        "invalid owner schema/resource",
    )?;
    require(
        owner["pid"]
            .as_i64()
            .is_some_and(|n| n > 0 && n <= i32::MAX as i64)
            && owner["timeout_seconds"].as_u64().is_some_and(|n| n > 0)
            && owner["owner_token"].as_str().is_some_and(|s| !s.is_empty()),
        "invalid owner bounds",
    )?;
    require(
        owner["owner_token"].as_str().is_some_and(|s| {
            s.len() <= 128 && !s.contains(['/', '\\', '\0']) && !s.chars().any(|c| c < ' ')
        }),
        "unsafe owner token",
    )?;
    let command = owner["command"]
        .as_array()
        .ok_or("owner command missing")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid command".into())
        })
        .collect::<Result<Vec<_>>>()?;
    require(
        json!(redact(&command)) == owner["command"]
            && redact_text(owner["expected_end_condition"].as_str().unwrap())
                == owner["expected_end_condition"],
        "owner contains unredacted sensitive data",
    )?;
    if let Some(v) = owner.get("runtime_identity").filter(|v| !v.is_null()) {
        require(
            v.is_object() && sanitize(v, None, 0)? == *v,
            "invalid/unsanitized runtime identity",
        )?;
    }
    for key in ["start_time", "heartbeat_time"] {
        chrono::DateTime::parse_from_rfc3339(owner[key].as_str().unwrap())?;
    }
    Ok(owner)
}
fn atomic(path: &Path, owner: &Value) -> Result<()> {
    let token = owner["owner_token"].as_str().ok_or("owner token missing")?;
    require(
        token.len() <= 128 && !token.contains(['/', '\\', '\0']),
        "unsafe owner token",
    )?;
    let temp = path.join(format!(".owner-{token}-{}.tmp", std::process::id()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        f.write_all(format!("{}\n", serde_json::to_string_pretty(owner)?).as_bytes())?;
        f.sync_all()?;
        fs::rename(&temp, path.join("owner.json"))?;
        fs::File::open(path)?.sync_all()?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn allowed(path: &Path, token: &str) -> Result<bool> {
    if path.is_symlink() || !path.is_dir() {
        return Ok(false);
    }
    let regex = regex::Regex::new(&format!(r"^\.owner-{}-[0-9]+\.tmp$", regex::escape(token)))?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.path().is_symlink() || !entry.file_type()?.is_file() {
            return Ok(false);
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != "owner.json" && !regex.is_match(&name) {
            return Ok(false);
        }
    }
    Ok(true)
}
fn remove(path: &Path, token: &str) -> Result<()> {
    require(allowed(path, token)?, "lock contains ambiguous files")?;
    if let Ok(owner) = load(&path.join("owner.json")) {
        require(
            owner["owner_token"] == token,
            "owner changed before cleanup",
        )?;
    }
    for entry in fs::read_dir(path)? {
        fs::remove_file(entry?.path())?;
    }
    fs::remove_dir(path)?;
    Ok(())
}
struct Lock {
    root: PathBuf,
    mutex: PathBuf,
    resource: PathBuf,
    owner: Value,
}
impl Lock {
    fn owned(&self) -> Result<()> {
        for directory in [&self.mutex, &self.resource] {
            let owner = load(&directory.join("owner.json"))?;
            require(
                owner["owner_token"] == self.owner["owner_token"]
                    && owner["resource"] == self.owner["resource"],
                "lock ownership changed while held",
            )?;
        }
        Ok(())
    }
    fn reap(&self) -> Result<()> {
        require(
            self.mutex.is_dir() && !self.mutex.is_symlink(),
            "existing mutex ambiguous",
        )?;
        let old = load(&self.mutex.join("owner.json"))?;
        require(
            old["hostname"] == host()? && pid_dead(old["pid"].as_i64().unwrap() as i32),
            "resource busy: owner live, ambiguous or cross-host",
        )?;
        let heartbeat =
            chrono::DateTime::parse_from_rfc3339(old["heartbeat_time"].as_str().unwrap())?;
        require(
            chrono::Utc::now()
                .signed_duration_since(heartbeat)
                .num_milliseconds()
                > old["timeout_seconds"]
                    .as_i64()
                    .ok_or("owner timeout out of range")?
                    .saturating_mul(1000),
            "dead owner heartbeat timeout not elapsed",
        )?;
        let token = old["owner_token"].as_str().unwrap();
        let resource = old["resource"].as_str().unwrap();
        for other in RESOURCES {
            require(
                other == resource || !self.root.join(other).exists(),
                "stale lock has ambiguous opposite receipt",
            )?;
        }
        let directory = self.root.join(resource);
        if directory.exists() {
            if directory.join("owner.json").exists() {
                let receipt = load(&directory.join("owner.json"))?;
                require(
                    ["owner_token", "resource", "hostname", "pid"]
                        .iter()
                        .all(|k| receipt[k] == old[k])
                        && allowed(&directory, token)?,
                    "stale resource receipt mismatch",
                )?;
            } else {
                require(
                    fs::read_dir(&directory)?.next().is_none(),
                    "stale resource directory ambiguous",
                )?;
            }
        }
        let quarantine = self.root.join(format!(".stale-{token}"));
        require(!quarantine.exists(), "stale quarantine exists")?;
        fs::rename(&self.mutex, &quarantine)?;
        let result = (|| -> Result<()> {
            if directory.exists() {
                remove(&directory, token)?;
            }
            remove(&quarantine, token)
        })();
        if result.is_err() && quarantine.exists() && !self.mutex.exists() {
            let _ = fs::rename(quarantine, &self.mutex);
        }
        result
    }
    fn acquire(&self) -> Result<()> {
        if !self.root.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.root)?;
        }
        require(
            self.root.is_dir() && !self.root.is_symlink(),
            "lock root ambiguous",
        )?;
        for _ in 0..3 {
            match fs::DirBuilder::new().mode(0o700).create(&self.mutex) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.reap()?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let result = (|| -> Result<()> {
                atomic(&self.mutex, &self.owner)?;
                require(
                    RESOURCES.iter().all(|r| !self.root.join(r).exists()),
                    "orphan resource receipt prevents acquisition",
                )?;
                fs::DirBuilder::new().mode(0o700).create(&self.resource)?;
                atomic(&self.resource, &self.owner)
            })();
            if let Err(e) = result {
                if self.resource.exists() && fs::read_dir(&self.resource)?.next().is_none() {
                    let _ = fs::remove_dir(&self.resource);
                }
                let _ = remove(&self.mutex, self.owner["owner_token"].as_str().unwrap());
                return Err(e);
            }
            return Ok(());
        }
        Err("lock acquisition failed to converge".into())
    }
    fn heartbeat(&mut self) -> Result<()> {
        self.owned()?;
        self.owner["heartbeat_time"] = json!(now());
        atomic(&self.resource, &self.owner)?;
        atomic(&self.mutex, &self.owner)
    }
    fn release(&self) -> Result<()> {
        self.owned()?;
        let token = self.owner["owner_token"].as_str().unwrap();
        remove(&self.resource, token)?;
        remove(&self.mutex, token)
    }
}
fn alive(pid: u32) -> bool {
    (unsafe { libc::kill(-(pid as i32), 0) }) == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
pub(crate) fn terminate(child: &mut Child) -> Result<()> {
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        if !alive(child.id()) {
            let _ = child.try_wait();
            return Ok(());
        }
        unsafe {
            libc::kill(-(child.id() as i32), signal);
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let _ = child.try_wait();
            if !alive(child.id()) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    require(
        !alive(child.id()),
        "child group cleanup unproven; retaining lock",
    )
}
fn force(args: &Args, common: &Path) -> Result<Value> {
    let resource = args.get("--resource")?;
    require(RESOURCES.contains(&resource), "unknown resource")?;
    let token = args.get("--owner-token")?;
    require(
        !token.is_empty() && token.len() <= 128 && !token.chars().any(|c| c < ' '),
        "exact owner token required",
    )?;
    let reason = args.get("--reason")?;
    require(
        !reason.trim().is_empty()
            && reason.len() <= 1000
            && !reason
                .chars()
                .any(|c| c < ' ' && !['\t', '\n'].contains(&c)),
        "audit reason required",
    )?;
    let root = common.join("ai-gateway-locks");
    for other in RESOURCES {
        require(
            other == resource || !root.join(other).exists(),
            "opposite resource exists",
        )?;
    }
    let directories = [root.join(".low-memory"), root.join(resource)];
    let mut valid = vec![];
    let mut invalid = 0;
    for directory in directories.iter().filter(|p| p.exists()) {
        require(allowed(directory, token)?, "ambiguous lock files")?;
        match load(&directory.join("owner.json")) {
            Ok(owner) => {
                require(
                    owner["owner_token"] == token && owner["resource"] == resource,
                    "exact token/resource mismatch",
                )?;
                valid.push(owner);
            }
            Err(_) => invalid += 1,
        }
    }
    require(!valid.is_empty(), "no valid receipt verifies owner token")?;
    let host = host()?;
    let cross = valid.iter().any(|o| o["hostname"] != host);
    let live = valid
        .iter()
        .any(|o| o["hostname"] == host && !pid_dead(o["pid"].as_i64().unwrap() as i32));
    require(
        !cross || args.flag("--allow-cross-host"),
        "cross-host override required",
    )?;
    require(
        !live || args.flag("--allow-live-owner"),
        "live-owner override required",
    )?;
    for directory in directories.iter().rev().filter(|p| p.exists()) {
        remove(directory, token)?;
    }
    Ok(
        json!({"action":"force-release","resource":resource,"owner_token":token,"reason":redact_text(reason.trim()),"cross_host_override":cross,"live_owner_override":live,"ambiguous_receipts_removed":invalid}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    let action = args
        .positionals
        .get(1)
        .map(String::as_str)
        .ok_or("resource-lock action required")?;
    let (worktree, common, head) = identity(&args.root())?;
    if action == "status" {
        let mut result = serde_json::Map::new();
        for name in [".low-memory", "integration-runtime", "rust-build"] {
            let path = common
                .join("ai-gateway-locks")
                .join(name)
                .join("owner.json");
            result.insert(
                name.into(),
                if !path.exists() {
                    Value::Null
                } else {
                    load(&path).unwrap_or_else(|e| json!({"error":e.to_string()}))
                },
            );
        }
        return Ok(Value::Object(result));
    }
    if action == "force-release" {
        return force(args, &common);
    }
    require(action == "run", "unknown lock operation")?;
    let resource = args.get("--resource")?;
    require(RESOURCES.contains(&resource), "unknown lock resource")?;
    let timeout = args
        .values
        .get("--timeout-seconds")
        .map(|s| s.parse::<u64>())
        .transpose()?
        .unwrap_or(7200);
    let heartbeat = args
        .values
        .get("--heartbeat-seconds")
        .map(|s| s.parse::<f64>())
        .transpose()?
        .unwrap_or(10.0);
    require(
        timeout > 0 && heartbeat.is_finite() && heartbeat > 0.0 && heartbeat < (timeout as f64),
        "invalid timeout/heartbeat bounds",
    )?;
    let expected = args.get("--expected-end-condition")?;
    require(
        !expected.trim().is_empty(),
        "expected end condition required",
    )?;
    let command = args
        .positionals
        .get(2..)
        .filter(|a| !a.is_empty())
        .ok_or("command after -- required")?;
    require(
        command.iter().all(|s| !s.is_empty()),
        "command arguments must be nonempty",
    )?;
    let timestamp = now();
    let mut owner = json!({"schema_version":"ai-gateway/resource-lock/v1","owner_token":uuid::Uuid::new_v4().simple().to_string(),"resource":resource,"hostname":host()?,"pid":std::process::id(),"worktree_path":worktree,"git_head":head,"command":redact(command),"start_time":timestamp,"heartbeat_time":timestamp,"timeout_seconds":timeout,"expected_end_condition":redact_text(expected)});
    if let Some(raw) = args.values.get("--runtime-identity-json") {
        require(
            !raw.is_empty() && raw.len() <= 65536,
            "runtime identity bounds",
        )?;
        let value = strict_json(raw.as_bytes())?;
        require(value.is_object(), "runtime identity must be object")?;
        owner["runtime_identity"] = sanitize(&value, None, 0)?;
    }
    let root = common.join("ai-gateway-locks");
    let mut lock = Lock {
        mutex: root.join(".low-memory"),
        resource: root.join(resource),
        root,
        owner,
    };
    SIGNAL.store(0, Ordering::SeqCst);
    let previous_int = unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        )
    };
    let previous_term = unsafe {
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        )
    };
    let result = (|| -> Result<i32> {
        lock.acquire()?;
        let signal = SIGNAL.load(Ordering::SeqCst);
        if signal != 0 {
            lock.release()?;
            return Ok(128 + signal);
        }
        let mut process = Command::new(&command[0]);
        process.args(&command[1..]).current_dir(&worktree);
        unsafe {
            process.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                lock.release()?;
                return Err(error.into());
            }
        };
        let mut next = Instant::now() + Duration::from_secs_f64(heartbeat);
        let outcome = loop {
            let signal = SIGNAL.load(Ordering::SeqCst);
            if signal != 0 {
                unsafe {
                    libc::kill(-(child.id() as i32), signal);
                };
                break Ok(128 + signal);
            }
            if Instant::now() >= next {
                if let Err(error) = lock.heartbeat() {
                    break Err(error);
                }
                next = Instant::now() + Duration::from_secs_f64(heartbeat);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    break Ok(status
                        .code()
                        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
                }
                Ok(None) => {}
                Err(e) => break Err(e.into()),
            };
            std::thread::sleep(Duration::from_millis(100));
        };
        terminate(&mut child)?;
        lock.release()?;
        outcome
    })();
    unsafe {
        libc::signal(libc::SIGINT, previous_int);
        libc::signal(libc::SIGTERM, previous_term);
    };
    Ok(json!({"_exit":result?}))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> Lock {
        let root =
            std::env::temp_dir().join(format!("platform-lock-{name}-{}", std::process::id()));
        let now = now();
        let owner = json!({"schema_version":"ai-gateway/resource-lock/v1","owner_token":uuid::Uuid::new_v4().simple().to_string(),"resource":"rust-build","hostname":host().unwrap(),"pid":std::process::id(),"worktree_path":"/fixture","git_head":"a".repeat(40),"command":["fixture"],"start_time":now,"heartbeat_time":now,"timeout_seconds":10,"expected_end_condition":"fixture exits"});
        Lock {
            mutex: root.join(".low-memory"),
            resource: root.join("rust-build"),
            root,
            owner,
        }
    }
    #[test]
    fn lock_excludes_other_resource_and_releases_owned_receipts() {
        let mut lock = fixture("exclusive");
        lock.acquire().unwrap();
        let other = Lock {
            root: lock.root.clone(),
            mutex: lock.mutex.clone(),
            resource: lock.root.join("integration-runtime"),
            owner: lock.owner.clone(),
        };
        assert!(other.acquire().is_err());
        lock.heartbeat().unwrap();
        lock.release().unwrap();
        assert!(!lock.mutex.exists() && !lock.resource.exists());
        fs::remove_dir(&lock.root).unwrap();
    }
    #[test]
    fn stale_reap_requires_dead_same_host_and_elapsed_timeout() {
        let lock = fixture("stale");
        lock.acquire().unwrap();
        let mut stale = lock.owner.clone();
        stale["pid"] = json!(i32::MAX);
        stale["heartbeat_time"] =
            json!((chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339());
        atomic(&lock.mutex, &stale).unwrap();
        atomic(&lock.resource, &stale).unwrap();
        lock.reap().unwrap();
        assert!(!lock.mutex.exists() && !lock.resource.exists());
        fs::remove_dir(&lock.root).unwrap();
    }
    #[test]
    fn native_child_group_termination_is_proven() {
        let mut command = Command::new("/bin/sleep");
        command.arg("60").process_group(0);
        let mut child = command.spawn().unwrap();
        assert!(alive(child.id()));
        terminate(&mut child).unwrap();
        assert!(!alive(child.id()));
    }
    #[test]
    fn receipt_redaction_preserves_execution_args() {
        let args = vec![
            "run".into(),
            "--api-key".into(),
            "example-secret".into(),
            "postgres://user:example@localhost/db".into(),
            "ACCESS_TOKEN=example".into(),
        ];
        let redacted = redact(&args);
        assert_eq!(redacted[2], "<redacted>");
        assert_eq!(redacted[3], "postgres://<redacted>@localhost/db");
        assert_eq!(redacted[4], "ACCESS_TOKEN=<redacted>");
        assert_eq!(args[2], "example-secret");
    }
    #[test]
    fn identity_sanitizes_and_bounds() {
        let value = sanitize(
            &json!({"apiKey":"example","nested":{"url":"https://u:p@host/path"}}),
            None,
            0,
        )
        .unwrap();
        assert_eq!(value["apiKey"], "<redacted>");
        assert_eq!(value["nested"]["url"], "https://<redacted>@host/path");
        assert!(sanitize(&json!("x".repeat(4097)), None, 0).is_err());
    }
    #[test]
    fn ambiguous_lock_files_are_never_removed() {
        let path = std::env::temp_dir().join(format!("platform-lock-test-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("unrelated"), "keep").unwrap();
        assert!(remove(&path, "token").is_err());
        assert!(path.join("unrelated").exists());
        fs::remove_dir_all(path).unwrap();
    }
}
