//! Explicit offline import of existing local database credentials. No shell/env
//! evaluation, SQL, credential rotation, service operation or provider export.
use super::{Result, ensure, material, valid_identifier};
use ai_platform_service_primitives::data_connection::DatabaseRole;
use std::{collections::BTreeMap, io::Read, net::IpAddr, path::Path};

const PREFIX: &str = "AI_GATEWAY_DATABASE_";
#[cfg(test)]
const MIGRATOR: &str = "ai_gateway_migrator";
const SUFFIXES: [&str; 7] = [
    "ADMIN",
    "MIGRATOR",
    "GATEWAY",
    "RUNTIME",
    "CAPABILITY_WORKER",
    "KNOWLEDGE_API",
    "KNOWLEDGE_WORKER",
];
// Deliberately no Debug/Serialize: neither diagnostics nor receipts expose secrets.
struct Credentials {
    passwords: BTreeMap<String, String>,
    bootstrap: String,
    source_database: String,
}
struct Dsn {
    user: String,
    password: String,
    address: IpAddr,
    port: u16,
    database: String,
}
fn decode(raw: &str) -> Result<String> {
    let mut result = Vec::new();
    let mut bytes = raw.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let a = bytes.next().and_then(|v| (v as char).to_digit(16));
            let b = bytes.next().and_then(|v| (v as char).to_digit(16));
            result.push(
                (a.ok_or("invalid database URI encoding")? * 16
                    + b.ok_or("invalid database URI encoding")?) as u8,
            );
        } else {
            // '+' is a literal URI character, never form-urlencoded space.
            ensure(
                b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:".contains(&b),
                "database URI character requires encoding",
            )?;
            result.push(b);
        }
    }
    let value = String::from_utf8(result).map_err(|_| "invalid database URI encoding")?;
    ensure(
        !value.is_empty() && !value.chars().any(char::is_control),
        "invalid database credential shape",
    )?;
    Ok(value)
}
fn address(value: &str) -> Result<IpAddr> {
    let ip: IpAddr = value
        .parse()
        .map_err(|_| "numeric loopback address required")?;
    ensure(
        ip.is_loopback() && ip.to_string() == value,
        "canonical numeric loopback address required",
    )?;
    Ok(ip)
}
fn dsn(raw: &str, expected_user: &str, database: Option<&str>) -> Result<Dsn> {
    let rest = raw
        .strip_prefix("postgresql://")
        .or_else(|| raw.strip_prefix("postgres://"))
        .ok_or("PostgreSQL URI required")?;
    ensure(
        !rest.contains(['?', '#']) && !rest.chars().any(char::is_whitespace),
        "database URI options or whitespace refused",
    )?;
    let (authority, db) = rest
        .split_once('/')
        .ok_or("database URI identity missing")?;
    ensure(!db.contains('/'), "database URI path invalid")?;
    let (auth, host) = authority
        .split_once('@')
        .ok_or("database URI credentials missing")?;
    ensure(!host.contains('@'), "database URI authority ambiguous")?;
    let (user, password) = auth
        .split_once(':')
        .ok_or("database URI password missing")?;
    let user = decode(user)?;
    let password = decode(password)?;
    let db = decode(db)?;
    ensure(
        user == expected_user
            && valid_identifier(&db)
            && database.is_none_or(|expected| db == expected),
        "database URI role or physical database mismatch",
    )?;
    let (host, port) = if let Some(ipv6) = host.strip_prefix('[') {
        let (ip, port) = ipv6
            .split_once("]:")
            .ok_or("explicit database endpoint required")?;
        ensure(
            ip.parse::<std::net::Ipv6Addr>().is_ok(),
            "bracketed IPv6 address required",
        )?;
        (ip, port)
    } else {
        host.split_once(':')
            .ok_or("explicit database endpoint required")?
    };
    ensure(
        port.bytes().all(|b| b.is_ascii_digit()),
        "invalid database port",
    )?;
    let port: u16 = port.parse().map_err(|_| "invalid database port")?;
    ensure(port > 0, "invalid database port")?;
    Ok(Dsn {
        user,
        password,
        address: address(host)?,
        port,
        database: db,
    })
}
fn literal(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let value = if raw.starts_with(['\'', '"']) {
        let quote = raw.as_bytes()[0] as char;
        ensure(
            raw.len() >= 2 && raw.ends_with(quote),
            "unterminated local database value",
        )?;
        let value = &raw[1..raw.len() - 1];
        ensure(!value.contains(quote), "ambiguous local database quoting")?;
        value
    } else {
        raw
    };
    ensure(
        !value.is_empty() && !value.chars().any(char::is_control),
        "invalid local database value",
    )?;
    // Values are literal. No dotenv expansion, shell interpolation or unescaping.
    Ok(value.into())
}
fn tracked(key: &str) -> bool {
    key.starts_with(PREFIX)
        || key.starts_with("LOCAL_DB_") && key.ends_with("_PASSWORD")
        || [
            "POSTGRES_USER",
            "POSTGRES_PASSWORD",
            "POSTGRES_HOST",
            "POSTGRES_PORT",
            "POSTGRES_DB",
            "AI_GATEWAY_ROLE_PREFIX",
            "GATEWAY_DATABASE__DSN",
            "KNOWLEDGE_DATABASE__DSN",
        ]
        .contains(&key)
}
fn parse(input: &str, database: &str) -> Result<Credentials> {
    ensure(valid_identifier(database), "invalid physical database")?;
    let mut values = BTreeMap::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, raw)) = line.split_once('=') else {
            ensure(!tracked(line), "invalid local database assignment")?;
            continue;
        };
        let key = key.trim();
        // Reject multiline dotenv syntax even for ignored fields. Otherwise a
        // database-looking line inside a provider's quoted multiline value could
        // be mistaken for an independent credential assignment.
        let mut quote = None;
        let mut escaped = false;
        for byte in raw.trim().bytes() {
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' && quote != Some(b'\'') {
                escaped = true;
                continue;
            }
            match quote {
                Some(current) if byte == current => quote = None,
                None if byte == b'\'' || byte == b'"' => quote = Some(byte),
                _ => {}
            }
        }
        ensure(
            quote.is_none() && !escaped,
            "multiline local env values refused",
        )?;
        if !tracked(key) {
            continue;
        } // Never parse or export provider fields.
        ensure(
            values.insert(key.to_owned(), literal(raw)?).is_none(),
            "duplicate local database assignment",
        )?;
    }
    let required = |key: &str| {
        values
            .get(key)
            .ok_or_else(|| "required local database credential missing".to_string())
    };
    // Legacy DSNs bind the one existing local source, never the explicitly
    // selected output database. A fresh target reuses credentials without
    // pretending that the old source already points at that new database.
    let admin = dsn(required("AI_GATEWAY_DATABASE_ADMIN_DSN")?, "postgres", None)?;
    let source_database = admin.database.clone();
    let mut parsed = BTreeMap::from([("ADMIN", admin)]);
    for suffix in SUFFIXES.into_iter().filter(|v| *v != "ADMIN") {
        let user = format!("ai_gateway_{}", suffix.to_ascii_lowercase());
        parsed.insert(
            suffix,
            dsn(
                required(&format!("{PREFIX}{suffix}_DSN"))?,
                &user,
                Some(&source_database),
            )?,
        );
    }
    let admin = &parsed["ADMIN"];
    for entry in parsed.values() {
        ensure(
            (entry.address, entry.port, &entry.database)
                == (admin.address, admin.port, &admin.database),
            "local database endpoints conflict",
        )?;
    }
    // Existing dotenv compatibility aliases must agree; they never become output inputs.
    for (key, expected) in [
        ("POSTGRES_USER", "postgres"),
        ("POSTGRES_DB", source_database.as_str()),
        ("POSTGRES_PASSWORD", admin.password.as_str()),
        ("AI_GATEWAY_ROLE_PREFIX", "ai_gateway_"),
    ] {
        if let Some(value) = values.get(key) {
            ensure(value == expected, "local database alias conflicts")?;
        }
    }
    if let Some(value) = values.get("POSTGRES_HOST") {
        ensure(
            address(value)? == admin.address,
            "local database host conflicts",
        )?;
    }
    if let Some(value) = values.get("POSTGRES_PORT") {
        ensure(
            value.parse::<u16>().ok() == Some(admin.port),
            "local database port conflicts",
        )?;
    }
    for (key, suffix) in [
        ("GATEWAY_DATABASE__DSN", "GATEWAY"),
        ("KNOWLEDGE_DATABASE__DSN", "KNOWLEDGE_API"),
    ] {
        if let Some(value) = values.get(key) {
            let expected = &parsed[suffix];
            let alias = dsn(value, &expected.user, Some(&source_database))?;
            ensure(
                (alias.address, alias.port, alias.password)
                    == (expected.address, expected.port, expected.password.clone()),
                "local database DSN aliases conflict",
            )?;
        }
    }
    let mut passwords = BTreeMap::new();
    for suffix in SUFFIXES.into_iter().filter(|v| *v != "ADMIN") {
        let entry = &parsed[suffix];
        if let Some(value) = values.get(&format!("LOCAL_DB_{suffix}_PASSWORD")) {
            ensure(
                value == &entry.password,
                "local role password aliases conflict",
            )?;
        }
        passwords.insert(entry.user.clone(), entry.password.clone());
    }
    for key in values.keys() {
        ensure(
            !key.starts_with(PREFIX) || SUFFIXES.iter().any(|s| key == &format!("{PREFIX}{s}_DSN")),
            "unsupported local database authority",
        )?;
        ensure(
            !key.starts_with("LOCAL_DB_")
                || SUFFIXES
                    .iter()
                    .filter(|s| **s != "ADMIN")
                    .any(|s| key == &format!("LOCAL_DB_{s}_PASSWORD")),
            "unknown local password role",
        )?;
    }
    Ok(Credentials {
        passwords,
        bootstrap: admin.password.clone(),
        source_database,
    })
}
fn read_private(path: &Path) -> Result<String> {
    ensure(
        path.is_absolute()
            && path.parent().and_then(|p| p.canonicalize().ok()).as_deref() == path.parent(),
        "absolute private env without symlink ancestors required",
    )?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| "private local env unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "private local env unavailable")?;
    ensure(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= 1024 * 1024,
        "private local env must be a bounded regular file",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure(
            [0o400, 0o600].contains(&(metadata.permissions().mode() & 0o7777))
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.nlink() == 1,
            "private local env requires owned 0400 or 0600 single-link file",
        )?;
    }
    let mut bytes = Vec::new();
    (&file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "private local env read failed")?;
    ensure(
        !bytes.is_empty() && bytes.len() <= 1024 * 1024,
        "private local env size invalid",
    )?;
    let after = file
        .metadata()
        .map_err(|_| "private local env unavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure(
            after.uid() == metadata.uid()
                && after.nlink() == 1
                && after.permissions().mode() == metadata.permissions().mode(),
            "private local env changed during read",
        )?;
    }
    ensure(
        bytes.len() as u64 == metadata.len()
            && after.len() == metadata.len()
            && after.modified().ok() == metadata.modified().ok(),
        "private local env changed during read",
    )?;
    String::from_utf8(bytes).map_err(|_| "private local env encoding invalid".into())
}
fn connection_record(directory: &Path, role: &str, database: &str) -> serde_json::Value {
    serde_json::json!({"dsn_file":directory.join("dsn"),"password_file":directory.join("password"),"tls_ca_file":directory.join("ca-bundle.pem"),"tls_cert_file":directory.join("tls.crt"),"tls_key_file":directory.join("tls.key"),"expected_host":"postgres","physical_database":database,"expected_role":role,"mode":"direct"})
}
fn json_file(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    material::new_file(
        path,
        &serde_json::to_vec_pretty(value).map_err(|_| "path map encoding failed")?,
    )
}
/// Explicitly imports existing secrets and invokes the existing real PKI preparer.
/// Failure keeps the new incomplete directory; never retries or replaces it.
pub fn prepare(
    root: &Path,
    input: &Path,
    output: &Path,
    database: &str,
    client: &str,
    backend: &str,
    openssl: &Path,
) -> Result<serde_json::Value> {
    let credentials = parse(&read_private(input)?, database)?;
    let root = root.canonicalize().map_err(|_| "repository unavailable")?;
    ensure(
        root.join("deploy/data/pgbouncer/pgbouncer.ini").is_file()
            && root
                .join("deploy/data/postgres/pg_hba.conf.template")
                .is_file(),
        "repository data templates unavailable",
    )?;
    let parent = output.parent().ok_or("external parent required")?;
    let canonical = parent
        .canonicalize()
        .map_err(|_| "external parent unavailable")?;
    ensure(
        output.is_absolute()
            && canonical == parent
            && !canonical.starts_with(&root)
            && std::fs::symlink_metadata(output).is_err(),
        "new absolute external directory without symlink ancestors required",
    )?;
    ensure(
        openssl.is_absolute() && openssl.is_file(),
        "explicit existing absolute OpenSSL executable required",
    )?;
    material::validate_networks(client, backend)?;
    material::private_directory(output)?;
    material::new_file(
        &output.join("INCOMPLETE"),
        b"Imported material is not deployment acceptance.\n",
    )?;
    let inputs = output.join("inputs");
    material::private_directory(&inputs)?;
    let bootstrap = inputs.join("postgres-bootstrap-password");
    material::new_file(&bootstrap, credentials.bootstrap.as_bytes())?;
    let mut paths = BTreeMap::new();
    for (role, password) in &credentials.passwords {
        let path = inputs.join(format!("{role}.password"));
        material::new_file(&path, password.as_bytes())?;
        paths.insert(role.clone(), path.display().to_string());
    }
    json_file(&output.join("imported-password-paths.json"), &paths)?;
    let application_paths: BTreeMap<String, String> = DatabaseRole::ALL
        .into_iter()
        .map(|r| (r.name().to_string(), paths[r.name()].clone()))
        .collect();
    json_file(&output.join("password-files.json"), &application_paths)?;
    let private = output.join("material");
    material::prepare(
        &root,
        &private,
        database,
        client,
        backend,
        &application_paths,
        &bootstrap,
        openssl,
    )?;
    let authority = output.join("authority");
    material::private_directory(&authority)?;
    let mut connections = BTreeMap::new();
    for suffix in SUFFIXES.into_iter().filter(|v| *v != "ADMIN") {
        let alias = suffix.to_ascii_lowercase();
        let role = format!("ai_gateway_{alias}");
        let source = private.join(&role);
        let directory = authority.join(&alias);
        material::private_directory(&directory)?;
        for name in ["tls.crt", "tls.key", "ca-bundle.pem"] {
            material::new_file(
                &directory.join(name),
                &std::fs::read(source.join(name))
                    .map_err(|_| "generated authority material unavailable")?,
            )?;
        }
        material::new_file(
            &directory.join("dsn"),
            format!("postgresql://{role}@postgres:5432/{database}\n").as_bytes(),
        )?;
        material::new_file(
            &directory.join("password"),
            credentials.passwords[&role].as_bytes(),
        )?;
        connections.insert(alias, connection_record(&directory, &role, database));
    }
    // All maps contain paths/public identities only; no original URI or provider data.
    json_file(
        &output.join("authority-connection-files.json"),
        &connections,
    )?;
    json_file(
        &output.join("deployment-paths.json"),
        &serde_json::json!({"DATA_PRIVATE_DIRECTORY":private,"DATA_AUTHORITY_DIRECTORY":authority,"POSTGRES_DB":database,"bootstrap_password_file":bootstrap,"password_files":output.join("password-files.json")}),
    )?;
    json_file(
        &output.join("import-receipt.json"),
        &serde_json::json!({"schema":"agentforge-local-env-data-import/v1","status":"prepared-not-deployed","source_database":credentials.source_database,"target_database":database,"login_roles":6,"existing_passwords_reused":true,"database_passwords_changed":false,"database_contacted":false,"database_created":false,"provider_fields_exported":false,"runtime_acceptance":false}),
    )?;
    verify_contents(output)?;
    std::fs::remove_file(output.join("INCOMPLETE"))
        .map_err(|_| "completion marker update failed")?;
    Ok(
        serde_json::json!({"status":"prepared-not-deployed","login_roles":6,"existing_passwords_reused":true,"database_contacted":false,"provider_fields_exported":false,"runtime_acceptance":false}),
    )
}

