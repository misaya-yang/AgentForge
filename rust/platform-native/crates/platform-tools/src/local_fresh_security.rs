//! Explicit augmentation of an existing fresh private generation. Separate
//! workload/browser CAs; no database/password mutation or trust-store changes.
use super::local_fresh::{directory, env, openssl, random, read, required, write};
use crate::*;
use ai_platform_service_primitives::workload_identity::{
    ServiceIdentity, secrets::CredentialFiles, tls::ClientCredentials,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};

const ROLES: [(&str, &str, u32); 6] = [
    ("gateway", "gateway", 1000),
    ("agent-runtime", "agent-runtime", 10001),
    ("agent-capability-worker", "agent-capability-worker", 10001),
    ("knowledge-service", "knowledge", 1000),
    ("knowledge-worker", "knowledge-worker", 1000),
    ("sdk-agent-worker", "sdk", 10001),
];
fn generation(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
        "explicit safe security generation required",
    )
}
fn issue_ca(exe: &Path, out: &Path, console: bool) -> Result<()> {
    if out.exists() {
        read(&out.join("ca.key"), true)?;
        read(&out.join("ca.pem"), false)?;
        return Ok(());
    }
    directory(out)?;
    let mut profile = String::from(
        "[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=AgentForge ",
    );
    profile.push_str(if console {
        "localhost browser CA\n"
    } else {
        "private workload CA\n"
    });
    profile.push_str("[ca]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n");
    if console {
        profile.push_str("nameConstraints=critical,permitted;DNS:localhost,permitted;IP:127.0.0.1/255.255.255.255,permitted;IP:0:0:0:0:0:0:0:1/ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n");
    }
    write(&out.join("ca.cnf"), profile.as_bytes(), 0o600)?;
    openssl(
        exe,
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            out.join("ca.key").to_str().ok_or("issuer path invalid")?,
            "-out",
            out.join("ca.pem").to_str().ok_or("issuer path invalid")?,
            "-days",
            "365",
            "-config",
            out.join("ca.cnf").to_str().ok_or("issuer path invalid")?,
        ],
    )?;
    fs::set_permissions(out.join("ca.key"), fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(out.join("ca.pem"), fs::Permissions::from_mode(0o644))?;
    Ok(())
}
fn public(exe: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let o = Command::new(exe)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "public certificate inspection unavailable")?;
    require(
        o.status.success() && o.stdout.len() < 128 * 1024,
        "public certificate inspection failed",
    )?;
    Ok(o.stdout)
}
fn check_leaf(exe: &Path, dir: &Path, dns: &str) -> Result<()> {
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();
    openssl(
        exe,
        &[
            "verify",
            "-purpose",
            "sslserver",
            "-verify_hostname",
            dns,
            "-CAfile",
            &p("ca.pem"),
            &p("cert.pem"),
        ],
    )?;
    openssl(
        exe,
        &[
            "x509",
            "-in",
            &p("cert.pem"),
            "-noout",
            "-checkend",
            "86400",
        ],
    )?;
    require(
        public(exe, &["x509", "-in", &p("cert.pem"), "-pubkey", "-noout"])?
            == public(exe, &["pkey", "-in", &p("key.pem"), "-pubout"])?,
        "certificate/key mismatch",
    )?;
    Ok(())
}
fn leaf(exe: &Path, ca: &Path, out: &Path, dns: &str, id: Option<&str>) -> Result<()> {
    directory(out)?;
    let mut ext = String::from(
        "[leaf]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth",
    );
    if id.is_some() {
        ext.push_str(",clientAuth");
    }
    ext.push_str(&format!("\nsubjectAltName=DNS:{dns}"));
    if let Some(id) = id {
        ServiceIdentity::parse(id).map_err(|_| "workload identity invalid")?;
        ext.push_str(&format!(",URI:{id}"));
    } else {
        ext.push_str(",IP:127.0.0.1,IP:0:0:0:0:0:0:0:1");
    }
    ext.push('\n');
    write(&out.join("leaf.cnf"), ext.as_bytes(), 0o600)?;
    let p = |n: &str| out.join(n).to_string_lossy().into_owned();
    openssl(
        exe,
        &[
            "req",
            "-new",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            &p("key.pem"),
            "-out",
            &p("leaf.csr"),
            "-subj",
            "/CN=AgentForge local service",
        ],
    )?;
    fs::set_permissions(out.join("key.pem"), fs::Permissions::from_mode(0o600))?;
    openssl(
        exe,
        &[
            "x509",
            "-req",
            "-in",
            &p("leaf.csr"),
            "-CA",
            &ca.join("ca.pem").to_string_lossy(),
            "-CAkey",
            &ca.join("ca.key").to_string_lossy(),
            "-set_serial",
            &format!("0x{}", &random()?[..32]),
            "-days",
            if id.is_some() { "7" } else { "30" },
            "-out",
            &p("cert.pem"),
            "-extfile",
            &p("leaf.cnf"),
            "-extensions",
            "leaf",
        ],
    )?;
    write(
        &out.join("ca.pem"),
        &read(&ca.join("ca.pem"), false)?,
        0o644,
    )?;
    let mut combined = read(&out.join("cert.pem"), false)?;
    combined.extend(read(&out.join("key.pem"), true)?);
    write(&out.join("identity.pem"), &combined, 0o600)?;
    combined.fill(0);
    check_leaf(exe, out, dns)
}
fn reuse_or_write(path: &Path, value: &[u8]) -> Result<()> {
    if path.exists() {
        require(
            read(path, true)? == value,
            "existing application secret differs; explicit rotation required",
        )
    } else {
        write(path, value, 0o600)
    }
}
fn application_keys(exe: &Path, base: &Path, e: &BTreeMap<String, String>) -> Result<()> {
    if !base.exists() {
        directory(base)?;
    }
    for role in [
        "gateway",
        "knowledge-service",
        "knowledge-worker",
        "agent-runtime",
    ] {
        if !base.join(role).exists() {
            directory(&base.join(role))?;
        }
    }
    let gateway = base.join("gateway");
    reuse_or_write(
        &gateway.join("jwt-secret"),
        required(e, "JWT_SECRET")?.as_bytes(),
    )?;
    let encryption = required(e, "GATEWAY_ENCRYPTION_KEY")?;
    reuse_or_write(&gateway.join("encryption-key"), encryption.as_bytes())?;
    let domain = if gateway.join("internal-auth-keys").exists() {
        String::from_utf8(read(&gateway.join("internal-auth-keys"), true)?)
            .map_err(|_| "domain key encoding invalid")?
    } else if let Some(value) = e.get("INTERNAL_AUTH_KEYS").filter(|s| !s.is_empty()) {
        value.clone()
    } else {
        let id = e
            .get("INTERNAL_AUTH_ACTIVE_KEY_ID")
            .filter(|v| !v.is_empty())
            .map(String::as_str)
            .unwrap_or("local");
        require(
            id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "domain signing key id invalid",
        )?;
        format!("{id}:{}", random()?)
    };
    require(
        domain.split(',').all(|part| {
            part.split_once(':')
                .is_some_and(|(id, key)| !id.is_empty() && key.len() >= 16)
        }),
        "domain signing key map invalid",
    )?;
    if let Some(existing) = e.get("INTERNAL_AUTH_KEYS").filter(|s| !s.is_empty()) {
        require(
            existing == &domain,
            "existing domain keys differ; no automatic rotation",
        )?;
    }
    for role in ["gateway", "knowledge-service", "knowledge-worker"] {
        reuse_or_write(
            &base.join(role).join("internal-auth-keys"),
            domain.as_bytes(),
        )?;
    }
    for role in ["knowledge-service", "knowledge-worker"] {
        reuse_or_write(
            &base.join(role).join("storage-signing-key"),
            encryption.as_bytes(),
        )?;
    }
    let bridge = base.join("agent-runtime/bridge.env");
    if !bridge.exists() {
        write(
            &bridge,
            format!("AI_PLATFORM_RUNTIME_MODEL_PROXY_TOKEN='{}'\n", random()?).as_bytes(),
            0o600,
        )?;
    } else {
        let v = read(&bridge, true)?;
        let value = v
            .strip_prefix(b"AI_PLATFORM_RUNTIME_MODEL_PROXY_TOKEN='")
            .and_then(|v| v.strip_suffix(b"'\n"));
        require(
            value.is_some_and(|v| v.len() == 64 && v.iter().all(u8::is_ascii_hexdigit)),
            "existing Runtime bridge must contain only its genuine local token",
        )?;
    }
    require(
        e.get("AI_PLATFORM_API_EDDSA_CONFIG_FILE")
            .is_none_or(String::is_empty),
        "existing API signing generation requires explicit reuse adapter",
    )?;
    let private = gateway.join("api-private.pem");
    if !private.exists() {
        openssl(
            exe,
            &[
                "genpkey",
                "-algorithm",
                "ED25519",
                "-out",
                &private.to_string_lossy(),
            ],
        )?;
        fs::set_permissions(&private, fs::Permissions::from_mode(0o600))?;
    } else {
        read(&private, true)?;
    }
    let public_pem = public(exe, &["pkey", "-in", &private.to_string_lossy(), "-pubout"])?;
    reuse_or_write(&gateway.join("api-public.pem"), &public_pem)?;
    let config = gateway.join("api-config.json");
    if !config.exists() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        write(
            &config,
            &serde_json::to_vec(&json!({"active_kid":"local-api-access-r1",
            "private_key_file":"/run/security/application/api-private.pem",
            "public_keys":[{"kid":"local-api-access-r1","public_key_file":"/run/security/application/api-public.pem",
                "not_before":now.saturating_sub(60),"not_after":now+365*86_400}]}))?,
            0o600,
        )?;
    } else {
        read(&config, true)?;
    }
    let administrator = base.join("administrator");
    if !administrator.exists() {
        directory(&administrator)?;
    }
    let input = administrator.join("input.json");
    if !input.exists() {
        let domain = required(e, "AUTH_ALLOWED_EMAIL_DOMAIN")?.to_ascii_lowercase();
        require(
            domain.split('.').all(|part| {
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }),
            "selected administrator email domain invalid",
        )?;
        let email = [
            "BOOTSTRAP_ADMIN_EMAIL",
            "ADMIN_EMAIL",
            "GATEWAY_AUTHENTICATION__BOOTSTRAP_ADMIN__EMAIL",
        ]
        .into_iter()
        .find_map(|key| e.get(key).filter(|v| !v.is_empty()).cloned())
        .unwrap_or(format!("admin@{domain}"));
        require(
            email.ends_with(&format!("@{domain}")) && !email.chars().any(char::is_whitespace),
            "selected administrator email must match configured domain",
        )?;
        let password = ["BOOTSTRAP_ADMIN_PASSWORD", "ADMIN_PASSWORD"]
            .into_iter()
            .find_map(|key| e.get(key).filter(|v| !v.is_empty()).cloned())
            .unwrap_or(format!("Af!9{}", random()?));
        write(
            &input,
            &serde_json::to_vec(&json!({"email":email,"password":password}))?,
            0o600,
        )?;
    } else {
        read(&input, true)?;
    }
    if !base.join("PREPARED").exists() {
        write(
            &base.join("PREPARED"),
            b"application-secret-files/v1\n",
            0o600,
        )?;
    }
    Ok(())
}

