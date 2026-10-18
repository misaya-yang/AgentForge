use base64::Engine;
const COMMANDS: &[&str] = &[
    "configure_gateway",
    "desktop_status",
    "open_console",
    "begin_login",
    "finish_login",
    "logout",
    "native_identity",
    "pair_node",
    "node_status",
    "check_update",
    "install_update",
];
fn main() {
    println!("cargo:rerun-if-env-changed=AGENTFORGE_DESKTOP_RELEASE_POLICY");
    let path = std::env::var_os("AGENTFORGE_DESKTOP_RELEASE_POLICY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "release-policy.json".into());
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = std::fs::symlink_metadata(&path).expect("public release policy unavailable");
    assert!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 16384,
        "public release policy must be a bounded regular file"
    );
    let bytes = std::fs::read(&path).expect("public release policy unavailable");
    let config: serde_json::Value =
        serde_json::from_slice(&bytes).expect("invalid public release policy");
    let allowed = [
        "enabled",
        "channel",
        "policy_url",
        "download_origin",
        "policy_public_key",
        "updater_public_key",
    ];
    let object = config
        .as_object()
        .expect("public release policy must be an object");
    assert!(
        object.len() == allowed.len() && object.keys().all(|key| allowed.contains(&key.as_str())),
        "unknown release policy field; private signing data must never be embedded"
    );
    if config["enabled"].as_bool() == Some(true) {
        let key = config["policy_public_key"].as_str().unwrap_or_default();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(key)
                .expect("invalid policy public key")
                .len(),
            32,
            "Ed25519 public policy key required"
        );
        assert!(
            !config["updater_public_key"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty(),
            "updater public key required"
        );
        let origin = url::Url::parse(config["download_origin"].as_str().unwrap_or_default())
            .expect("invalid update origin");
        let policy = url::Url::parse(config["policy_url"].as_str().unwrap_or_default())
            .expect("invalid policy URL");
        assert!(
            origin.scheme() == "https"
                && origin.host_str().is_some()
                && origin.path() == "/"
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "pinned HTTPS update origin required"
        );
        assert!(
            policy.origin() == origin.origin()
                && policy.username().is_empty()
                && policy.password().is_none()
                && policy.query().is_none()
                && policy.fragment().is_none(),
            "policy outside pinned update origin"
        );
        assert!(
            matches!(config["channel"].as_str(), Some("stable" | "beta")),
            "release channel required"
        );
    } else {
        assert_eq!(
            config["enabled"].as_bool(),
            Some(false),
            "explicit updater enabled flag required"
        );
    }
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("release-policy.json"),
        bytes,
    )
    .expect("cannot stage public release policy");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("desktop ACL/configuration generation failed");
}
