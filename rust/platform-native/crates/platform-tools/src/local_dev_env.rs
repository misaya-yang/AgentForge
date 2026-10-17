//! Explicit preparation of private local development inputs. Does not touch Docker or PostgreSQL.
use crate::*;
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};

const PROJECT: &str = "agentforge-dev-7801d4a9a4";
const PRINCIPALS: &[&str] = &[
    "migrator",
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
];

fn secret(bytes: usize) -> Result<String> {
    let mut value = vec![0; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut value)?;
    Ok(value.iter().map(|b| format!("{b:02x}")).collect())
}
fn required<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    values
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("private local configuration requires {key}").into())
}
fn dsn(user: &str, password: &str, host: &str, port: u16, database: &str) -> Result<String> {
    let mut url = reqwest::Url::parse("postgresql://127.0.0.1/")?;
    require(
        url.set_username(user).is_ok() && url.set_password(Some(password)).is_ok(),
        "invalid local database identity",
    )?;
    require(
        url.set_host(Some(host)).is_ok() && url.set_port(Some(port)).is_ok(),
        "invalid local database address",
    )?;
    url.set_path(&format!("/{database}"));
    Ok(url.into())
}
fn prepare(values: BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    if let Some(project) = values.get("COMPOSE_PROJECT_NAME").filter(|s| !s.is_empty()) {
        require(
            project == PROJECT,
            "refusing a different local Compose project",
        )?;
    }
    let host = values
        .get("POSTGRES_HOST")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "127.0.0.1".into());
    require(
        host == "127.0.0.1",
        "local development requires the Compose-published numeric 127.0.0.1 PostgreSQL host",
    )?;
    let port: u16 = required(&values, "POSTGRES_PORT")?
        .parse()
        .map_err(|_| "invalid local PostgreSQL port")?;
    require(port != 0, "invalid local PostgreSQL port")?;
    let database = required(&values, "POSTGRES_DB")?.to_owned();
    require(
        regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$")?.is_match(&database),
        "invalid local database name",
    )?;
    let admin = dsn(
        required(&values, "POSTGRES_USER")?,
        required(&values, "POSTGRES_PASSWORD")?,
        &host,
        port,
        &database,
    )?;
    let prefix = values
        .get("AI_GATEWAY_ROLE_PREFIX")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "ai_gateway_".into());
    require(
        regex::Regex::new(r"^[a-z][a-z0-9_]{0,20}_$")?.is_match(&prefix),
        "invalid authority role prefix",
    )?;
    let mut changes = BTreeMap::new();
    require(
        values
            .get("AI_PLATFORM_LOCAL_DEV_CREDENTIALS")
            .filter(|s| !s.is_empty())
            .is_none_or(|s| s == "true"),
        "local credential initialization is explicitly disabled; private env unchanged",
    )?;
    for (key, value) in [
        ("COMPOSE_PROJECT_NAME", PROJECT),
        ("POSTGRES_HOST", &host),
        ("LOCAL_S3_PORT", "9000"),
        ("LOCAL_S3_CONSOLE_PORT", "9001"),
        ("AI_GATEWAY_ROLE_PREFIX", &prefix),
        ("AI_PLATFORM_LOCAL_DEV_CREDENTIALS", "true"),
    ] {
        if values.get(key).is_none_or(|v| v.is_empty()) {
            changes.insert(key.into(), value.into());
        }
    }
    for (key, bytes) in [("LOCAL_S3_ACCESS_KEY", 10), ("LOCAL_S3_SECRET_KEY", 32)] {
        if values.get(key).is_none_or(|v| v.is_empty()) {
            changes.insert(key.into(), secret(bytes)?);
        }
    }
    if let Some(existing) = values
        .get("AI_GATEWAY_DATABASE_ADMIN_DSN")
        .filter(|s| !s.is_empty())
    {
        require(
            existing == &admin,
            "existing administrator DSN conflicts with local bootstrap inputs",
        )?;
    } else {
        changes.insert("AI_GATEWAY_DATABASE_ADMIN_DSN".into(), admin.clone());
    }
    for principal in PRINCIPALS {
        let password_key = format!("LOCAL_DB_{}_PASSWORD", principal.to_ascii_uppercase());
        let password = match values.get(&password_key).filter(|s| !s.is_empty()) {
            Some(p) => p.clone(),
            None => {
                let value = secret(32)?;
                changes.insert(password_key.clone(), value.clone());
                value
            }
        };
        let role_dsn = dsn(
            &format!("{prefix}{principal}"),
            &password,
            &host,
            port,
            &database,
        )?;
        let dsn_key = format!("AI_GATEWAY_DATABASE_{}_DSN", principal.to_ascii_uppercase());
        if let Some(existing) = values.get(&dsn_key).filter(|s| !s.is_empty()) {
            require(
                existing == &role_dsn,
                format!(
                    "existing {principal} DSN conflicts with private role password; no credential was changed"
                ),
            )?;
        } else {
            changes.insert(dsn_key, role_dsn.clone());
        }
        let application_key = match *principal {
            "gateway" => Some("GATEWAY_DATABASE__DSN"),
            "knowledge_api" => Some("KNOWLEDGE_DATABASE__DSN"),
            _ => None,
        };
        if let Some(application_key) = application_key {
            if let Some(existing) = values.get(application_key).filter(|s| !s.is_empty()) {
                require(
                    existing == &admin || existing == &role_dsn,
                    "existing application DSN is outside the selected local profile",
                )?;
            }
            if values.get(application_key) != Some(&role_dsn) {
                changes.insert(application_key.into(), role_dsn);
            }
        }
    }
    // Host services need the same Redis instance through its published loopback port.
    let password = required(&values, "REDIS_PASSWORD")?;
    require(
        password.bytes().all(|b| b.is_ascii_alphanumeric()),
        "local Redis password must be URL-safe alphanumeric",
    )?;
    let redis_port: u16 = required(&values, "REDIS_PORT")?
        .parse()
        .map_err(|_| "invalid local Redis port")?;
    require(redis_port != 0, "invalid local Redis port")?;
    let redis = format!("redis://:{password}@127.0.0.1:{redis_port}/3");
    if let Some(existing) = values
        .get("INTERNAL_COMM_REDIS_URL")
        .filter(|s| !s.is_empty())
    {
        require(
            existing == &redis || existing == &format!("redis://:{password}@redis:6379/3"),
            "existing communication Redis URL is outside the selected local profile",
        )?;
    }
    if values.get("INTERNAL_COMM_REDIS_URL") != Some(&redis) {
        changes.insert("INTERNAL_COMM_REDIS_URL".into(), redis);
    }
    Ok(changes)
}