fn app_files(role: &str) -> &'static [&'static str] {
    match role {
        "gateway" => &[
            "jwt-secret",
            "encryption-key",
            "internal-auth-keys",
            "api-private.pem",
            "api-public.pem",
            "api-config.json",
        ],
        "knowledge-service" | "knowledge-worker" => &["internal-auth-keys", "storage-signing-key"],
        _ => &[],
    }
}
pub fn extend(args: &Args) -> Result<Value> {
    let root = PathBuf::from(args.get("--directory")?);
    require(
        read(&root.join("PREPARED"), true)? == b"agentforge-local-fresh/v1\n",
        "existing prepared generation required",
    )?;
    let e = env(&root.join("fresh.env"))?;
    let mut native: Value =
        serde_json::from_slice(&read(&root.join("native-gateway.json"), false)?)
            .map_err(|_| "original public native options invalid")?;
    require(native.is_object(), "native options object required")?;
    native["allow_private_knowledge_http"] = json!(false);
    let name = args.get("--generation")?;
    generation(name)?;
    let exe = PathBuf::from(args.get("--openssl")?);
    let security = root.join("security");
    if !security.exists() {
        directory(&security)?;
    }
    let releases = security.join("releases");
    if !releases.exists() {
        directory(&releases)?;
    }
    let release = releases.join(name);
    require(
        !release.exists(),
        "new explicit security generation required; existing credentials never regenerated",
    )?;
    directory(&release)?;
    write(&release.join("INCOMPLETE"), b"pending\n", 0o600)?;
    write(
        &release.join("native-gateway.json"),
        &serde_json::to_vec_pretty(&native)?,
        0o644,
    )?;
    let issuer = security.join("workload-issuer");
    issue_ca(&exe, &issuer, false)?;
    let browser = security.join("browser-issuer");
    let supplied = [
        "--console-cert-file",
        "--console-key-file",
        "--console-ca-file",
    ];
    let count = supplied
        .iter()
        .filter(|k| args.values.contains_key(**k))
        .count();
    require(
        count == 0 || count == 3,
        "complete existing console TLS inputs required",
    )?;
    if count == 0 {
        issue_ca(&exe, &browser, true)?;
    }
    let keys = security.join("application");
    application_keys(&exe, &keys, &e)?;
    require(
        keys.join("PREPARED").is_file(),
        "incomplete application material; no automatic reissue",
    )?;
    for (role, identity, _) in ROLES {
        let dir = release.join(role);
        directory(&dir)?;
        let work = dir.join("workload");
        directory(&work)?;
        let id = format!("spiffe://agentforge.internal/service/{identity}");
        leaf(
            &exe,
            &issuer,
            &work.join(name),
            &format!("{role}-workload"),
            Some(&id),
        )?;
        std::os::unix::fs::symlink(name, work.join("current"))?;
        // Reuse the canonical native TLS verifier, including exact URI identity.
        ClientCredentials::new(
            CredentialFiles::new(work.clone()).map_err(|_| "workload layout invalid")?,
            ServiceIdentity::parse(&id).map_err(|_| "workload identity invalid")?,
        )
        .load(ServiceIdentity::parse(&id).map_err(|_| "workload identity invalid")?)
        .map_err(|_| "issued workload credentials rejected")?;
        if !app_files(role).is_empty() {
            let app = dir.join("application");
            directory(&app)?;
            for file in app_files(role) {
                write(
                    &app.join(file),
                    &read(&keys.join(role).join(file), true)?,
                    0o600,
                )?;
            }
        }
    }
    let administrator = release.join("administrator");
    directory(&administrator)?;
    write(
        &administrator.join("input.json"),
        &read(&keys.join("administrator/input.json"), true)?,
        0o600,
    )?;
    let frontend = release.join("frontend");
    directory(&frontend)?;
    if count == 3 {
        for (arg, file, private) in [
            (supplied[0], "cert.pem", false),
            (supplied[1], "key.pem", true),
            (supplied[2], "ca.pem", false),
        ] {
            write(
                &frontend.join(file),
                &read(Path::new(args.get(arg)?), private)?,
                if private { 0o600 } else { 0o644 },
            )?;
        }
        check_leaf(&exe, &frontend, "localhost")?;
    } else {
        leaf(&exe, &browser, &frontend.join("tls"), "localhost", None)?;
        for file in ["cert.pem", "key.pem", "ca.pem"] {
            write(
                &frontend.join(file),
                &read(&frontend.join("tls").join(file), file == "key.pem")?,
                if file == "key.pem" { 0o600 } else { 0o644 },
            )?;
        }
    }
    let repo = args.root();
    let source = fs::read_to_string(repo.join("web/nginx.conf"))
        .map_err(|_| "current frontend nginx source unavailable")?;
    let config = nginx_https(&source)?;
    write(&frontend.join("nginx.conf"), config.as_bytes(), 0o600)?;
    let port = e
        .get("FRONTEND_PORT")
        .map(String::as_str)
        .unwrap_or("8081")
        .parse::<u16>()
        .map_err(|_| "fixed console HTTPS port invalid")?;
    require(port > 0, "fixed console HTTPS port required")?;
    let origin = format!("https://localhost:{port}");
    let mut vars = String::new();
    for (key, value) in [
        (
            "LOCAL_FRESH_SECURITY_DIRECTORY",
            release.to_string_lossy().into_owned(),
        ),
        ("LOCAL_FRESH_SECURITY_GENERATION", name.to_owned()),
        (
            "LOCAL_FRESH_NATIVE_OPTIONS_FILE",
            release
                .join("native-gateway.json")
                .to_string_lossy()
                .into_owned(),
        ),
        ("LOCAL_FRESH_CONSOLE_ORIGIN", origin.clone()),
        (
            "LOCAL_FRESH_RUNTIME_BRIDGE_ENV_FILE",
            keys.join("agent-runtime/bridge.env")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "LOCAL_FRESH_BFF_ORIGINS",
            serde_json::to_string(&vec![origin.clone()])?,
        ),
    ] {
        require(
            !value.contains(['\'', '\n', '\r']),
            "security selection path invalid",
        )?;
        vars.push_str(&format!("{key}='{value}'\n"));
    }
    let ca_der = public(
        &exe,
        &[
            "x509",
            "-in",
            &frontend.join("ca.pem").to_string_lossy(),
            "-outform",
            "DER",
        ],
    )?;
    let leaf_der = public(
        &exe,
        &[
            "x509",
            "-in",
            &frontend.join("cert.pem").to_string_lossy(),
            "-outform",
            "DER",
        ],
    )?;
    write(&release.join("security.env"), vars.as_bytes(), 0o600)?;
    fs::remove_file(release.join("INCOMPLETE"))?;
    write(&release.join("PREPARED"), b"local-security/v1\n", 0o600)?;
    let temporary = security.join(format!(".ACTIVE-{}", random()?));
    write(&temporary, format!("{name}\n").as_bytes(), 0o600)?;
    fs::rename(&temporary, security.join("ACTIVE"))?;
    Ok(
        json!({"status":"security-prepared-not-trusted-or-started","generation":name,
        "origin":origin,"workload_services":5,"optional_sdk_identity_prepared":true,"transport_mode":"required",
        "source_env_rewritten":false,"database_passwords_rotated":false,
        "system_trust_modified":false,"browser_trust_verified":false,
        "console_ca_sha256":hash(&ca_der),"console_leaf_sha256":hash(&leaf_der),
        "console_ca_public_file":frontend.join("ca.pem")}),
    )
}
pub(super) fn nginx_https(source: &str) -> Result<String> {
    require(
        source.matches("listen 8080;").count() == 1,
        "frontend listener source contract changed",
    )?;
    let mut result = source.replacen(
        "listen 8080;",
        r#"listen 8443 ssl;
        ssl_certificate /run/security/current/cert.pem;
        ssl_certificate_key /run/security/current/key.pem;
        ssl_protocols TLSv1.2 TLSv1.3;
        ssl_session_tickets off;
        if ($host != localhost) { return 421; }"#,
        1,
    );
    result = result.replace(
        "proxy_set_header Host $host;",
        "proxy_set_header Host $http_host;",
    );
    let index = result.rfind('}').ok_or("frontend HTTP context missing")?;
    result.insert_str(index,r#"
    # Independent plaintext container health only. No app/API/login on HTTP.
    server {
        listen 8080;
        server_name _;
        location = /health { access_log off; return 200 '{"status":"healthy"}'; add_header Content-Type application/json; }
        location / { return 404; }
    }
"#);
    Ok(result)
}
fn chown(path: &Path, uid: u32) -> Result<()> {
    let p = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    require(
        unsafe { libc::chown(p.as_ptr(), uid, uid) } == 0,
        "security ownership install failed",
    )
}
fn owned_dir(path: &Path, uid: u32) -> Result<()> {
    if !path.exists() {
        directory(path)?;
    }
    require(
        fs::symlink_metadata(path)?.is_dir()
            && !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "regular security directory required",
    )?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    chown(path, uid)
}
fn install_file(source: &Path, target: &Path, uid: u32) -> Result<()> {
    let private = source
        .file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|file| {
            [
                "key.pem",
                "identity.pem",
                "jwt-secret",
                "encryption-key",
                "internal-auth-keys",
                "storage-signing-key",
                "api-private.pem",
                "api-config.json",
                "input.json",
            ]
            .contains(&file)
        });
    reuse_or_write(target, &read(source, private)?)?;
    fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
    chown(target, uid)
}
fn select_generation(root: &Path, name: &str) -> Result<()> {
    generation(name)?;
    // A killed PID1 installer may leave its publication temporary behind. Each
    // attempt uses a fresh name; it never deletes another generation/temporary.
    let next = root.join(format!(".current-{}", random()?));
    std::os::unix::fs::symlink(name, &next)?;
    fs::rename(next, root.join("current"))?;
    Ok(())
}
pub fn install(args: &Args) -> Result<Value> {
    require(
        args.get("--input")? == "/input" && args.get("--directory")? == "/output",
        "fixed security installer paths required",
    )?;
    let role = args.get("--role")?;
    let name = args.get("--generation")?;
    generation(name)?;
    let uid = if role == "administrator" {
        1000
    } else if role == "frontend" {
        101
    } else {
        ROLES
            .iter()
            .find(|(r, _, _)| *r == role)
            .ok_or("unknown security role")?
            .2
    };
    let source = Path::new("/input");
    let output = Path::new("/output");
    owned_dir(output, uid)?;
    if role == "administrator" {
        install_file(&source.join("input.json"), &output.join("input.json"), uid)?;
    } else if role == "frontend" {
        let target = output.join(name);
        owned_dir(&target, uid)?;
        for file in ["cert.pem", "key.pem", "ca.pem", "nginx.conf"] {
            install_file(&source.join(file), &target.join(file), uid)?;
        }
        select_generation(output, name)?;
    } else {
        let work = output.join("workload");
        owned_dir(&work, uid)?;
        let target = work.join(name);
        owned_dir(&target, uid)?;
        for file in ["cert.pem", "key.pem", "ca.pem", "identity.pem"] {
            install_file(
                &source.join("workload").join(name).join(file),
                &target.join(file),
                uid,
            )?;
        }
        if !app_files(role).is_empty() {
            let app = output.join("application");
            owned_dir(&app, uid)?;
            for file in app_files(role) {
                install_file(&source.join("application").join(file), &app.join(file), uid)?;
            }
        }
        select_generation(&work, name)?;
    }
    Ok(json!({"status":"installed","role":role,"generation":name,"uid":uid,"transport":"required"}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn https_preserves_bff_proxy_and_plain_health_has_no_app() {
        let config = nginx_https(include_str!(
            "../../../../../deploy/helm/ai-gateway/files/nginx.conf"
        ))
        .unwrap();
        assert!(config.contains("listen 8443 ssl;"));
        assert!(config.contains("proxy_set_header X-Forwarded-Proto $scheme;"));
        assert!(config.contains("proxy_set_header Host $http_host;"));
        assert!(config.contains("location / { return 404; }"));
        assert!(!config.contains("proxy_ssl_verify off"));
        assert!(nginx_https("server { listen 80; }").is_err());
    }
    #[test]
    fn rotation_names_and_private_role_mapping_are_closed() {
        for invalid in ["../old", "", "r1/current", "r1\nr2"] {
            assert!(generation(invalid).is_err());
        }
        assert!(generation("transport-r2").is_ok());
        assert!(app_files("agent-runtime").is_empty());
        assert!(!app_files("knowledge-service").contains(&"api-private.pem"));
    }
    #[test]
    fn extension_reuses_application_material_and_refuses_credential_drift() {
        let root =
            std::env::temp_dir().join(format!("agentforge-app-secret-test-{}", random().unwrap()));
        directory(&root).unwrap();
        let e = BTreeMap::from([
            ("JWT_SECRET".into(), "existing-jwt-private-fixture".into()),
            (
                "GATEWAY_ENCRYPTION_KEY".into(),
                "existing-encryption-fixture".into(),
            ),
            ("AUTH_ALLOWED_EMAIL_DOMAIN".into(), "example.com".into()),
        ]);
        let base = root.join("application");
        application_keys(Path::new("/usr/bin/openssl"), &base, &e).unwrap();
        let files = [
            "gateway/api-private.pem",
            "gateway/api-config.json",
            "gateway/internal-auth-keys",
            "agent-runtime/bridge.env",
            "administrator/input.json",
        ];
        let original: Vec<_> = files
            .iter()
            .map(|file| read(&base.join(file), true).unwrap())
            .collect();
        application_keys(Path::new("/usr/bin/openssl"), &base, &e).unwrap();
        for (file, bytes) in files.iter().zip(&original) {
            assert_eq!(read(&base.join(file), true).unwrap(), *bytes);
        }
        let input: Value = serde_json::from_slice(&original[4]).unwrap();
        assert_eq!(input["email"], "admin@example.com");
        assert!(input["password"].as_str().unwrap().len() >= 64);
        assert!(!app_files("knowledge-worker").contains(&"api-private.pem"));
        let mut drift = e;
        drift.insert("JWT_SECRET".into(), "different-selected-secret".into());
        assert!(application_keys(Path::new("/usr/bin/openssl"), &base, &drift).is_err());
        assert_eq!(
            read(&base.join("gateway/api-private.pem"), true).unwrap(),
            original[0]
        );
        let mut valid = drift;
        valid.insert("JWT_SECRET".into(), "existing-jwt-private-fixture".into());
        fs::write(
            base.join("agent-runtime/bridge.env"),
            b"AI_PLATFORM_RUNTIME_MODEL_PROXY_TOKEN='bad'\nPROVIDER_API_KEY='forbidden'\n",
        )
        .unwrap();
        assert!(application_keys(Path::new("/usr/bin/openssl"), &base, &valid).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn real_issued_workload_and_console_certificates_validate_and_wrong_identity_fails() {
        let root =
            std::env::temp_dir().join(format!("agentforge-security-test-{}", random().unwrap()));
        directory(&root).unwrap();
        let openssl = Path::new("/usr/bin/openssl");
        let issuer = root.join("issuer");
        issue_ca(openssl, &issuer, false).unwrap();
        let work = root.join("work");
        directory(&work).unwrap();
        let id = "spiffe://agentforge.internal/service/gateway";
        leaf(
            openssl,
            &issuer,
            &work.join("r1"),
            "gateway-workload",
            Some(id),
        )
        .unwrap();
        std::os::unix::fs::symlink("r1", work.join("current")).unwrap();
        let good = ServiceIdentity::parse(id).unwrap();
        let files = CredentialFiles::new(work.clone()).unwrap();
        ClientCredentials::new(files.clone(), good.clone())
            .load(good.clone())
            .unwrap();
        let wrong =
            ServiceIdentity::parse("spiffe://agentforge.internal/service/knowledge").unwrap();
        assert!(ClientCredentials::new(files, wrong).load(good).is_err());
        let old_certificate = read(&work.join("r1/cert.pem"), false).unwrap();
        leaf(
            openssl,
            &issuer,
            &work.join("r2"),
            "gateway-workload",
            Some(id),
        )
        .unwrap();
        std::os::unix::fs::symlink("r1", work.join(".current-1")).unwrap();
        select_generation(&work, "r2").unwrap();
        select_generation(&work, "r2").unwrap();
        assert!(work.join(".current-1").is_symlink());
        ClientCredentials::new(
            CredentialFiles::new(work.clone()).unwrap(),
            ServiceIdentity::parse(id).unwrap(),
        )
        .load(ServiceIdentity::parse(id).unwrap())
        .unwrap();
        assert_eq!(
            read(&work.join("r1/cert.pem"), false).unwrap(),
            old_certificate
        );
        assert_ne!(
            read(&work.join("r2/cert.pem"), false).unwrap(),
            old_certificate
        );
        fs::set_permissions(work.join("r2/key.pem"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            ClientCredentials::new(
                CredentialFiles::new(work.clone()).unwrap(),
                ServiceIdentity::parse(id).unwrap()
            )
            .load(ServiceIdentity::parse(id).unwrap())
            .is_err()
        );
        let browser = root.join("browser");
        issue_ca(openssl, &browser, true).unwrap();
        let console = root.join("console");
        leaf(openssl, &browser, &console, "localhost", None).unwrap();
        assert!(check_leaf(openssl, &console, "example.com").is_err());
        assert!(
            leaf(
                openssl,
                &browser,
                &root.join("forbidden-dns"),
                "example.com",
                None
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
