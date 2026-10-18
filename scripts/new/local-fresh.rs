//! Std-only local operations. No Cargo, shell evaluation, image pull, migration
//! chain, volume deletion, DB superuser TCP or credential output.
use std::{
    collections::BTreeMap,
    env, fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[path = "local-fresh-memory.rs"]
mod memory_observation;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const PROJECT: &str = "agentforge-dev-7801d4a9a4";
const IMAGES: &[&str] = &[
    "ADMIN_BOOTSTRAP_IMAGE",
    "GATEWAY_IMAGE",
    "KNOWLEDGE_IMAGE",
    "FRONTEND_IMAGE",
    "MIGRATE_IMAGE",
    "AI_PLATFORM_AGENT_RUNTIME_IMAGE",
    "AGENT_CAPABILITY_WORKER_IMAGE",
];
const FILES: &[&str] = &[
    "qdrant-files",
    "files-qdrant-storage",
    "files-postgres",
    "files-socket",
    "files-authority",
    "files-gateway",
    "files-runtime",
    "files-capability_worker",
    "files-knowledge_api",
    "files-knowledge_worker",
    "files-model-gateway",
    "files-model-runtime",
    "files-model-knowledge-api",
    "files-model-knowledge-worker",
];
fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn text(path: &Path, private: bool) -> Result<String> {
    let m = fs::symlink_metadata(path).map_err(|_| "selected input file unavailable")?;
    ensure(
        m.is_file() && !m.file_type().is_symlink() && m.len() < 2 * 1024 * 1024,
        "regular bounded input file required",
    )?;
    ensure(
        m.permissions().mode() & 0o022 == 0 && (!private || m.permissions().mode() & 0o077 == 0),
        "input file permissions unsafe",
    )?;
    fs::read_to_string(path).map_err(|_| "input encoding invalid".into())
}
fn parse(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line
            .split_once('=')
            .ok_or("env syntax invalid; contents withheld")?;
        ensure(
            !key.is_empty()
                && key
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
            "env key invalid; contents withheld",
        )?;
        let raw = raw.trim();
        let value = if raw.starts_with(['\'', '"']) {
            ensure(
                raw.len() >= 2 && raw.ends_with(raw.as_bytes()[0] as char),
                "env quoting invalid; contents withheld",
            )?;
            raw[1..raw.len() - 1].to_owned()
        } else {
            raw.to_owned()
        };
        ensure(
            !value.chars().any(char::is_control),
            "env control character refused",
        )?;
        ensure(
            result.insert(key.to_owned(), value).is_none(),
            "duplicate env key refused",
        )?;
    }
    Ok(result)
}
fn needed<'a>(e: &'a BTreeMap<String, String>, k: &str) -> Result<&'a str> {
    e.get(k)
        .filter(|v| !v.is_empty())
        .map(String::as_str)
        .ok_or_else(|| format!("missing local input: {k}").into())
}
fn command(program: &str, args: &[String]) -> Command {
    let mut c = Command::new(program);
    c.args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "SSH_AUTH_SOCK",
    ] {
        if let Some(v) = env::var_os(key) {
            c.env(key, v);
        }
    }
    c.env("COMPOSE_PARALLEL_LIMIT", "1");
    c
}
fn run(program: &str, args: &[String], label: &str) -> Result<String> {
    let mut c = command(program, args);

    let out = c
        .output()
        .map_err(|_| format!("{label}: executable unavailable"))?;
    ensure(
        out.status.success(),
        &format!("{label} failed; command output withheld"),
    )?;
    String::from_utf8(out.stdout).map_err(|_| format!("{label}: output encoding invalid").into())
}
fn docker(args: &[String], label: &str) -> Result<String> {
    run("docker", args, label)
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|v| (*v).to_owned()).collect()
}
fn safe_path(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or("path encoding invalid")?;
    ensure(
        path.is_absolute() && !value.contains([',', '\n', '\r', '\0']),
        "canonical absolute bind path required",
    )?;
    Ok(value.into())
}
fn mount(source: &Path, target: &Path, ro: bool) -> Result<String> {
    Ok(format!(
        "type=bind,src={},dst={}{}",
        safe_path(source)?,
        safe_path(target)?,
        if ro { ",readonly" } else { "" }
    ))
}
fn ownership(root: &Path) -> Result<()> {
    let ids = docker(
        &strings(&[
            "ps",
            "-a",
            "--filter",
            &format!("label=com.docker.compose.project={PROJECT}"),
            "--format",
            "{{.ID}}",
        ]),
        "owned inventory",
    )?;
    ensure(
        !ids.trim().is_empty(),
        "existing selected project not found; no second project created",
    )?;
    for id in ids.lines().filter(|v| !v.is_empty()) {
        let labels = docker(
            &strings(&[
                "inspect",
                "--format",
                "{{index .Config.Labels \"com.docker.compose.project\"}}|{{index .Config.Labels \"com.docker.compose.project.working_dir\"}}",
                id,
            ]),
            "ownership inspection",
        )?;
        ensure(
            labels.trim() == format!("{PROJECT}|{}", root.display()),
            "project working-directory ownership mismatch",
        )?;
    }
    Ok(())
}
fn ipv4_range(value: &str) -> Result<(u32, u32)> {
    let (ip, bits) = value.split_once('/').ok_or("Docker IPv4 CIDR invalid")?;
    let ip = ip
        .parse::<std::net::Ipv4Addr>()
        .map_err(|_| "Docker IPv4 CIDR invalid")?;
    let bits = bits
        .parse::<u32>()
        .map_err(|_| "Docker IPv4 prefix invalid")?;
    ensure(bits <= 32, "Docker IPv4 prefix invalid")?;
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    Ok((u32::from(ip) & mask, mask))
}
fn overlaps(a: (u32, u32), b: (u32, u32)) -> bool {
    (a.0 & b.1) == b.0 || (b.0 & a.1) == a.0
}
fn memory_limit(value: &str) -> Result<u64> {
    let value = value.trim().to_ascii_lowercase();
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<f64>()
        .map_err(|_| "service memory bound invalid")?;
    let scale = match &value[split..] {
        "" | "b" => 1u64,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return Err("service memory unit unsupported".into()),
    };
    let bytes = number * scale as f64;
    ensure(
        bytes.is_finite() && bytes > 0.0 && bytes <= 16.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "service memory bound invalid",
    )?;
    Ok(bytes.ceil() as u64)
}
fn selected_memory(root: &Path, e: &BTreeMap<String, String>, service: &str) -> Result<u64> {
    let source = match service {
        "agent-python-sandbox" => "deploy/local/compose.core-sandbox.yml",
        "sdk-agent-worker" => "deploy/local/compose.core-sdk.yml",
        _ => "deploy/local/compose.core.yml",
    };
    let raw = fs::read_to_string(root.join(source))
        .map_err(|_| "public service memory configuration unavailable")?;
    let mut selected = false;
    for line in raw.lines() {
        if line == format!("  {service}:") {
            selected = true;
            continue;
        }
        if selected && line.starts_with("  ") && !line.starts_with("    ") {
            break;
        }
        if selected {
            if let Some(value) = line.strip_prefix("    mem_limit:") {
                let value = value.trim().trim_matches(['\'', '"']);
                let resolved = if let Some(inner) = value
                    .strip_prefix('$')
                    .and_then(|v| v.strip_prefix('{'))
                    .and_then(|v| v.strip_suffix('}'))
                {
                    let (key, default) = inner
                        .split_once(":-")
                        .ok_or("explicit bounded memory default required")?;
                    e.get(key)
                        .filter(|v| !v.is_empty())
                        .map(String::as_str)
                        .unwrap_or(default)
                } else {
                    value
                };
                return memory_limit(resolved);
            }
        }
    }
    Err("selected service has no explicit memory bound".into())
}
fn memory(database_only: bool, requested: u64) -> Result<()> {
    memory_observation::admit(memory_observation::snapshot()?, database_only, requested)
}

