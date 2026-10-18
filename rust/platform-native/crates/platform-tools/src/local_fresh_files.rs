//! Bounded installation into new, service-owned private volumes. The offline
//! issuer/input tree is never mounted into an application or changed in place.
use super::local_fresh::{directory, random, read, write};
use crate::*;
use std::{fs, os::unix::fs::PermissionsExt};

fn public(seed: &[u8; 32], id: &str, now: u64) -> Value {
    json!({"schema_version":"gateway-verification-keys/v2","keys":[{
        "key_id":id,
        "public_key_hex":ai_platform_capability_contract::lease_v3::hex(
            &ed25519_dalek::SigningKey::from_bytes(seed).verifying_key().to_bytes()),
        "not_before_ms":now.saturating_sub(60_000),
        "not_after_ms":now+30*86_400_000u64,"revoked":false}]})
}
fn seed() -> Result<([u8; 32], String)> {
    let text = random()?;
    let bytes = ai_platform_capability_contract::lease_v3::unhex::<32>(&text)
        .map_err(|_| "OS signing key invalid")?;
    Ok((bytes, text))
}
pub(super) fn prepare_model_keys(e: &mut BTreeMap<String, String>, out: &Path) -> Result<()> {
    // This fresh initializer is deliberately a first-issue operation. Existing
    // selected signing generations must go through their explicit rotation path.
    for key in [
        "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE",
        "WORKLOAD_PUBLIC_KEYRING_FILE",
        "KNOWLEDGE_MODEL_WORKLOAD_KEY_FILE",
        "KNOWLEDGE_MODEL_WORKLOAD_PUBLIC_KEYS_FILE",
    ] {
        require(
            e.get(key).is_none_or(String::is_empty),
            "existing model signing generation selected; explicit reuse/rotation profile required",
        )?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let model = out.join("model");
    directory(&model)?;
    let gateway = model.join("gateway");
    directory(&gateway)?;
    let (g, gh) = seed()?;
    let gp = public(&g, "local-gateway-fresh-1", now);
    write(
        &gateway.join("lease-private.json"),
        &serde_json::to_vec(&json!({
        "active_key_id":"local-gateway-fresh-1","signing_seed_hex":gh,"verification":gp}))?,
        0o600,
    )?;
    write(
        &model.join("gateway-public.json"),
        &serde_json::to_vec(&gp)?,
        0o600,
    )?;
    let mut all = Vec::new();
    for role in ["knowledge-api", "knowledge-worker"] {
        let dir = model.join(role);
        directory(&dir)?;
        let (s, h) = seed()?;
        let id = format!("local-{role}-fresh-1");
        let p = public(&s, &id, now);
        all.extend(
            p["keys"]
                .as_array()
                .ok_or("public key generation failed")?
                .clone(),
        );
        write(
            &dir.join("workload.json"),
            &serde_json::to_vec(&json!({
            "key_id":id,"seed_hex":h,"public_keys":p}))?,
            0o600,
        )?;
    }
    write(
        &gateway.join("knowledge-public.json"),
        &serde_json::to_vec(&json!({
        "schema_version":"gateway-verification-keys/v2","keys":all}))?,
        0o600,
    )?;
    e.insert(
        "LOCAL_FRESH_MODEL_DIRECTORY".into(),
        model.to_string_lossy().into(),
    );
    // A new database has no activated commercial price publication. This explicit
    // sentinel permits process construction and fails reservation closed until
    // an actual book is published; it is never claimed as an activated revision.
    e.entry("KNOWLEDGE_MODEL_PRICE_BOOK_REVISION".into())
        .or_insert("unpublished".into());
    Ok(())
}
fn owner(path: &Path, uid: u32) -> Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    require(
        unsafe { libc::chown(c.as_ptr(), uid, uid) } == 0,
        "private file owner installation failed",
    )
}
fn install_file(source: &Path, target: &Path, uid: u32) -> Result<()> {
    let bytes = read(source, true)?;
    if target.exists() {
        require(
            read(target, true)? == bytes,
            "existing installed generation differs; explicit rotation required",
        )?;
    } else {
        write(target, &bytes, 0o600)?;
    }
    fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
    owner(target, uid)
}
fn install_directory(path: &Path, uid: u32) -> Result<()> {
    if !path.exists() {
        directory(path)?;
    }
    require(
        fs::symlink_metadata(path)?.is_dir(),
        "regular private directory required",
    )?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    owner(path, uid)
}
type InstallPlan = (u32, Vec<(PathBuf, PathBuf)>);
fn lane_plan(lane: &str) -> Result<InstallPlan> {
    Ok(match lane {
        "socket" => (70, vec![]),
        "qdrant-storage" => (1000, vec![]),
        "postgres" => (
            70,
            vec![
                (
                    "material/postgres/postgres.crt".into(),
                    "postgres.crt".into(),
                ),
                (
                    "material/postgres/postgres.key".into(),
                    "postgres.key".into(),
                ),
                (
                    "material/postgres/ca-bundle.pem".into(),
                    "ca-bundle.pem".into(),
                ),
                ("material/pg_hba.conf".into(), "pg_hba.conf".into()),
                (
                    "material/postgres-bootstrap-password".into(),
                    "bootstrap-password".into(),
                ),
            ],
        ),
        "authority" => {
            let mut files = Vec::new();
            for role in [
                "migrator",
                "gateway",
                "runtime",
                "capability_worker",
                "knowledge_api",
                "knowledge_worker",
            ] {
                for name in ["dsn", "password", "tls.crt", "tls.key", "ca-bundle.pem"] {
                    files.push((
                        PathBuf::from(format!("authority/{role}/{name}")),
                        PathBuf::from(format!("{role}/{name}")),
                    ));
                }
            }
            (70, files)
        }
        "gateway" | "runtime" | "capability_worker" | "knowledge_api" | "knowledge_worker" => {
            let uid = if matches!(lane, "runtime" | "capability_worker") {
                10001
            } else {
                1000
            };
            (
                uid,
                ["dsn-control", "tls.crt", "tls.key", "ca-bundle.pem"]
                    .into_iter()
                    .map(|n| {
                        (
                            PathBuf::from(format!("material/ai_gateway_{lane}/{n}")),
                            PathBuf::from(n),
                        )
                    })
                    .collect(),
            )
        }
        "model-gateway" => (
            1000,
            vec![
                (
                    "gateway/lease-private.json".into(),
                    "lease-private.json".into(),
                ),
                (
                    "gateway/knowledge-public.json".into(),
                    "knowledge-public.json".into(),
                ),
                ("gateway-public.json".into(), "gateway-public.json".into()),
            ],
        ),
        "model-knowledge-api" | "model-knowledge-worker" => (
            1000,
            vec![
                (
                    format!("{}/workload.json", lane.trim_start_matches("model-")).into(),
                    "workload.json".into(),
                ),
                ("gateway-public.json".into(), "gateway-public.json".into()),
            ],
        ),
        "model-runtime" => (
            10001,
            vec![("gateway-public.json".into(), "gateway-public.json".into())],
        ),
        _ => return Err("unknown bounded private installation lane".into()),
    })
}
pub fn install(args: &Args) -> Result<Value> {
    require(
        args.get("--input")? == "/input" && args.get("--directory")? == "/output",
        "fixed private installation paths required",
    )?;
    let lane = args.get("--lane")?;
    let (uid, files) = lane_plan(lane)?;
    let out = Path::new("/output");
    for (source, target) in files {
        let p = out.join(target);
        if let Some(parent) = p.parent() {
            install_directory(parent, uid)?;
        }
        install_file(&Path::new("/input").join(source), &p, uid)?;
    }
    install_directory(out, uid)?;
    if lane == "qdrant-storage" {
        for directory in ["snapshots", "snapshots_temp"] {
            install_directory(&out.join(directory), uid)?;
        }
    }
    Ok(
        json!({"status":"installed","lane":lane,"owner_uid":uid,"issuer_mounted_into_service":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independently_issued_model_roles_only_receive_own_private_key() {
        let a = public(&[17; 32], "a", 100_000);
        let b = public(&[18; 32], "b", 100_000);
        assert_ne!(
            a["keys"][0]["public_key_hex"],
            b["keys"][0]["public_key_hex"]
        );
        for p in [a, b] {
            let keys: ai_platform_capability_contract::lease_v3::PublicKeyring =
                serde_json::from_value(p).unwrap();
            keys.validate().unwrap();
        }
    }
    #[test]
    fn bounded_lane_rejects_path_injection_before_reading() {
        let mut args = Args::default();
        args.values.insert("--input".into(), "/input".into());
        args.values.insert("--directory".into(), "/output".into());
        for value in ["../issuer", "gateway/../../issuer", "model-root", "all"] {
            args.values.insert("--lane".into(), value.into());
            assert_eq!(
                install(&args).unwrap_err().to_string(),
                "unknown bounded private installation lane"
            );
        }
    }
    #[test]
    fn changed_installed_generation_is_never_replaced() {
        let base =
            std::env::temp_dir().join(format!("agentforge-install-test-{}", random().unwrap()));
        directory(&base).unwrap();
        let input = base.join("input");
        let output = base.join("output");
        write(&input, b"new-key-material", 0o600).unwrap();
        write(&output, b"old-key-material", 0o600).unwrap();
        assert!(install_file(&input, &output, unsafe { libc::geteuid() }).is_err());
        assert_eq!(read(&output, true).unwrap(), b"old-key-material");
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn authority_plan_installs_all_six_original_passwords_only_for_operator() {
        let (uid, files) = lane_plan("authority").unwrap();
        assert_eq!(uid, 70);
        assert_eq!(files.len(), 30);
        for role in [
            "migrator",
            "gateway",
            "runtime",
            "capability_worker",
            "knowledge_api",
            "knowledge_worker",
        ] {
            for file in ["dsn", "password", "tls.crt", "tls.key", "ca-bundle.pem"] {
                assert!(files.contains(&(
                    PathBuf::from(format!("authority/{role}/{file}")),
                    PathBuf::from(format!("{role}/{file}"))
                )));
            }
        }
        for lane in [
            "gateway",
            "runtime",
            "capability_worker",
            "knowledge_api",
            "knowledge_worker",
        ] {
            let (_, files) = lane_plan(lane).unwrap();
            assert_eq!(files.len(), 4);
            assert!(files.iter().all(|(source, _)| {
                source.starts_with(format!("material/ai_gateway_{lane}"))
                    && source.file_name().unwrap() != "password"
            }));
        }
    }
}