pub fn run(args: &Args) -> Result<Value> {
    let path = PathBuf::from(args.get("--env-file")?);
    let metadata = std::fs::symlink_metadata(&path)?;
    require(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "local env must be a regular non-symlink file",
    )?;
    require(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.permissions().mode() & 0o077 == 0,
        "local env must be private and owned by the current user",
    )?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid env filename")?;
    require(
        (name == ".env" || name.starts_with(".env.")) && !name.ends_with(".example"),
        "local preparation requires an explicit private .env file, never a public example",
    )?;
    let canonical = path.canonicalize()?;
    let root = args.root().canonicalize()?;
    require(
        canonical.parent() == Some(root.as_path()),
        "local development env must belong to the selected repository root",
    )?;
    if root.join(".git").exists() && canonical.starts_with(&root) {
        let tracked = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["ls-files", "--error-unmatch", "--"])
            .arg(&canonical)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        require(
            !tracked.success(),
            "refusing to write credentials into a tracked file",
        )?;
    }
    let original = std::fs::read_to_string(&path)?;
    let mut values = BTreeMap::new();
    for row in dotenvy::from_read_iter(original.as_bytes()) {
        let (key, value) = row.map_err(|_| "invalid private env syntax; contents withheld")?;
        require(
            values.insert(key, value).is_none(),
            "duplicate key in private env; contents withheld",
        )?;
    }
    let changes = prepare(values)?;
    if !changes.is_empty() {
        let mut text = original
            .lines()
            .filter(|line| {
                !line.split_once('=').is_some_and(|(k, _)| {
                    changes.contains_key(k.trim().strip_prefix("export ").unwrap_or(k.trim()))
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str("\n# Native local development inputs (private; no password rotation)\n");
        for (key, value) in &changes {
            require(
                !value.contains(['\r', '\n', '\0']),
                "invalid local env value; contents withheld",
            )?;
            text.push_str(&format!("{key}={value}\n"));
        }
        let temp = path.with_file_name(format!(".env.local-{}.tmp", uuid::Uuid::new_v4()));
        let update = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            require(
                std::fs::read_to_string(&path)? == original,
                "private env changed concurrently; update refused",
            )?;
            std::fs::rename(&temp, &path)?;
            Ok(())
        })();
        if temp.exists() {
            let _ = std::fs::remove_file(&temp);
        }
        update?;
    }
    Ok(
        json!({"result":"pass","command":"local-dev-env","project":PROJECT,"updated_keys":changes.keys().collect::<Vec<_>>(),"values_withheld":true,"docker_mutated":false,"database_mutated":false,"credentials_rotated":false}),
    )
}