struct Options {
    root: PathBuf,
    out: PathBuf,
    input: PathBuf,
    data: PathBuf,
    tools: String,
    client: String,
    backend: String,
    images: Vec<PathBuf>,
    mode: String,
    sandbox: bool,
    sdk: bool,
    security_generation: String,
    console_inputs: Vec<(String, PathBuf)>,
}
impl Options {
    fn from_args() -> Result<Self> {
        let mut values = BTreeMap::new();
        let mut images = Vec::new();
        let mut mode = None;
        let mut sandbox = false;
        let mut sdk = false;
        let mut it = env::args().skip(1);
        while let Some(k) = it.next() {
            if matches!(
                k.as_str(),
                "prepare" | "database" | "secure-extend" | "up" | "check"
            ) {
                ensure(mode.replace(k).is_none(), "one action required")?;
            } else if k == "--sandbox" {
                sandbox = true;
            } else if k == "--sdk" {
                sdk = true;
            } else if k == "--image-env" {
                images.push(PathBuf::from(
                    it.next().ok_or("image receipt path required")?,
                ));
            } else {
                ensure(
                    [
                        "--root",
                        "--directory",
                        "--env-file",
                        "--data-input",
                        "--tools-image",
                        "--client-cidr",
                        "--backend-cidr",
                        "--security-generation",
                        "--console-cert-file",
                        "--console-key-file",
                        "--console-ca-file",
                    ]
                    .contains(&k.as_str()),
                    "unknown local fresh argument",
                )?;
                ensure(
                    values
                        .insert(k, it.next().ok_or("argument value required")?)
                        .is_none(),
                    "duplicate local argument",
                )?;
            }
        }
        let get = |k: &str| -> Result<String> {
            values
                .get(k)
                .cloned()
                .ok_or_else(|| format!("missing {k}").into())
        };
        let root = PathBuf::from(get("--root")?).canonicalize()?;
        let out = PathBuf::from(get("--directory")?);
        ensure(
            out.is_absolute()
                && !out.starts_with(&root)
                && !out
                    .components()
                    .any(|v| matches!(v, std::path::Component::ParentDir)),
            "new external private directory required",
        )?;
        let input = PathBuf::from(
            values
                .get("--env-file")
                .cloned()
                .unwrap_or_else(|| root.join(".env.dev").display().to_string()),
        )
        .canonicalize()?;
        let data = PathBuf::from(get("--data-input")?).canonicalize()?;
        let tools = get("--tools-image")?;
        ensure(
            !tools.contains(char::is_whitespace)
                && !tools.is_empty()
                && !tools.starts_with('-')
                && !tools.starts_with("agentforge-native-dev"),
            "dedicated admitted ops image required, never compiler image",
        )?;
        Ok(Self {
            root,
            out,
            input,
            data,
            tools,
            client: get("--client-cidr")?,
            backend: get("--backend-cidr")?,
            images,
            mode: mode.unwrap_or_else(|| "up".into()),
            sandbox,
            sdk,
            security_generation: values
                .get("--security-generation")
                .cloned()
                .unwrap_or("transport-r1".into()),
            console_inputs: [
                "--console-cert-file",
                "--console-key-file",
                "--console-ca-file",
            ]
            .into_iter()
            .filter_map(|k| values.get(k).map(|v| (k.into(), PathBuf::from(v))))
            .collect(),
        })
    }
    fn database_only(&self) -> bool {
        matches!(self.mode.as_str(), "prepare" | "database")
    }
    fn core(&self) -> bool {
        matches!(self.mode.as_str(), "up" | "check")
    }
    fn security_selection(&self) -> Result<PathBuf> {
        let active = text(&self.out.join("security/ACTIVE"), true)?;
        let name = active.trim();
        ensure(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "active security generation invalid",
        )?;
        let release = self.out.join("security/releases").join(name);
        ensure(
            !release.join("INCOMPLETE").exists()
                && text(&release.join("PREPARED"), true)?.trim() == "local-security/v1",
            "complete security generation required",
        )?;
        Ok(release)
    }
    fn effective_env(&self, path: &Path) -> Result<BTreeMap<String, String>> {
        let mut e = parse(&text(path, true)?)?;
        if self.core() && path == self.out.join("fresh.env") {
            let release = self.security_selection()?;
            let selection = parse(&text(&release.join("security.env"), true)?)?;
            let name = release
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("security generation invalid")?;
            let port = e
                .get("FRONTEND_PORT")
                .map(String::as_str)
                .unwrap_or("8081")
                .parse::<u16>()
                .map_err(|_| "fixed console port invalid")?;
            ensure(port > 0, "fixed console port required")?;
            let origin = format!("https://localhost:{port}");
            ensure(
                selection.len() == 6
                    && needed(&selection, "LOCAL_FRESH_SECURITY_DIRECTORY")?
                        == release.display().to_string()
                    && needed(&selection, "LOCAL_FRESH_SECURITY_GENERATION")? == name
                    && needed(&selection, "LOCAL_FRESH_NATIVE_OPTIONS_FILE")?
                        == release.join("native-gateway.json").display().to_string()
                    && needed(&selection, "LOCAL_FRESH_RUNTIME_BRIDGE_ENV_FILE")?
                        == self
                            .out
                            .join("security/application/agent-runtime/bridge.env")
                            .display()
                            .to_string()
                    && needed(&selection, "LOCAL_FRESH_CONSOLE_ORIGIN")? == origin
                    && needed(&selection, "LOCAL_FRESH_BFF_ORIGINS")? == format!("[\"{origin}\"]"),
                "security selection paths/origin must match the selected private generation",
            )?;
            for (key, value) in selection {
                ensure(
                    [
                        "LOCAL_FRESH_SECURITY_DIRECTORY",
                        "LOCAL_FRESH_SECURITY_GENERATION",
                        "LOCAL_FRESH_NATIVE_OPTIONS_FILE",
                        "LOCAL_FRESH_CONSOLE_ORIGIN",
                        "LOCAL_FRESH_BFF_ORIGINS",
                        "LOCAL_FRESH_RUNTIME_BRIDGE_ENV_FILE",
                    ]
                    .contains(&key.as_str()),
                    "security selector contains an unexpected key",
                )?;
                e.insert(key, value);
            }
        }
        for p in &self.images {
            for (k, v) in parse(&text(p, false)?)? {
                ensure(
                    IMAGES.contains(&k.as_str())
                        || [
                            "AGENT_PYTHON_SANDBOX_IMAGE",
                            "SDK_AGENT_WORKER_IMAGE",
                            "LOCAL_FRESH_TOOLS_IMAGE",
                        ]
                        .contains(&k.as_str()),
                    "image receipt may contain only image identities",
                )?;
                e.insert(k, v);
            }
        }
        Ok(e)
    }
    fn compose(&self) -> Vec<String> {
        let mut args = strings(&["compose", "--project-name", PROJECT, "--project-directory"]);
        args.push(self.root.display().to_string());
        args.push("--env-file".into());
        args.push(self.out.join("fresh.env").display().to_string());
        if self.core() {
            if let Ok(release) = self.security_selection() {
                args.push("--env-file".into());
                args.push(release.join("security.env").display().to_string());
            }
        }
        for p in &self.images {
            args.push("--env-file".into());
            args.push(
                p.canonicalize()
                    .unwrap_or_else(|_| p.clone())
                    .display()
                    .to_string(),
            );
        }
        args.push("-f".into());
        args.push(
            self.root
                .join(if self.database_only() {
                    "deploy/local/compose.core-database.yml"
                } else {
                    "deploy/local/compose.core.yml"
                })
                .display()
                .to_string(),
        );
        if self.sandbox && !self.database_only() {
            args.push("-f".into());
            args.push(
                self.root
                    .join("deploy/local/compose.core-sandbox.yml")
                    .display()
                    .to_string(),
            );
        }
        if self.sdk && !self.database_only() {
            args.push("-f".into());
            args.push(
                self.root
                    .join("deploy/local/compose.core-sdk.yml")
                    .display()
                    .to_string(),
            );
            args.extend(strings(&["--profile", "sdk-agents"]));
        }
        args
    }
    fn call(&self, args: &[&str], label: &str) -> Result<String> {
        let mut all = self.compose();
        all.extend(strings(args));
        docker(&all, label)
    }
    fn image_admission(&self, e: &BTreeMap<String, String>) -> Result<()> {
        ensure(
            e.get("LOCAL_FRESH_TOOLS_IMAGE")
                .is_none_or(|v| v == &self.tools),
            "ops image receipt must match explicit --tools-image",
        )?;
        let selected: Vec<&str> = if self.mode == "secure-extend" {
            vec![]
        } else if self.database_only() {
            vec!["MIGRATE_IMAGE"]
        } else {
            IMAGES
                .iter()
                .copied()
                .chain(self.sandbox.then_some("AGENT_PYTHON_SANDBOX_IMAGE"))
                .chain(self.sdk.then_some("SDK_AGENT_WORKER_IMAGE"))
                .collect()
        };
        for key in selected {
            let image = needed(e, key)?;
            ensure(
                !image.starts_with('-') && !image.contains(char::is_whitespace),
                "image identity invalid",
            )?;
            docker(
                &strings(&["image", "inspect", "--format", "{{.Id}}", image]),
                &format!("existing image admission {key}"),
            )?;
        }
        docker(
            &strings(&["image", "inspect", "--format", "{{.Id}}", &self.tools]),
            "existing ops image admission",
        )?;
        Ok(())
    }
    fn prepare(&self) -> Result<()> {
        ensure(
            !self.out.exists(),
            "preparation requires a new directory; use up/check to reuse prepared files",
        )?;
        let e = self.effective_env(&self.input)?;
        ensure(
            needed(&e, "COMPOSE_PROJECT_NAME")? == PROJECT,
            "selected env project mismatch",
        )?;
        self.image_admission(&e)?;
        fs::DirBuilder::new().mode(0o700).create(&self.out)?;
        fs::write(self.out.join("INCOMPLETE"), b"native preparation pending\n")?;
        fs::set_permissions(
            self.out.join("INCOMPLETE"),
            fs::Permissions::from_mode(0o600),
        )?;
        let uid = run("id", &strings(&["-u"]), "operator UID")?;
        let gid = run("id", &strings(&["-g"]), "operator GID")?;
        ensure(
            uid.trim().parse::<u32>().is_ok() && gid.trim().parse::<u32>().is_ok(),
            "operator identity invalid",
        )?;
        let mut args = strings(&[
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--user",
            &format!("{}:{}", uid.trim(), gid.trim()),
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev,size=64m,mode=1777",
        ]);
        for (source, target, ro) in [
            (
                self.input.clone(),
                PathBuf::from("/private/source.env"),
                true,
            ),
            (
                self.root.join("deploy"),
                PathBuf::from("/repo/deploy"),
                true,
            ),
            (self.out.clone(), self.out.clone(), false),
            (self.data.clone(), self.data.clone(), true),
        ] {
            args.push("--mount".into());
            args.push(mount(&source, &target, ro)?);
        }
        for p in &self.images {
            args.push("--mount".into());
            args.push(mount(&p.canonicalize()?, &p.canonicalize()?, true)?);
        }
        for key in [
            "QDRANT_TLS_CERT_FILE",
            "QDRANT_TLS_KEY_FILE",
            "QDRANT_CA_BUNDLE_FILE",
        ] {
            if let Some(value) = e.get(key).filter(|v| !v.is_empty()) {
                let p = PathBuf::from(value).canonicalize()?;
                args.push("--mount".into());
                args.push(mount(&p, &p, true)?);
            }
        }
        args.extend(strings(&[
            "--entrypoint",
            "/usr/local/bin/platform-tools",
            &self.tools,
            "local-fresh-inputs",
            "--repo-root",
            "/repo",
            "--env-file",
            "/private/source.env",
            "--directory",
            &self.out.display().to_string(),
            "--host-root",
            &self.root.display().to_string(),
            "--data-input",
            &self.data.display().to_string(),
            "--database",
            "gateway_dev_fresh",
            "--client-cidr",
            &self.client,
            "--backend-cidr",
            &self.backend,
            "--tools-image",
            &self.tools,
            "--openssl",
            "/usr/bin/openssl",
        ]));
        args.extend(strings(&[
            "--phase",
            if self.database_only() {
                "database"
            } else {
                "core"
            },
        ]));
        for p in &self.images {
            args.push("--image-env".into());
            args.push(p.canonicalize()?.display().to_string());
        }
        let receipt = docker(&args, "native offline preparation")?;
        ensure(
            self.out.join("PREPARED").is_file(),
            "native preparation did not complete",
        )?;
        fs::remove_file(self.out.join("INCOMPLETE"))?;
        fs::write(self.out.join("prepare-receipt.json"), receipt)?;
        fs::set_permissions(
            self.out.join("prepare-receipt.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        Ok(())
    }
    fn extend_security(&self) -> Result<()> {
        ensure(
            text(&self.out.join("PREPARED"), true)?.trim() == "agentforge-local-fresh/v1",
            "original prepared generation required; fresh.env is never replaced",
        )?;
        let e = self.effective_env(&self.out.join("fresh.env"))?;
        self.image_admission(&e)?;
        let security = self.out.join("security");
        if !security.exists() {
            fs::DirBuilder::new().mode(0o700).create(&security)?;
        }
        let uid = run("id", &strings(&["-u"]), "operator UID")?;
        let gid = run("id", &strings(&["-g"]), "operator GID")?;
        let mut args = strings(&[
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--memory",
            "256m",
            "--cpus",
            "1",
            "--pids-limit",
            "128",
            "--user",
            &format!("{}:{}", uid.trim(), gid.trim()),
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev,size=64m,mode=1777",
        ]);
        for (source, target, ro) in [
            (self.out.join("PREPARED"), self.out.join("PREPARED"), true),
            (self.out.join("fresh.env"), self.out.join("fresh.env"), true),
            (
                self.out.join("native-gateway.json"),
                self.out.join("native-gateway.json"),
                true,
            ),
            (security.clone(), security.clone(), false),
            (
                self.root.join("web/nginx.conf"),
                PathBuf::from("/repo/web/nginx.conf"),
                true,
            ),
        ] {
            args.push("--mount".into());
            args.push(mount(&source, &target, ro)?);
        }
        for (_, source) in &self.console_inputs {
            let canonical = source.canonicalize()?;
            args.push("--mount".into());
            args.push(mount(&canonical, &canonical, true)?);
        }
        args.extend(strings(&[
            "--entrypoint",
            "/usr/local/bin/platform-tools",
            &self.tools,
            "local-fresh-security-extend",
            "--directory",
            &self.out.display().to_string(),
            "--repo-root",
            "/repo",
            "--openssl",
            "/usr/bin/openssl",
            "--generation",
            &self.security_generation,
        ]));
        for (key, path) in &self.console_inputs {
            args.push(key.clone());
            args.push(path.canonicalize()?.display().to_string());
        }
        let receipt = docker(&args, "native secure generation extension")?;
        let release = self.security_selection()?;
        fs::write(release.join("extension-receipt.json"), receipt)?;
        fs::set_permissions(
            release.join("extension-receipt.json"),
            fs::Permissions::from_mode(0o600),
        )?;
        // No trust anchor is supplied here: only the actual host trust store is measured.
        let trust = run(
            "security",
            &strings(&[
                "verify-cert",
                "-c",
                &release.join("frontend/cert.pem").display().to_string(),
                "-p",
                "ssl",
                "-s",
                "localhost",
            ]),
            "browser OS trust",
        );
        fs::write(
            release.join("browser-trust.json"),
            format!(
                "{{\"system_trust_modified\":false,\"os_trust_verified\":{}}}\n",
                trust.is_ok()
            ),
        )?;
        println!(
            "console TLS prepared; OS trust measured: {}; browser profile trust is separate",
            trust.is_ok()
        );
        Ok(())
    }
    fn preflight(&self) -> Result<()> {
        ensure(
            !self.out.join("INCOMPLETE").exists(),
            "incomplete preparation must not start",
        )?;
        ensure(
            text(&self.out.join("PREPARED"), true)?.trim() == "agentforge-local-fresh/v1",
            "prepared generation required",
        )?;
        let e = self.effective_env(&self.out.join("fresh.env"))?;
        ensure(
            needed(&e, "POSTGRES_DB")? == "gateway_dev_fresh"
                && needed(&e, "COMPOSE_PROJECT_NAME")? == PROJECT,
            "prepared database/project identity mismatch",
        )?;
        ensure(
            needed(&e, "LOCAL_FRESH_DATA_INPUT")? == self.data.display().to_string()
                && needed(&e, "DATA_CLIENT_CIDR")? == self.client
                && needed(&e, "DATA_BACKEND_CIDR")? == self.backend
                && needed(&e, "LOCAL_FRESH_TOOLS_IMAGE")? == self.tools,
            "prepared generation selection mismatch",
        )?;
        self.image_admission(&e)?;
        self.call(&["config", "--quiet"], "private Compose configuration")?;
        let images = self.call(&["config", "--images"], "Compose image inventory")?;
        for image in images.lines().filter(|v| !v.is_empty()) {
            docker(
                &strings(&["image", "inspect", "--format", "{{.Id}}", image]),
                "existing infrastructure image admission",
            )?;
        }
        let own = format!("{PROJECT}_data-postgres");
        let client = ipv4_range(&self.client)?;
        let backend = ipv4_range(&self.backend)?;
        ensure(!overlaps(client, backend), "selected data subnets overlap")?;
        let networks = docker(
            &strings(&["network", "ls", "--format", "{{.Name}}"]),
            "network inventory",
        )?;
        for name in networks.lines().filter(|v| !v.is_empty()) {
            let subnets = docker(
                &strings(&[
                    "network",
                    "inspect",
                    "--format",
                    "{{range .IPAM.Config}}{{.Subnet}} {{end}}",
                    name,
                ]),
                "network subnet inspection",
            )?;
            if name == own {
                let project = docker(
                    &strings(&[
                        "network",
                        "inspect",
                        "--format",
                        "{{index .Labels \"com.docker.compose.project\"}}",
                        name,
                    ]),
                    "network ownership",
                )?;
                ensure(
                    project.trim() == PROJECT && subnets.trim() == self.client,
                    "existing data network ownership/CIDR differs; refusing recreation",
                )?;
                continue;
            }
            for subnet in subnets.split_whitespace().filter(|s| !s.contains(':')) {
                let actual = ipv4_range(subnet)?;
                ensure(
                    !overlaps(client, actual) && !overlaps(backend, actual),
                    "selected data subnet overlaps existing Docker network",
                )?;
            }
        }
        memory(
            self.database_only(),
            if self.database_only() {
                256 * 1024 * 1024
            } else {
                512 * 1024 * 1024
            },
        )
    }
    fn start(&self, service: &str, once: bool) -> Result<()> {
        ownership(&self.root)?;
        let e = self.effective_env(&self.out.join("fresh.env"))?;
        memory(
            self.database_only(),
            selected_memory(&self.root, &e, service)?,
        )?;
        if once {
            self.call(
                &["up", "-d", "--no-build", "--pull", "never", service],
                &format!("native one-shot {service}"),
            )?;
            let id = self.call(&["ps", "-a", "-q", service], "one-shot identity")?;
            ensure(
                !id.trim().is_empty() && id.lines().count() == 1,
                "one-shot singleton identity required",
            )?;
            let deadline = Instant::now() + Duration::from_secs(180);
            loop {
                let state = docker(
                    &strings(&[
                        "inspect",
                        "--format",
                        "{{.State.Status}}|{{.State.ExitCode}}",
                        id.trim(),
                    ]),
                    "one-shot state",
                )?;
                if state.trim() == "exited|0" {
                    break;
                }
                ensure(
                    !state.starts_with("exited|") && !state.starts_with("dead|"),
                    &format!("native one-shot {service} failed; private logs withheld"),
                )?;
                ensure(
                    Instant::now() < deadline,
                    "native one-shot deadline exceeded",
                )?;
                thread::sleep(Duration::from_secs(1));
            }
        } else {
            self.call(
                &[
                    "up",
                    "-d",
                    "--no-build",
                    "--pull",
                    "never",
                    "--wait",
                    "--wait-timeout",
                    "120",
                    service,
                ],
                &format!("serial service {service}"),
            )?;
        }
        println!("local fresh ready: {service}");
        Ok(())
    }
    fn http(&self) -> Result<()> {
        let e = self.effective_env(&self.out.join("fresh.env"))?;
        for (key, default, path) in [
            ("GATEWAY_PORT", 8080, "/health/ready"),
            ("KNOWLEDGE_SERVICE_PORT", 8092, "/health/ready"),
            ("AI_PLATFORM_AGENT_RUNTIME_HOST_PORT", 8094, "/health/ready"),
        ] {
            let port = match e.get(key) {
                Some(value) => value
                    .parse::<u16>()
                    .map_err(|_| "selected HTTP port invalid")?,
                None => default,
            };
            ensure(port > 0, "selected HTTP port must be fixed and nonzero")?;
            let url = format!("http://127.0.0.1:{port}{path}");
            let end = Instant::now() + Duration::from_secs(120);
            loop {
                if run(
                    "curl",
                    &strings(&[
                        "--fail",
                        "--silent",
                        "--output",
                        "/dev/null",
                        "--max-time",
                        "5",
                        &url,
                    ]),
                    "HTTP readiness",
                )
                .is_ok()
                {
                    break;
                }
                ensure(
                    Instant::now() < end,
                    "real HTTP readiness failed; bodies/credentials withheld",
                )?;
                thread::sleep(Duration::from_secs(2));
            }
        }
        let origin = needed(&e, "LOCAL_FRESH_CONSOLE_ORIGIN")?;
        ensure(
            origin.starts_with("https://localhost:"),
            "exact local HTTPS console origin required",
        )?;
        let ca = self.security_selection()?.join("frontend/ca.pem");
        text(&ca, false)?;
        run(
            "/usr/bin/curl",
            &strings(&[
                "--cacert",
                &ca.display().to_string(),
                "--fail",
                "--silent",
                "--output",
                "/dev/null",
                "--max-time",
                "10",
                origin,
            ]),
            "console HTTPS chain and hostname verification",
        )?;
        println!(
            "local fresh HTTPS chain/hostname and service readiness passed; browser profile trust/login remain separate acceptance"
        );
        Ok(())
    }
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("local fresh: {error}");
        std::process::exit(1);
    }
}
fn execute() -> Result<()> {
    let options = Options::from_args()?;
    ownership(&options.root)?;
    if options.mode == "prepare" || !options.out.exists() {
        options.prepare()?;
    }
    if options.mode == "secure-extend" {
        options.extend_security()?;
        println!(
            "local security material prepared; browser trust is checked independently; no core service started"
        );
        return Ok(());
    }
    options.preflight()?;
    if options.mode == "prepare" {
        println!("local fresh prepared; no DB/service started");
        return Ok(());
    }
    if options.mode == "database" {
        for service in ["files-postgres", "files-socket", "files-authority"] {
            options.start(service, true)?;
        }
        options.start("postgres", false)?;
        options.start("database-fresh", true)?;
        println!(
            "local fresh full schema installed and role/mTLS verification passed; old database retained"
        );
        return Ok(());
    }
    if options.mode == "up" {
        for service in FILES {
            options.start(service, true)?;
        }
        for service in ["postgres", "redis", "object-store", "qdrant"] {
            options.start(service, false)?;
        }
        options.start("database-fresh", true)?;
        for service in [
            "files-security-gateway",
            "files-security-agent-runtime",
            "files-security-agent-capability-worker",
            "files-security-knowledge-service",
            "files-security-knowledge-worker",
            "files-security-frontend",
            "files-security-administrator",
        ] {
            options.start(service, true)?;
        }
        options.start("admin-bootstrap", true)?;
        options.start("gateway-init", true)?;
        options.start("knowledge-service", false)?;
        options.start("knowledge-worker", false)?;
        if options.sandbox {
            options.start("agent-python-sandbox", false)?;
        }
        for service in [
            "agent-capability-worker",
            "agent-runtime",
            "gateway",
            "frontend",
        ] {
            options.start(service, false)?;
        }
        if options.sdk {
            options.start("files-security-sdk-agent-worker", true)?;
            options.start("sdk-agent-worker", false)?;
        }
    }
    options.http()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_env_never_expands_or_executes() {
        let e = parse("A='$SECRET # literal'\nB=bare\n").unwrap();
        assert_eq!(e["A"], "$SECRET # literal");
        assert_eq!(e["B"], "bare");
        for raw in ["A=one\nA=two", "lower=value", "A='unclosed", "A=bad\0"] {
            assert!(parse(raw).is_err());
        }
    }
    #[test]
    fn bind_paths_refuse_mount_option_injection() {
        assert!(
            mount(
                Path::new("/private/a,readonly=false"),
                Path::new("/input"),
                true
            )
            .is_err()
        );
        assert!(mount(Path::new("relative"), Path::new("/input"), true).is_err());
        assert_eq!(
            mount(Path::new("/private/input"), Path::new("/input"), true).unwrap(),
            "type=bind,src=/private/input,dst=/input,readonly"
        );
    }
    fn options(mode: &str) -> Options {
        Options {
            root: "/repo".into(),
            out: "/external/output".into(),
            input: "/external/source.env".into(),
            data: "/external/data".into(),
            tools: "fixture-tools:local".into(),
            client: "10.71.10.0/24".into(),
            backend: "10.71.11.0/24".into(),
            images: vec![],
            mode: mode.into(),
            sandbox: true,
            sdk: true,
            security_generation: "fixture-r1".into(),
            console_inputs: vec![],
        }
    }
    #[test]
    fn database_phase_has_no_optional_or_runtime_image_dependency() {
        let args = options("database").compose();
        assert!(
            args.iter()
                .any(|v| v == "/repo/deploy/local/compose.core-database.yml")
        );
        assert!(
            !args
                .iter()
                .any(|v| v.contains("sandbox") || v.contains("sdk"))
        );
        assert!(!args.iter().any(|v| v.contains("compose.core.yml")));
    }
    #[test]
    fn explicit_full_options_select_exact_existing_contracts() {
        let args = options("up").compose();
        assert!(
            args.iter()
                .any(|v| v == "/repo/deploy/local/compose.core.yml")
        );
        assert!(
            args.iter()
                .any(|v| v == "/repo/deploy/local/compose.core-sandbox.yml")
        );
        assert!(
            args.iter()
                .any(|v| v == "/repo/deploy/local/compose.core-sdk.yml")
        );
        assert!(args.windows(2).any(|v| v == ["--profile", "sdk-agents"]));
    }
    #[test]
    fn security_extension_is_explicit_atomic_and_does_not_gate_database() {
        let root =
            std::env::temp_dir().join(format!("local-security-selector-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let mut option = options("up");
        option.out = root.clone();
        assert!(option.security_selection().is_err());
        fs::create_dir_all(root.join("security/releases/r1")).unwrap();
        let put = |name: &str, value: &str| {
            fs::write(root.join(name), value).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        };
        put("security/ACTIVE", "../escape\n");
        assert!(option.security_selection().is_err());
        put("security/ACTIVE", "r1\n");
        put("security/releases/r1/PREPARED", "local-security/v1\n");
        assert!(option.security_selection().is_ok());
        put("security/releases/r1/INCOMPLETE", "pending");
        assert!(option.security_selection().is_err());
        fs::remove_file(root.join("security/releases/r1/INCOMPLETE")).unwrap();
        put("fresh.env", "FRONTEND_PORT='8443'\n");
        let selected = format!(
            "LOCAL_FRESH_SECURITY_DIRECTORY='{}'\nLOCAL_FRESH_SECURITY_GENERATION='r1'\nLOCAL_FRESH_NATIVE_OPTIONS_FILE='{}'\nLOCAL_FRESH_RUNTIME_BRIDGE_ENV_FILE='{}'\nLOCAL_FRESH_CONSOLE_ORIGIN='https://localhost:8443'\nLOCAL_FRESH_BFF_ORIGINS='[\"https://localhost:8443\"]'\n",
            root.join("security/releases/r1").display(),
            root.join("security/releases/r1/native-gateway.json")
                .display(),
            root.join("security/application/agent-runtime/bridge.env")
                .display()
        );
        put("security/releases/r1/security.env", &selected);
        assert_eq!(
            option.effective_env(&root.join("fresh.env")).unwrap()["LOCAL_FRESH_CONSOLE_ORIGIN"],
            "https://localhost:8443"
        );
        put(
            "security/releases/r1/security.env",
            &selected.replace("https://localhost:8443", "https://elsewhere.example:8443"),
        );
        assert!(option.effective_env(&root.join("fresh.env")).is_err());
        put(
            "security/releases/r1/security.env",
            &(selected + "UNEXPECTED_SECRET='forbidden'\n"),
        );
        assert!(option.effective_env(&root.join("fresh.env")).is_err());
        option.mode = "database".into();
        assert!(!option.compose().iter().any(|v| v.contains("security.env")));
        option.mode = "secure-extend".into();
        assert!(!option.core());
        assert!(!option.database_only());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn network_overlap_catches_subnet_and_supernet_but_keeps_current_default() {
        let chosen = ipv4_range("10.71.10.0/24").unwrap();
        for candidate in ["10.0.0.0/8", "10.71.10.0/25", "10.71.10.64/26"] {
            assert!(overlaps(chosen, ipv4_range(candidate).unwrap()));
        }
        for candidate in ["172.18.0.0/16", "10.71.11.0/24"] {
            assert!(!overlaps(chosen, ipv4_range(candidate).unwrap()));
        }
        assert!(ipv4_range("10.71.10.0/33").is_err());
    }
    #[test]
    fn capacity_reserves_actual_selected_service_limits() {
        let root = std::env::current_dir().unwrap();
        let mut e = BTreeMap::new();
        assert_eq!(
            selected_memory(&root, &e, "object-store").unwrap(),
            512 * 1024 * 1024
        );
        assert_eq!(
            selected_memory(&root, &e, "database-fresh").unwrap(),
            256 * 1024 * 1024
        );
        e.insert("AGENT_CAPABILITY_WORKER_MEMORY_LIMIT".into(), "1.5g".into());
        assert_eq!(
            selected_memory(&root, &e, "agent-capability-worker").unwrap(),
            1536 * 1024 * 1024
        );
        assert!(selected_memory(&root, &e, "unlisted-service").is_err());
        for invalid in ["unbounded", "-1g", "NaN", "0", "8tb"] {
            assert!(memory_limit(invalid).is_err());
        }
    }
}
