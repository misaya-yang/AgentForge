//! Offline local-Compose CA and atomic credential generations. Never starts a
//! service or exports a private key through stdout, argv, environment or logs.
use ai_platform_service_primitives::workload_identity::{ServiceIdentity, secrets::read_file};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
fn arguments() -> Result<(String, BTreeMap<String, String>), &'static str> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or("command_required")?;
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        if !key.starts_with("--") || values.contains_key(&key) {
            return Err("option_invalid");
        }
        let value = args
            .next()
            .filter(|v| !v.starts_with("--"))
            .ok_or("option_value_required")?;
        values.insert(key, value);
    }
    Ok((command, values))
}
fn value<'a>(args: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, &'static str> {
    args.get(name)
        .map(String::as_str)
        .ok_or("required_option_missing")
}
fn absolute(value: &str) -> Result<PathBuf, &'static str> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("absolute_directory_required");
    }
    Ok(path)
}
fn directory(path: &Path) -> Result<(), &'static str> {
    #[cfg(not(unix))]
    return Err("private_acl_adapter_required");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !path.exists() {
            fs::create_dir_all(path).map_err(|_| "directory_create_failed")?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| "directory_permissions_failed")?;
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| "directory_unavailable")?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err("private_directory_required");
        }
        Ok(())
    }
}
fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), &'static str> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o644 });
    }
    let mut file = options
        .open(path)
        .map_err(|_| "output_exists_or_unavailable")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "output_write_failed")
}
fn openssl(directory: &Path, args: &[&str]) -> Result<(), &'static str> {
    if !Command::new("openssl")
        .args(args)
        .current_dir(directory)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "openssl_unavailable")?
        .success()
    {
        return Err("openssl_operation_failed");
    }
    Ok(())
}
fn public_output(args: &[&str]) -> Result<Vec<u8>, &'static str> {
    let output = Command::new("openssl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "openssl_unavailable")?;
    if !output.status.success() || output.stdout.len() > 64 * 1024 {
        return Err("certificate_public_key_check_failed");
    }
    Ok(output.stdout)
}
fn activate(root: &Path, generation: &str) -> Result<(), &'static str> {
    if generation.is_empty()
        || generation.len() > 80
        || !generation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("generation_invalid");
    }
    let base = fs::canonicalize(root).map_err(|_| "credentials_root_unavailable")?;
    let target = base.join(generation);
    if !fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
        return Err("generation_invalid");
    }
    let cert = target.join("cert.pem");
    let key = target.join("key.pem");
    let ca = target.join("ca.pem");
    let _ = read_file(&key, true).map_err(|_| "private_key_invalid")?;
    let _ = read_file(&cert, false).map_err(|_| "certificate_invalid")?;
    let _ = read_file(&ca, false).map_err(|_| "trust_bundle_invalid")?;
    openssl(&target, &["verify", "-CAfile", "ca.pem", "cert.pem"])?;
    let key_path = key.to_str().ok_or("path_invalid")?;
    let cert_path = cert.to_str().ok_or("path_invalid")?;
    if public_output(&["pkey", "-in", key_path, "-pubout"])?
        != public_output(&["x509", "-in", cert_path, "-pubkey", "-noout"])?
    {
        return Err("certificate_key_mismatch");
    }
    #[cfg(unix)]
    {
        let next = base.join(format!(".current-{}", std::process::id()));
        std::os::unix::fs::symlink(generation, &next)
            .map_err(|_| "generation_switch_prepare_failed")?;
        fs::rename(next, base.join("current")).map_err(|_| "generation_switch_failed")?;
        fs::File::open(&base)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "generation_switch_sync_failed")?;
        Ok(())
    }
    #[cfg(not(unix))]
    Err("atomic_private_generation_adapter_required")
}
/// Kubernetes projects immutable root-owned Secret files as 0440 for a selected
/// pod fsGroup. Only the import boundary accepts that bounded group-read mode;
/// the activated workload key is always copied into a 0600 owner-private file.
fn projected_key(
    path: &Path,
) -> Result<ai_platform_service_primitives::workload_identity::secrets::SecretBytes, &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path).map_err(|_| "svid_source_invalid")?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o037 != 0 {
            return Err("svid_source_key_permissions");
        }
    }
    #[cfg(not(unix))]
    return Err("private_acl_adapter_required");
    read_file(path, false).map_err(|_| "svid_source_invalid")
}
fn run() -> Result<(), &'static str> {
    let (command, args) = arguments()?;
    let allowed: &[&str] = match command.as_str() {
        "init-ca" => &["--out"],
        "issue" => &[
            "--ca",
            "--out",
            "--generation",
            "--spiffe-id",
            "--dns",
            "--days",
            "--trust-bundle",
        ],
        "activate" => &["--out", "--generation"],
        "import-svid" => &["--out", "--generation", "--source-root", "--spiffe-id"],
        _ => return Err("command_invalid"),
    };
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown_option");
    }
    let root = absolute(value(&args, "--out")?)?;
    directory(&root)?;
    match command.as_str() {
        "init-ca" => {
            write_new(&root.join("ca.key"), b"", true)?;
            write_new(&root.join("ca.cnf"),b"[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=AgentForge local workload CA\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n",false)?;
            openssl(
                &root,
                &[
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-days",
                    "365",
                    "-keyout",
                    "ca.key",
                    "-out",
                    "ca.pem",
                    "-config",
                    "ca.cnf",
                ],
            )?;
        }
        "issue" => {
            let ca = absolute(value(&args, "--ca")?)?;
            let _ = read_file(&ca.join("ca.key"), true).map_err(|_| "ca_private_key_invalid")?;
            let generation = value(&args, "--generation")?;
            if generation.is_empty()
                || generation.len() > 80
                || !generation
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err("generation_invalid");
            }
            let id = ServiceIdentity::parse(value(&args, "--spiffe-id")?)
                .map_err(|_| "spiffe_id_invalid")?;
            let dns = value(&args, "--dns")?;
            if dns.len() > 253
                || dns.split('.').any(|s| {
                    s.is_empty()
                        || s.len() > 63
                        || s.starts_with('-')
                        || s.ends_with('-')
                        || !s
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })
            {
                return Err("dns_san_invalid");
            }
            let days = args
                .get("--days")
                .map(String::as_str)
                .unwrap_or("1")
                .parse::<u8>()
                .map_err(|_| "leaf_lifetime_invalid")?;
            if !(1..=7).contains(&days) {
                return Err("leaf_lifetime_invalid");
            }
            let target = root.join(generation);
            if target.exists() {
                return Err("generation_already_exists");
            }
            directory(&target)?;
            write_new(&target.join("key.pem"), b"", true)?;
            write_new(&target.join("leaf.cnf"),format!("[leaf]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth,clientAuth\nsubjectAltName=DNS:{dns},URI:{}\n",id.as_str()).as_bytes(),false)?;
            openssl(
                &target,
                &[
                    "req",
                    "-new",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-keyout",
                    "key.pem",
                    "-out",
                    "leaf.csr",
                    "-subj",
                    "/CN=AgentForge workload",
                ],
            )?;
            let ca_cert = ca.join("ca.pem");
            let ca_key = ca.join("ca.key");
            let serial = format!("0x{}", uuid::Uuid::new_v4().simple());
            openssl(
                &target,
                &[
                    "x509",
                    "-req",
                    "-in",
                    "leaf.csr",
                    "-CA",
                    ca_cert.to_str().ok_or("path_invalid")?,
                    "-CAkey",
                    ca_key.to_str().ok_or("path_invalid")?,
                    "-set_serial",
                    &serial,
                    "-days",
                    &days.to_string(),
                    "-out",
                    "cert.pem",
                    "-extfile",
                    "leaf.cnf",
                    "-extensions",
                    "leaf",
                ],
            )?;
            let bundle = args
                .get("--trust-bundle")
                .map(PathBuf::from)
                .unwrap_or(ca_cert);
            let bytes = read_file(&bundle, false).map_err(|_| "trust_bundle_invalid")?;
            write_new(&target.join("ca.pem"), bytes.expose(), false)?;
            let cert =
                read_file(&target.join("cert.pem"), false).map_err(|_| "certificate_invalid")?;
            let key =
                read_file(&target.join("key.pem"), true).map_err(|_| "private_key_invalid")?;
            let mut combined = cert.expose().to_vec();
            combined.extend_from_slice(key.expose());
            let result = write_new(&target.join("identity.pem"), &combined, true);
            combined.fill(0);
            result?;
            // Issuing never activates implicitly: roll out overlapping trust before keys.
        }
        "import-svid" => {
            use rustls::pki_types::{CertificateDer, pem::PemObject};
            let source = fs::canonicalize(absolute(value(&args, "--source-root")?)?)
                .map_err(|_| "svid_source_unavailable")?;
            // Kubernetes swaps ..data atomically. Pin that immutable projection
            // once instead of resolving three leaf symlinks across a rotation.
            let projected = source.join("..data").exists();
            let snapshot = if projected {
                let snapshot = fs::canonicalize(source.join("..data"))
                    .map_err(|_| "svid_source_unavailable")?;
                if !snapshot.starts_with(&source) {
                    return Err("svid_source_escape");
                }
                snapshot
            } else {
                source.clone()
            };
            let generation = value(&args, "--generation")?;
            if generation.is_empty()
                || generation.len() > 80
                || !generation
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err("generation_invalid");
            }
            let identity = ServiceIdentity::parse(value(&args, "--spiffe-id")?)
                .map_err(|_| "spiffe_id_invalid")?;
            let mut inputs = Vec::new();
            for (name, private) in [("cert.pem", false), ("key.pem", true), ("ca.pem", false)] {
                let target =
                    fs::canonicalize(snapshot.join(name)).map_err(|_| "svid_source_unavailable")?;
                if !target.starts_with(&source) {
                    return Err("svid_source_escape");
                }
                inputs.push((
                    name,
                    private,
                    if private && projected {
                        projected_key(&target)?
                    } else {
                        read_file(&target, private).map_err(|_| "svid_source_invalid")?
                    },
                ));
            }
            let leaf = CertificateDer::pem_slice_iter(inputs[0].2.expose())
                .next()
                .ok_or("svid_certificate_missing")?
                .map_err(|_| "svid_certificate_invalid")?;
            if ai_platform_service_primitives::workload_identity::certificate_identity(
                leaf.as_ref(),
            )
            .map_err(|_| "svid_identity_invalid")?
                != identity
            {
                return Err("svid_identity_mismatch");
            }
            let target = root.join(generation);
            if target.exists() {
                return Err("generation_already_exists");
            }
            directory(&target)?;
            for (name, private, bytes) in &inputs {
                write_new(&target.join(name), bytes.expose(), *private)?;
            }
            let mut combined = inputs[0].2.expose().to_vec();
            combined.extend_from_slice(inputs[1].2.expose());
            let result = write_new(&target.join("identity.pem"), &combined, true);
            combined.fill(0);
            result?;
            activate(&root, generation)?;
        }
        "activate" => activate(&root, value(&args, "--generation")?)?,
        _ => unreachable!(),
    }
    println!("workload credential operation complete; private material retained in files");
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("workload credentials: {error}");
            std::process::ExitCode::from(2)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn projected_import_accepts_only_owner_or_group_read_and_copies_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("svid-projection-{}", uuid::Uuid::new_v4()));
        directory(&root).unwrap();
        let key = root.join("key.pem");
        write_new(&key, b"synthetic-projected-key", true).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(read_file(&key, true).is_err());
        let bytes = projected_key(&key).unwrap();
        let copy = root.join("private.pem");
        write_new(&copy, bytes.expose(), true).unwrap();
        assert!(read_file(&copy, true).is_ok());
        for mode in [0o444, 0o460, 0o640 | 0o001] {
            fs::set_permissions(&key, fs::Permissions::from_mode(mode)).unwrap();
            assert!(projected_key(&key).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn provisioning_never_accepts_relative_roots_or_overwrites_private_files() {
        assert!(absolute("relative").is_err());
        let root = std::env::temp_dir().join(format!("workload-issuer-{}", uuid::Uuid::new_v4()));
        directory(&root).unwrap();
        let key = root.join("key");
        write_new(&key, b"synthetic-test-key", true).unwrap();
        assert!(write_new(&key, b"replacement", true).is_err());
        assert_eq!(
            read_file(&key, true).unwrap().expose(),
            b"synthetic-test-key"
        );
        for generation in ["../escape", "/absolute", ".", "", "generation/child"] {
            assert!(activate(&root, generation).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
