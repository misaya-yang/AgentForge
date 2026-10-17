//! Explicit-input private configuration export; no dotenv discovery or network.
use ai_platform_config_loader::{Environment, resolve_settings};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(b: bool) -> Result<()> {
    if b {
        Ok(())
    } else {
        Err("invalid explicit configuration".into())
    }
}
fn read(path: &Path) -> Result<Value> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = opts.open(path)?;
    require(f.metadata()?.is_file())?;
    let mut bytes = vec![];
    f.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    require(bytes.len() <= 4 * 1024 * 1024)?;
    let v: Value = serde_json::from_slice(&bytes)?;
    require(v.is_object())?;
    Ok(v)
}
fn sdk_profile(environment: &Environment) -> bool {
    let e = |k: &str| environment.get(k).map(String::as_str).unwrap_or("");
    if e("SDK_AGENT_WORKER_TOKEN").len() < 32 {
        return false;
    }
    match e("SDK_AGENT_WORKER_URL") {
        "http://127.0.0.1:8096" => e("SDK_AGENT_WORKER_TLS_CA_FILE").is_empty(),
        "https://sdk-agent-worker:8096" => {
            let ca = e("SDK_AGENT_WORKER_TLS_CA_FILE");
            !ca.is_empty() && Path::new(ca).is_absolute()
        }
        _ => false,
    }
}
fn prepare(environment: Environment, native: Value) -> Result<Value> {
    let settings = resolve_settings(&environment)?;
    require(!serde_json::to_string(&native)?.contains('<'))?;
    let e = |k: &str| environment.get(k).map(String::as_str).unwrap_or("");
    require(e("AI_PLATFORM_TOPOLOGY_MODE") == "full")?;
    require(settings["host"] == "0.0.0.0" && settings["port"] == 8080)?;
    for (section, key, host, port) in [
        ("database", "dsn", "postgres", 5432),
        ("redis", "url", "redis", 6379),
    ] {
        let v = &settings[section];
        let address = url::Url::parse(v[key].as_str().ok_or("missing address")?)?;
        require(
            v["enabled"] == true
                && address.host_str() == Some(host)
                && address.port() == Some(port),
        )?;
    }
    require(settings["database"]["auto_init"] == false)?;
    require(
        e("KB_SERVICE_URL") == "http://knowledge-service:8092"
            && e("AI_PLATFORM_AGENT_RUNTIME_URL") == "http://agent-runtime:8094"
            && e("AI_PLATFORM_CAPABILITY_WORKER_URL") == "http://agent-capability-worker:8095",
    )?;
    require(
        native["allow_private_knowledge_http"] == true
            && e("AI_PLATFORM_INTERNAL_TOKEN").chars().count() >= 16,
    )?;
    require(
        native["kernel_revision"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && !s.contains('<')),
    )?;
    if environment.contains_key("SDK_AGENT_WORKER_URL") {
        require(sdk_profile(&environment))?;
    }
    require(
        native["local_channel"] == "disabled" && native["confluence_registration"] == "disabled",
    )?;
    Ok(json!({"settings":settings,"environment":environment,"native":native}))
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut values = std::collections::BTreeMap::new();
    require(args.len() % 2 == 0)?;
    for pair in args.chunks_exact(2) {
        require(
            [
                "--environment-json",
                "--native-json",
                "--output",
                "--repo-root",
            ]
            .contains(&pair[0].as_str()),
        )?;
        require(
            values
                .insert(pair[0].clone(), PathBuf::from(&pair[1]))
                .is_none(),
        )?;
    }
    let root = values
        .get("--repo-root")
        .cloned()
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let output = values.get("--output").ok_or("output required")?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let output = parent.join(output.file_name().ok_or("file name required")?);
    require(!output.starts_with(root))?;
    let env: Environment = serde_json::from_value(read(
        values
            .get("--environment-json")
            .ok_or("environment required")?,
    )?)?;
    let native = read(values.get("--native-json").ok_or("native required")?)?;
    let bytes = serde_json::to_vec(&prepare(env, native)?)?;
    require(bytes.len() <= 4 * 1024 * 1024)?;
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(output)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    Ok(())
}
fn main() {
    if run().is_err() {
        eprintln!(
            "Configuration preparation failed; check explicit inputs, topology, dependencies and external output path. No input values are logged."
        );
        std::process::exit(1)
    }
    println!(
        "Private configuration file created. Run the native binary's --check-config-stdin before runtime approval; this export is not readiness evidence."
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_profile_keeps_loopback_http_and_requires_exact_private_https_identity() {
        let mut env = Environment::from([
            ("SDK_AGENT_WORKER_TOKEN".into(), "a".repeat(32)),
            (
                "SDK_AGENT_WORKER_URL".into(),
                "http://127.0.0.1:8096".into(),
            ),
        ]);
        assert!(sdk_profile(&env));
        env.insert(
            "SDK_AGENT_WORKER_URL".into(),
            "http://sdk-agent-worker:8096".into(),
        );
        assert!(!sdk_profile(&env));
        env.insert(
            "SDK_AGENT_WORKER_URL".into(),
            "https://sdk-agent-worker:8096".into(),
        );
        assert!(!sdk_profile(&env));
        env.insert(
            "SDK_AGENT_WORKER_TLS_CA_FILE".into(),
            "/run/sdk-worker/ca.pem".into(),
        );
        assert!(sdk_profile(&env));
        for url in [
            "https://other-worker:8096",
            "https://sdk-agent-worker:8096/path",
            "https://sdk-agent-worker:8096?key=1",
            "http://127.0.0.1:8096",
        ] {
            env.insert("SDK_AGENT_WORKER_URL".into(), url.into());
            assert!(!sdk_profile(&env));
        }
        env.insert(
            "SDK_AGENT_WORKER_URL".into(),
            "https://sdk-agent-worker:8096".into(),
        );
        env.insert("SDK_AGENT_WORKER_TLS_CA_FILE".into(), "relative.pem".into());
        assert!(!sdk_profile(&env));
    }
    #[test]
    fn objects_only() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("input");
        std::fs::write(&p, "[]").unwrap();
        assert!(read(&p).is_err());
        std::fs::write(&p, "{}").unwrap();
        assert_eq!(read(&p).unwrap(), json!({}));
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("input");
        std::fs::write(&p, "{}").unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(read(&link).is_err());
    }
}
