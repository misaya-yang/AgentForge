//! Offline fresh input preparation: no SQL/Docker operation, existing env rewrite,
//! password rotation, provider-key generation or historical migration execution.
use crate::*;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    process::{Command, Stdio},
};
const TARGET: &str = "gateway_dev_fresh";
const PROJECT: &str = "agentforge-dev-7801d4a9a4";
pub(super) fn read(path: &Path, private: bool) -> Result<Vec<u8>> {
    ai_platform_service_primitives::workload_identity::secrets::read_file(path, private)
        .map(|v| v.expose().to_vec())
        .map_err(|_| "private local input unavailable or unsafe".into())
}
pub(super) fn write(path: &Path, value: &[u8], mode: u32) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|_| "new private output required")?;
    f.write_all(value)
        .and_then(|_| f.sync_all())
        .map_err(|_| "private output write failed".into())
}
pub(super) fn directory(path: &Path) -> Result<()> {
    let mut b = fs::DirBuilder::new();
    b.mode(0o700);
    b.create(path)
        .map_err(|_| "new private directory required".into())
}
pub(super) fn required<'a>(e: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    e.get(key)
        .filter(|v| !v.is_empty())
        .map(String::as_str)
        .ok_or_else(|| format!("missing required local input: {key}").into())
}
pub(super) fn env(path: &Path) -> Result<BTreeMap<String, String>> {
    let bytes = read(path, true)?;
    let mut e = BTreeMap::new();
    for pair in dotenvy::from_read_iter(bytes.as_slice()) {
        let (k, v) = pair.map_err(|_| "private env syntax invalid; contents withheld")?;
        require(
            e.insert(k, v).is_none(),
            "duplicate private env key; contents withheld",
        )?;
    }
    Ok(e)
}
pub(super) fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| "OS randomness unavailable")?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}
pub(super) fn openssl(exe: &Path, args: &[&str]) -> Result<()> {
    require(
        exe.is_absolute() && exe.is_file(),
        "explicit OpenSSL executable required",
    )?;
    let status = Command::new(exe)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "OpenSSL unavailable")?;
    require(
        status.success(),
        "native local PKI generation/validation failed",
    )
}
fn dsn(value: &str, database: &str, host: &str) -> Result<String> {
    let mut u = reqwest::Url::parse(value).map_err(|_| "database credential shape invalid")?;
    require(
        matches!(u.scheme(), "postgres" | "postgresql")
            && !u.username().is_empty()
            && u.password().is_some(),
        "database credential identity missing",
    )?;
    u.set_host(Some(host))
        .map_err(|_| "database endpoint invalid")?;
    u.set_port(Some(5432))
        .map_err(|_| "database endpoint invalid")?;
    u.set_path(&format!("/{database}"));
    u.set_query(None);
    u.set_fragment(None);
    Ok(u.into())
}
fn percent_password(value: &str) -> Result<String> {
    let mut bytes = value.bytes();
    let mut out = Vec::new();
    while let Some(c) = bytes.next() {
        if c == b'%' {
            let a = bytes
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or("credential encoding invalid")?;
            let b = bytes
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or("credential encoding invalid")?;
            out.push((a * 16 + b) as u8);
        } else {
            out.push(c);
        }
    }
    String::from_utf8(out).map_err(|_| "credential encoding invalid".into())
}
fn quote(value: &str) -> Result<String> {
    require(
        !value.contains(['\n', '\r', '\0', '\'']),
        "private env value requires unsupported multiline/quote representation",
    )?;
    Ok(format!("'{value}'"))
}
fn qdrant(e: &mut BTreeMap<String, String>, out: &Path, exe: &Path) -> Result<()> {
    let keys = ["QDRANT_API_KEY", "QDRANT_READ_ONLY_API_KEY"];
    for key in keys {
        if e.get(key).is_none_or(String::is_empty) {
            e.insert(key.into(), random()?);
        }
    }
    require(
        e[keys[0]] != e[keys[1]],
        "Qdrant writer/read-only keys must differ",
    )?;
    let paths = [
        "QDRANT_TLS_CERT_FILE",
        "QDRANT_TLS_KEY_FILE",
        "QDRANT_CA_BUNDLE_FILE",
    ];
    let configured = paths
        .iter()
        .filter(|k| e.get(**k).is_some_and(|v| !v.is_empty()))
        .count();
    require(
        configured == 0 || configured == 3,
        "partial existing Qdrant TLS input; no replacement generated",
    )?;
    let q = out.join("qdrant");
    directory(&q)?;
    if configured == 3 {
        for (key, name, private) in [
            (paths[0], "tls.crt", false),
            (paths[1], "tls.key", true),
            (paths[2], "ca-bundle.pem", false),
        ] {
            write(
                &q.join(name),
                &read(Path::new(&e[key]), private)?,
                if private { 0o600 } else { 0o644 },
            )?;
        }
    } else {
        let p = |n: &str| q.join(n).to_string_lossy().into_owned();
        openssl(
            exe,
            &[
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-out",
                &p("ca.key"),
            ],
        )?;
        openssl(
            exe,
            &[
                "req",
                "-x509",
                "-new",
                "-key",
                &p("ca.key"),
                "-sha256",
                "-days",
                "30",
                "-subj",
                "/CN=AgentForge local Qdrant CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
                "-out",
                &p("ca-bundle.pem"),
            ],
        )?;
        openssl(
            exe,
            &[
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-out",
                &p("tls.key"),
            ],
        )?;
        openssl(
            exe,
            &[
                "req",
                "-new",
                "-key",
                &p("tls.key"),
                "-subj",
                "/CN=qdrant",
                "-out",
                &p("tls.csr"),
            ],
        )?;
        write(&q.join("server.ext"),b"basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:qdrant,IP:127.0.0.1\n",0o600)?;
        openssl(
            exe,
            &[
                "x509",
                "-req",
                "-in",
                &p("tls.csr"),
                "-CA",
                &p("ca-bundle.pem"),
                "-CAkey",
                &p("ca.key"),
                "-CAcreateserial",
                "-days",
                "7",
                "-sha256",
                "-extfile",
                &p("server.ext"),
                "-out",
                &p("tls.crt"),
            ],
        )?;
    }
    for name in ["tls.key", "ca.key"] {
        let p = q.join(name);
        if p.exists() {
            fs::set_permissions(p, fs::Permissions::from_mode(0o600))?;
        }
    }
    for name in ["tls.crt", "ca-bundle.pem"] {
        fs::set_permissions(q.join(name), fs::Permissions::from_mode(0o644))?;
    }
    openssl(
        exe,
        &[
            "verify",
            "-purpose",
            "sslserver",
            "-verify_hostname",
            "qdrant",
            "-CAfile",
            q.join("ca-bundle.pem").to_str().unwrap(),
            q.join("tls.crt").to_str().unwrap(),
        ],
    )?;
    let public = |args: &[&str]| -> Result<Vec<u8>> {
        let output = Command::new(exe)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| "OpenSSL TLS public key validation unavailable")?;
        require(
            output.status.success(),
            "OpenSSL TLS public key validation failed",
        )?;
        Ok(output.stdout)
    };
    let cert = q.join("tls.crt");
    let key = q.join("tls.key");
    require(
        public(&["x509", "-in", &cert.to_string_lossy(), "-pubkey", "-noout"])?
            == public(&["pkey", "-in", &key.to_string_lossy(), "-pubout"])?,
        "Qdrant TLS certificate/private key mismatch",
    )?;
    for (key, name) in [
        (paths[0], "tls.crt"),
        (paths[1], "tls.key"),
        (paths[2], "ca-bundle.pem"),
    ] {
        e.insert(key.into(), q.join(name).to_string_lossy().into());
    }
    e.insert(
        "LOCAL_FRESH_QDRANT_DIRECTORY".into(),
        q.to_string_lossy().into(),
    );
    Ok(())
}
pub fn prepare(args: &Args) -> Result<Value> {
    let input = PathBuf::from(args.get("--env-file")?);
    let out = PathBuf::from(args.get("--directory")?);
    let root = args
        .root()
        .canonicalize()
        .map_err(|_| "public source root unavailable")?;
    require(
        out.is_absolute()
            && out.is_dir()
            && !out.starts_with(&root)
            && !out.join("fresh.env").exists(),
        "existing new external private preparation directory required",
    )?;
    require(
        fs::symlink_metadata(&out)?.is_dir()
            && fs::metadata(&out)?.permissions().mode() & 0o077 == 0,
        "private output directory required",
    )?;
    super::local_fresh_contract::validate(&root)?;
    let target = args.get("--database")?;
    require(
        target == TARGET,
        "explicit gateway_dev_fresh target required",
    )?;
    let mut e = env(&input)?;
    require(
        required(&e, "COMPOSE_PROJECT_NAME")? == PROJECT,
        "existing owned project required",
    )?;
    if let Some(paths) = args.multiple.get("--image-env") {
        for path in paths {
            let bytes = read(Path::new(path), false)?;
            for pair in dotenvy::from_read_iter(bytes.as_slice()) {
                let (key, value) = pair.map_err(|_| "image receipt syntax invalid")?;
                require(
                    [
                        "ADMIN_BOOTSTRAP_IMAGE",
                        "GATEWAY_IMAGE",
                        "KNOWLEDGE_IMAGE",
                        "FRONTEND_IMAGE",
                        "MIGRATE_IMAGE",
                        "AI_PLATFORM_AGENT_RUNTIME_IMAGE",
                        "AGENT_CAPABILITY_WORKER_IMAGE",
                        "AGENT_PYTHON_SANDBOX_IMAGE",
                        "SDK_AGENT_WORKER_IMAGE",
                        "LOCAL_FRESH_TOOLS_IMAGE",
                    ]
                    .contains(&key.as_str()),
                    "image receipt contains a non-image input",
                )?;
                require(
                    !value.is_empty() && !value.contains(char::is_whitespace),
                    "image receipt identity invalid",
                )?;
                e.insert(key, value);
            }
        }
    }
    require(
        required(&e, "POSTGRES_USER")? == "postgres",
        "fresh entry requires the existing UID70 postgres Unix peer operator",
    )?;
    let source = required(&e, "POSTGRES_DB")?.to_owned();
    require(
        source != target,
        "fresh target must differ from source database",
    )?;
    require(
        e.get("AI_PLATFORM_DEPLOYMENT_MODE")
            .is_none_or(|v| v == "single-instance"),
        "fresh core retains singleton admission; select the separately gated HA profile explicitly",
    )?;
    require(
        e.get("AI_PLATFORM_HA_ENABLED")
            .is_none_or(|v| v == "false" || v.is_empty()),
        "fresh core cannot bypass pending HA chaos admission",
    )?;
    let exe = PathBuf::from(args.get("--openssl")?);
    let data = PathBuf::from(args.get("--data-input")?);
    ai_platform_tools::data_security::local_env::validate(&data)
        .map_err(|_| "existing data input validation failed; contents withheld")?;
    let deployment: Value =
        serde_json::from_slice(&read(&data.join("deployment-paths.json"), true)?)?;
    require(
        deployment["POSTGRES_DB"] == target,
        "data input must target gateway_dev_fresh",
    )?;
    let receipt: Value = serde_json::from_slice(&read(&data.join("import-receipt.json"), true)?)?;
    require(
        receipt["source_database"] == source,
        "existing data source identity mismatch",
    )?;
    for role in [
        "MIGRATOR",
        "GATEWAY",
        "RUNTIME",
        "CAPABILITY_WORKER",
        "KNOWLEDGE_API",
        "KNOWLEDGE_WORKER",
    ] {
        let u = reqwest::Url::parse(required(&e, &format!("AI_GATEWAY_DATABASE_{role}_DSN"))?)
            .map_err(|_| "source role credential invalid")?;
        let input_password = read(
            &data
                .join("authority")
                .join(role.to_ascii_lowercase())
                .join("password"),
            true,
        )?;
        let decoded = percent_password(u.password().ok_or("source password required")?)?;
        require(
            decoded.as_bytes() == input_password.as_slice(),
            "imported credential differs from selected source",
        )?;
    }
    e.insert(
        "LOCAL_FRESH_DATA_INPUT".into(),
        data.to_string_lossy().into(),
    );
    e.insert("DATA_CLIENT_CIDR".into(), args.get("--client-cidr")?.into());
    e.insert(
        "DATA_BACKEND_CIDR".into(),
        args.get("--backend-cidr")?.into(),
    );
    let hba = String::from_utf8(read(&data.join("material/pg_hba.conf"), true)?)?;
    for key in ["DATA_CLIENT_CIDR", "DATA_BACKEND_CIDR"] {
        require(
            hba.split_whitespace().any(|v| v == e[key]),
            "data network differs from imported HBA",
        )?;
    }
    let source_admin = dsn(
        required(&e, "AI_GATEWAY_DATABASE_ADMIN_DSN")?,
        &source,
        "postgres",
    )?;
    e.insert("LOCAL_CONTAINER_SOURCE_ADMIN_DSN".into(), source_admin);
    for role in [
        "MIGRATOR",
        "GATEWAY",
        "RUNTIME",
        "CAPABILITY_WORKER",
        "KNOWLEDGE_API",
        "KNOWLEDGE_WORKER",
    ] {
        let key = format!("AI_GATEWAY_DATABASE_{role}_DSN");
        let value = dsn(required(&e, &key)?, target, "postgres")?;
        e.insert(format!("LOCAL_CONTAINER_{role}_DSN"), value);
    }
    e.insert("LOCAL_FRESH_SOURCE_DATABASE".into(), source);
    e.insert("POSTGRES_DB".into(), target.into());
    let redis = required(&e, "REDIS_PASSWORD")?;
    let mut redis_url =
        reqwest::Url::parse("redis://redis:6379/13").map_err(|_| "Redis endpoint invalid")?;
    redis_url
        .set_password(Some(redis))
        .map_err(|_| "Redis credential invalid")?;
    e.insert("INTERNAL_COMM_REDIS_URL".into(), redis_url.into());

    e.insert(
        "AGENTFORGE_REPOSITORY_ROOT".into(),
        args.get("--host-root")?.into(),
    );
    let phase = args.get("--phase")?;
    require(
        matches!(phase, "database" | "core"),
        "explicit preparation phase required",
    )?;
    let image_keys: &[&str] = if phase == "database" {
        &["MIGRATE_IMAGE"]
    } else {
        &[
            "ADMIN_BOOTSTRAP_IMAGE",
            "GATEWAY_IMAGE",
            "KNOWLEDGE_IMAGE",
            "FRONTEND_IMAGE",
            "MIGRATE_IMAGE",
            "AI_PLATFORM_AGENT_RUNTIME_IMAGE",
            "AGENT_CAPABILITY_WORKER_IMAGE",
        ]
    };
    for key in image_keys {
        required(&e, key)?;
    }
    e.insert(
        "LOCAL_FRESH_TOOLS_IMAGE".into(),
        args.get("--tools-image")?.into(),
    );
    qdrant(&mut e, &out, &exe)?;
    super::local_fresh_files::prepare_model_keys(&mut e, &out)?;
    let revision = required(&e, "AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION")?.to_owned();
    require(
        !revision.contains('<'),
        "reviewed Runtime kernel revision required",
    )?;
    let mut native: Value = serde_json::from_slice(
        &fs::read(root.join("deploy/platform-native/native-options.example.json"))
            .map_err(|_| "public native options unavailable")?,
    )
    .map_err(|_| "public native options invalid")?;
    native["kernel_revision"] = json!(revision);
    native["worker_writes_enabled"] = json!(
        e.get("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED")
            .map(String::as_str)
            != Some("false")
    );
    write(
        &out.join("native-gateway.json"),
        &serde_json::to_vec_pretty(&native)?,
        0o644,
    )?;
    e.insert(
        "LOCAL_CONTAINER_GATEWAY_NATIVE_OPTIONS_FILE".into(),
        out.join("native-gateway.json").to_string_lossy().into(),
    );
    let mut text = String::new();
    for (k, v) in &e {
        text.push_str(&format!("{k}={}\n", quote(v)?));
    }
    write(&out.join("fresh.env"), text.as_bytes(), 0o600)?;
    write(&out.join("PREPARED"), b"agentforge-local-fresh/v1\n", 0o600)?;
    Ok(
        json!({"schema":"agentforge-local-fresh/v1","status":"prepared-not-started","target_database":TARGET,"existing_project":PROJECT,"source_env_unchanged":true,"database_passwords_rotated":false,"database_contacted":false,"qdrant_writer_reader_distinct":true,"provider_values_withheld":true}),
    )
}
pub fn qdrant_files(args: &Args) -> Result<Value> {
    let source = PathBuf::from(args.get("--input")?);
    let out = PathBuf::from(args.get("--directory")?);
    require(
        source == Path::new("/input") && out == Path::new("/output"),
        "fixed private Qdrant volume paths required",
    )?;
    for name in ["tls.crt", "tls.key", "ca-bundle.pem"] {
        let bytes = read(&source.join(name), name == "tls.key")?;
        let p = out.join(name);
        if p.exists() {
            require(
                read(&p, name == "tls.key")? == bytes,
                "existing private TLS volume differs; explicit rotation required",
            )?;
        } else {
            write(&p, &bytes, if name == "tls.key" { 0o600 } else { 0o644 })?;
        }
        let path = std::ffi::CString::new(p.as_os_str().as_encoded_bytes())?;
        require(
            unsafe { libc::chown(path.as_ptr(), 1000, 1000) } == 0,
            "private TLS ownership install failed",
        )?;
    }
    let path = std::ffi::CString::new(out.as_os_str().as_encoded_bytes())?;
    require(
        unsafe { libc::chown(path.as_ptr(), 1000, 1000) } == 0,
        "private TLS directory install failed",
    )?;
    fs::set_permissions(&out, fs::Permissions::from_mode(0o700))?;
    Ok(json!({"status":"installed","private_key_owner":1000,"old_qdrant_storage_changed":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn database_retarget_keeps_role_secret_and_isolates_only_target() {
        let source = "postgresql://ai_gateway_gateway:p%40ss%2Fword@127.0.0.1:5441/original";
        let target = reqwest::Url::parse(&dsn(source, TARGET, "postgres").unwrap()).unwrap();
        assert_eq!(target.username(), "ai_gateway_gateway");
        assert_eq!(target.password(), Some("p%40ss%2Fword"));
        assert_eq!(target.host_str(), Some("postgres"));
        assert_eq!(target.port(), Some(5432));
        assert_eq!(target.path(), "/gateway_dev_fresh");
        assert_eq!(
            percent_password(target.password().unwrap()).unwrap(),
            "p@ss/word"
        );
        assert!(dsn("postgresql://postgres@localhost/old", TARGET, "postgres").is_err());
    }
    #[test]
    fn private_env_values_are_literal_and_multiline_is_refused() {
        assert_eq!(quote("$NAME # literal").unwrap(), "'$NAME # literal'");
        for value in ["line\nnext", "a'b", "\0", "\r"] {
            assert!(quote(value).is_err());
        }
        assert!(percent_password("%xx").is_err());
        assert!(percent_password("%").is_err());
    }
}