fn verify_contents(output: &Path) -> Result<()> {
    material::validate_material(&output.join("material"))?;
    let paths: BTreeMap<String, String> =
        serde_json::from_str(&read_private(&output.join("imported-password-paths.json"))?)
            .map_err(|_| "imported path map invalid")?;
    ensure(paths.len() == 6, "imported role inventory invalid")?;
    let deployment: serde_json::Value =
        serde_json::from_str(&read_private(&output.join("deployment-paths.json"))?)
            .map_err(|_| "deployment path map invalid")?;
    let database = deployment["POSTGRES_DB"]
        .as_str()
        .filter(|v| valid_identifier(v))
        .ok_or("deployment database identity invalid")?;
    ensure(
        deployment
            == serde_json::json!({"DATA_PRIVATE_DIRECTORY":output.join("material"),"DATA_AUTHORITY_DIRECTORY":output.join("authority"),"POSTGRES_DB":database,"bootstrap_password_file":output.join("inputs/postgres-bootstrap-password"),"password_files":output.join("password-files.json")}),
        "deployment path map mismatch",
    )?;
    let connections: BTreeMap<String, serde_json::Value> = serde_json::from_str(&read_private(
        &output.join("authority-connection-files.json"),
    )?)
    .map_err(|_| "authority path map invalid")?;
    ensure(connections.len() == 6, "authority role inventory invalid")?;
    let applications: BTreeMap<String, String> =
        serde_json::from_str(&read_private(&output.join("password-files.json"))?)
            .map_err(|_| "application path map invalid")?;
    let expected_applications: BTreeMap<String, String> = DatabaseRole::ALL
        .into_iter()
        .map(|r| {
            (
                r.name().into(),
                output
                    .join("inputs")
                    .join(format!("{}.password", r.name()))
                    .display()
                    .to_string(),
            )
        })
        .collect();
    ensure(
        applications == expected_applications,
        "application path map mismatch",
    )?;
    let bootstrap = read_private(&output.join("inputs/postgres-bootstrap-password"))?;
    ensure(
        !bootstrap.is_empty() && !bootstrap.chars().any(char::is_control),
        "bootstrap credential shape invalid",
    )?;
    for suffix in SUFFIXES.into_iter().filter(|v| *v != "ADMIN") {
        let alias = suffix.to_ascii_lowercase();
        let role = format!("ai_gateway_{alias}");
        let expected = output.join("inputs").join(format!("{role}.password"));
        ensure(
            paths.get(&role) == Some(&expected.display().to_string()),
            "imported path map mismatch",
        )?;
        let password = read_private(&expected)?;
        ensure(
            !password.is_empty() && !password.chars().any(char::is_control),
            "imported credential shape invalid",
        )?;
        let directory = output.join("authority").join(&alias);
        ensure(
            connections.get(&alias) == Some(&connection_record(&directory, &role, database)),
            "authority path map mismatch",
        )?;
        ensure(
            read_private(&directory.join("dsn"))?
                == format!("postgresql://{role}@postgres:5432/{database}\n"),
            "authority DSN identity mismatch",
        )?;
        ensure(
            read_private(&directory.join("password"))? == password,
            "authority password copy mismatch",
        )?;
        for name in ["tls.crt", "tls.key", "ca-bundle.pem"] {
            ensure(
                read_private(&directory.join(name))?
                    == read_private(&output.join("material").join(&role).join(name))?,
                "authority PKI copy mismatch",
            )?;
        }
    }
    fn private_tree(path: &Path) -> Result<()> {
        let m = std::fs::symlink_metadata(path).map_err(|_| "imported material unavailable")?;
        ensure(
            !m.file_type().is_symlink() && (m.is_file() || m.is_dir()),
            "imported material cannot contain links or special files",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            ensure(
                m.uid() == unsafe { libc::geteuid() }
                    && (m.is_dir() || m.nlink() == 1)
                    && m.permissions().mode() & 0o7777 == if m.is_dir() { 0o700 } else { 0o600 },
                "imported material ownership or permissions invalid",
            )?;
        }
        if m.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|_| "imported directory unavailable")? {
                private_tree(&entry.map_err(|_| "imported directory unavailable")?.path())?;
            }
        }
        Ok(())
    }
    private_tree(output)
}
/// Offline integrity check for this import wrapper, including the real nested PKI.
pub fn validate(output: &Path) -> Result<serde_json::Value> {
    ensure(
        output.is_absolute() && output.canonicalize().ok().as_deref() == Some(output),
        "canonical absolute private import directory required",
    )?;
    ensure(
        !output.join("INCOMPLETE").exists(),
        "local credential preparation incomplete",
    )?;
    let receipt: serde_json::Value =
        serde_json::from_str(&read_private(&output.join("import-receipt.json"))?)
            .map_err(|_| "import receipt invalid")?;
    ensure(
        receipt["schema"] == "agentforge-local-env-data-import/v1"
            && receipt["status"] == "prepared-not-deployed"
            && receipt["login_roles"] == 6
            && receipt["database_contacted"] == false
            && receipt["database_passwords_changed"] == false
            && receipt["existing_passwords_reused"] == true
            && receipt["provider_fields_exported"] == false
            && receipt["runtime_acceptance"] == false
            && receipt["database_created"] == false
            && receipt["source_database"]
                .as_str()
                .is_some_and(valid_identifier),
        "import receipt contract invalid",
    )?;
    verify_contents(output)?;
    let deployment: serde_json::Value =
        serde_json::from_str(&read_private(&output.join("deployment-paths.json"))?)
            .map_err(|_| "deployment path map invalid")?;
    ensure(
        receipt["target_database"] == deployment["POSTGRES_DB"],
        "target database receipt mismatch",
    )?;
    Ok(
        serde_json::json!({"status":"passed","scope":"import-private-files-and-pki-integrity","database_contacted":false,"runtime_acceptance":false}),
    )
}

#[cfg(test)]
mod tests;
