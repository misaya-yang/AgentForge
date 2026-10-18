use super::{Result, ensure, valid_identifier, validate_ini};
use ai_platform_service_primitives::data_connection::{DatabaseRole, read_material};
use std::{collections::BTreeMap, io::Write, net::Ipv4Addr, path::Path, process::Command};
pub(super) fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|_| "private output already exists or unavailable")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "private output write failed".into())
}
fn text_secret(path: &Path) -> Result<String> {
    let value = String::from_utf8(read_material(path, true).map_err(|e| e.to_string())?)
        .map_err(|_| "secret encoding invalid")?;
    let value = value.trim_end_matches(['\r', '\n']);
    ensure(
        !value.is_empty() && !value.chars().any(char::is_control),
        "secret shape invalid",
    )?;
    Ok(value.into())
}
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
fn cidr(value: &str) -> Result<(u32, u32)> {
    let (ip, bits) = value.split_once('/').ok_or("IPv4 CIDR required")?;
    let ip: Ipv4Addr = ip.parse().map_err(|_| "invalid IPv4 address")?;
    let bits: u32 = bits.parse().map_err(|_| "invalid CIDR prefix")?;
    ensure(
        (16..=30).contains(&bits),
        "bounded private network required",
    )?;
    ensure(ip.is_private(), "private data network required")?;
    let mask = u32::MAX << (32 - bits);
    let network = u32::from(ip) & mask;
    ensure(
        network == u32::from(ip),
        "CIDR must use canonical network address",
    )?;
    Ok((network, mask))
}
pub fn validate_networks(client: &str, backend: &str) -> Result<()> {
    let (a, am) = cidr(client)?;
    let (b, bm) = cidr(backend)?;
    ensure(
        a & bm != b && b & am != a,
        "client/backend networks overlap",
    )
}
pub fn render(
    root: &Path,
    output: &Path,
    database: &str,
    client: &str,
    backend: &str,
    passwords: &BTreeMap<String, String>,
    bootstrap: &Path,
) -> Result<()> {
    ensure(valid_identifier(database), "invalid database identifier")?;
    validate_networks(client, backend)?;
    ensure(
        passwords.len() == 5,
        "exact five role password files required",
    )?;
    let mut users = String::new();
    for role in DatabaseRole::ALL {
        let path = passwords
            .get(role.name())
            .ok_or("role password file missing")?;
        let password = text_secret(Path::new(path))?;
        users.push_str(&format!("{} {}\n", quoted(role.name()), quoted(&password)));
    }
    let bootstrap = text_secret(bootstrap)?;
    let ini = std::fs::read_to_string(root.join("deploy/data/pgbouncer/pgbouncer.ini"))
        .map_err(|_| "pooler template unavailable")?
        .replace("dbname=gateway", &format!("dbname={database}"));
    validate_ini(&ini)?;
    let hba = std::fs::read_to_string(root.join("deploy/data/postgres/pg_hba.conf.template"))
        .map_err(|_| "HBA template unavailable")?
        .replace("hostssl gateway ", &format!("hostssl {database} "))
        .replace("@CLIENT_CIDR@", client)
        .replace("@BACKEND_CIDR@", backend);
    new_file(&output.join("pgbouncer.ini"), ini.as_bytes())?;
    new_file(&output.join("pg_hba.conf"), hba.as_bytes())?;
    new_file(&output.join("pgbouncer-users.txt"), users.as_bytes())?;
    new_file(
        &output.join("postgres-bootstrap-password"),
        bootstrap.as_bytes(),
    )?;
    Ok(())
}
fn openssl(binary: &Path, args: &[String]) -> Result<()> {
    let mut command = Command::new(binary);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New artifacts must be private even when the operator has a permissive
        // umask. Only this OpenSSL child changes its mask; never the tool process.
        unsafe {
            command.pre_exec(|| {
                libc::umask(0o077);
                Ok(())
            });
        }
    }
    let output = command
        .output()
        .map_err(|_| "OpenSSL executable unavailable")?;
    ensure(
        output.status.success(),
        "OpenSSL certificate operation failed (output redacted)",
    )
}
fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}
pub(super) fn private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| "new private directory required".into())
}
/// Explicit operator command only; never runs from validation, startup, or tests.
/// Inputs are existing role passwords. This never changes PostgreSQL credentials,
/// runs Docker, changes ownership, replaces an existing artifact or deletes failures.
pub fn prepare(
    root: &Path,
    output: &Path,
    database: &str,
    client: &str,
    backend: &str,
    passwords: &BTreeMap<String, String>,
    bootstrap: &Path,
    openssl_binary: &Path,
) -> Result<serde_json::Value> {
    let root = root.canonicalize().map_err(|_| "repository unavailable")?;
    let parent = output
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or("existing external parent required")?;
    ensure(
        output.is_absolute() && !output.exists() && !parent.starts_with(&root),
        "new absolute external private directory required",
    )?;
    validate_networks(client, backend)?;
    ensure(
        valid_identifier(database) && passwords.len() == 5,
        "database or role inventory invalid",
    )?;
    // Verify all secrets before the first output write or certificate operation.
    text_secret(bootstrap)?;
    for role in DatabaseRole::ALL {
        text_secret(Path::new(
            passwords.get(role.name()).ok_or("role password missing")?,
        ))?;
    }
    openssl(openssl_binary, &args(&["version"]))?;
    private_directory(output)?;
    new_file(
        &output.join("INCOMPLETE"),
        b"No deployment readiness until preparation completes and ownership is verified.\n",
    )?;
    let issuer = output.join("issuer");
    private_directory(&issuer)?;
    let ca = issuer.join("ca.crt");
    let key = issuer.join("ca.key");
    openssl(
        openssl_binary,
        &vec![
            "req".into(),
            "-x509".into(),
            "-newkey".into(),
            "rsa:3072".into(),
            "-nodes".into(),
            "-sha256".into(),
            "-days".into(),
            "365".into(),
            "-subj".into(),
            "/CN=AgentForge Data Internal CA".into(),
            "-addext".into(),
            "basicConstraints=critical,CA:TRUE,pathlen:0".into(),
            "-addext".into(),
            "keyUsage=critical,keyCertSign,cRLSign".into(),
            "-keyout".into(),
            key.display().to_string(),
            "-out".into(),
            ca.display().to_string(),
        ],
    )?;
    let mut identities = vec![
        (
            "postgres".to_owned(),
            "serverAuth",
            Some("DNS:postgres,DNS:postgres-backend"),
        ),
        ("pgbouncer".into(), "serverAuth", Some("DNS:pgbouncer")),
        ("pgbouncer-backend".into(), "clientAuth", None),
        ("ai_gateway_migrator".into(), "clientAuth", None),
    ];
    for role in DatabaseRole::ALL {
        identities.push((role.name().into(), "clientAuth", None));
    }
    let mut public_fingerprints = BTreeMap::new();
    for (name, eku, san) in identities {
        let directory = output.join(&name);
        private_directory(&directory)?;
        let cert = directory.join("tls.crt");
        let private = directory.join("tls.key");
        let csr = directory.join("request.csr");
        let extensions = directory.join("extensions.cnf");
        let ext = format!(
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage={eku}\n{}",
            san.map(|s| format!("subjectAltName={s}\n"))
                .unwrap_or_default()
        );
        new_file(&extensions, ext.as_bytes())?;
        openssl(
            openssl_binary,
            &vec![
                "req".into(),
                "-new".into(),
                "-newkey".into(),
                "rsa:3072".into(),
                "-nodes".into(),
                "-sha256".into(),
                "-subj".into(),
                format!("/CN={name}"),
                "-keyout".into(),
                private.display().to_string(),
                "-out".into(),
                csr.display().to_string(),
            ],
        )?;
        let serial = uuid::Uuid::new_v4().simple().to_string();
        openssl(
            openssl_binary,
            &vec![
                "x509".into(),
                "-req".into(),
                "-sha256".into(),
                "-days".into(),
                "30".into(),
                "-in".into(),
                csr.display().to_string(),
                "-CA".into(),
                ca.display().to_string(),
                "-CAkey".into(),
                key.display().to_string(),
                "-set_serial".into(),
                format!("0x{serial}"),
                "-extfile".into(),
                extensions.display().to_string(),
                "-out".into(),
                cert.display().to_string(),
            ],
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "key permission update failed")?;
        }
        use sha2::{Digest, Sha256};
        public_fingerprints.insert(
            name.clone(),
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(&cert).map_err(|_| "certificate read failed")?)
            ),
        );
        new_file(
            &directory.join("ca-bundle.pem"),
            &std::fs::read(&ca).map_err(|_| "CA read failed")?,
        )?;
        if name == "postgres" {
            new_file(
                &directory.join("postgres.crt"),
                &std::fs::read(&cert).map_err(|_| "leaf certificate read failed")?,
            )?;
            new_file(
                &directory.join("postgres.key"),
                &std::fs::read(&private).map_err(|_| "leaf private key read failed")?,
            )?;
        }
        if name == "pgbouncer" {
            new_file(
                &directory.join("pgbouncer.crt"),
                &std::fs::read(&cert).map_err(|_| "leaf certificate read failed")?,
            )?;
            new_file(
                &directory.join("pgbouncer.key"),
                &std::fs::read(&private).map_err(|_| "leaf private key read failed")?,
            )?;
        }
        if name == "pgbouncer-backend" {
            let target = output.join("pgbouncer");
            new_file(
                &target.join("pgbouncer-backend.crt"),
                &std::fs::read(&cert).map_err(|_| "leaf certificate read failed")?,
            )?;
            new_file(
                &target.join("pgbouncer-backend.key"),
                &std::fs::read(&private).map_err(|_| "leaf private key read failed")?,
            )?;
        }
        if let Some(role) = DatabaseRole::ALL.iter().find(|role| role.name() == name) {
            for (mode, host, port, alias) in [
                ("tx", "pgbouncer", 6432, format!("{}_tx", role.alias())),
                (
                    "session",
                    "pgbouncer",
                    6432,
                    format!("{}_session", role.alias()),
                ),
                ("control", "postgres", 5432, database.into()),
            ] {
                new_file(
                    &directory.join(format!("dsn-{mode}")),
                    format!("postgresql://{}@{host}:{port}/{alias}\n", role.name()).as_bytes(),
                )?;
            }
        }
    }
    render(
        &root, output, database, client, backend, passwords, bootstrap,
    )?;
    use sha2::{Digest, Sha256};
    let ca_sha256 = format!(
        "{:x}",
        Sha256::digest(std::fs::read(&ca).map_err(|_| "CA read failed")?)
    );
    let receipt = serde_json::json!({"schema":"agentforge-data-material/v1","status":"prepared-not-deployed","leaf_valid_days":30,"ca_valid_days":365,"ca_bundle_sha256":ca_sha256,"certificate_pem_sha256":public_fingerprints,"ownership_install_required":true,"database_passwords_changed":false,"rotation_drill":false});
    new_file(
        &output.join("material-receipt.json"),
        &serde_json::to_vec_pretty(&receipt).map_err(|_| "receipt serialization failed")?,
    )?;
    std::fs::remove_file(output.join("INCOMPLETE"))
        .map_err(|_| "completion marker update failed")?;
    Ok(receipt)
}
pub fn validate_material(directory: &Path) -> Result<serde_json::Value> {
    ensure(
        !directory.join("INCOMPLETE").exists(),
        "private material preparation incomplete",
    )?;
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join("material-receipt.json"))
            .map_err(|_| "material receipt unavailable")?,
    )
    .map_err(|_| "material receipt invalid")?;
    ensure(
        receipt["schema"] == "agentforge-data-material/v1"
            && receipt["status"] == "prepared-not-deployed",
        "material receipt contract invalid",
    )?;
    let fingerprints = receipt["certificate_pem_sha256"]
        .as_object()
        .ok_or("certificate manifest missing")?;
    ensure(
        fingerprints.len() == 9,
        "certificate identity inventory incomplete",
    )?;
    use sha2::{Digest, Sha256};
    for (name, digest) in fingerprints {
        ensure(
            name == "postgres"
                || name == "pgbouncer"
                || name == "pgbouncer-backend"
                || name == "ai_gateway_migrator"
                || DatabaseRole::ALL.iter().any(|r| r.name() == name),
            "unknown certificate identity",
        )?;
        let bytes = read_material(&directory.join(name).join("tls.crt"), false)
            .map_err(|e| e.to_string())?;
        let observed = format!("{:x}", Sha256::digest(bytes));
        ensure(
            digest.as_str() == Some(observed.as_str()),
            "certificate manifest mismatch",
        )?;
        read_material(&directory.join(name).join("tls.key"), true).map_err(|e| e.to_string())?;
        let bundle = read_material(&directory.join(name).join("ca-bundle.pem"), false)
            .map_err(|e| e.to_string())?;
        let observed = format!("{:x}", Sha256::digest(bundle));
        ensure(
            receipt["ca_bundle_sha256"].as_str() == Some(observed.as_str()),
            "CA bundle manifest mismatch",
        )?;
    }
    for (source, target, basename) in [
        ("postgres", "postgres", "postgres"),
        ("pgbouncer", "pgbouncer", "pgbouncer"),
        ("pgbouncer-backend", "pgbouncer", "pgbouncer-backend"),
    ] {
        for (original, suffix, private) in [("tls.crt", "crt", false), ("tls.key", "key", true)] {
            let a = read_material(&directory.join(source).join(original), private)
                .map_err(|e| e.to_string())?;
            let b = read_material(
                &directory.join(target).join(format!("{basename}.{suffix}")),
                private,
            )
            .map_err(|e| e.to_string())?;
            ensure(a == b, "mounted certificate/key copy mismatch")?;
        }
    }
    validate_ini(
        &std::fs::read_to_string(directory.join("pgbouncer.ini"))
            .map_err(|_| "rendered pool config unavailable")?,
    )?;
    read_material(&directory.join("pgbouncer-users.txt"), true).map_err(|e| e.to_string())?;
    read_material(&directory.join("postgres-bootstrap-password"), true)
        .map_err(|e| e.to_string())?;
    Ok(
        serde_json::json!({"status":"passed","scope":"private-material-integrity-and-permissions","runtime_acceptance":false,"service_uid_readability_verified":false}),
    )
}
pub fn rotation_bundle(old: &Path, new: &Path, output: &Path) -> Result<serde_json::Value> {
    let mut old = read_material(old, false).map_err(|e| e.to_string())?;
    let new = read_material(new, false).map_err(|e| e.to_string())?;
    ensure(old != new, "rotation requires a distinct new CA")?;
    ensure(
        old.starts_with(b"-----BEGIN CERTIFICATE-----")
            && new.starts_with(b"-----BEGIN CERTIFICATE-----"),
        "CA certificate shape invalid",
    )?;
    old.push(b'\n');
    old.extend(new);
    new_file(output, &old)?;
    Ok(
        serde_json::json!({"status":"dual-trust-bundle-prepared","runtime_drill":false,"reload_performed":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_networks_must_be_distinct_and_canonical() {
        assert!(validate_networks("10.71.10.0/24", "10.71.11.0/24").is_ok());
        for (a, b) in [
            ("10.71.10.0/24", "10.71.10.0/25"),
            ("10.71.10.5/24", "10.71.11.0/24"),
            ("8.8.8.0/24", "10.71.11.0/24"),
            ("0.0.0.0/0", "10.71.11.0/24"),
        ] {
            assert!(validate_networks(a, b).is_err());
        }
    }
    #[test]
    fn authfile_escapes_quotes_backslashes_without_shell_interpolation() {
        assert_eq!(quoted("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
