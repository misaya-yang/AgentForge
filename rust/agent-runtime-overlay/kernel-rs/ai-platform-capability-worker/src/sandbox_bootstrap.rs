//! Control-only sandbox bootstrap. Reads one private seed file, never logs or
//! forwards its bytes, and has no fallback to an in-process Python executor.
use ai_platform_capability_contract::sandbox_job::SandboxJobSigner;
use ai_platform_capability_worker::sandbox_transport::RemoteSandboxBroker;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

pub fn configure(
    socket: Option<&Path>,
    key_id: Option<&str>,
    key_file: Option<&Path>,
) -> anyhow::Result<Option<Arc<RemoteSandboxBroker>>> {
    let (socket, key_id, key_file) = match (socket, key_id, key_file) {
        (None, None, None) => return Ok(None),
        (Some(socket), Some(key_id), Some(key_file))
            if socket.is_absolute() && key_file.is_absolute() && !key_id.is_empty() =>
        {
            (socket, key_id, key_file)
        }
        _ => anyhow::bail!(
            "sandbox socket, verifier key id and private signing-key file must be selected together"
        ),
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(key_file)
        .map_err(|_| anyhow::anyhow!("sandbox signing-key file is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("sandbox signing-key file metadata is unavailable"))?;
    if !metadata.is_file() || metadata.len() != 32 {
        anyhow::bail!("sandbox signing-key file must contain exactly 32 raw bytes");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!(
                "sandbox signing-key file must be private and owned by the control worker user"
            );
        }
    }
    let mut seed = [0u8; 32];
    file.read_exact(&mut seed)
        .map_err(|_| anyhow::anyhow!("sandbox signing-key file read failed"))?;
    let signer = Arc::new(SandboxJobSigner::new(key_id.into(), seed).map_err(anyhow::Error::msg)?);
    seed.fill(0);
    Ok(Some(Arc::new(RemoteSandboxBroker::new(
        socket.into(),
        signer,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_or_relative_configuration_fails_before_key_access() {
        assert!(configure(None, None, None).unwrap().is_none());
        assert!(
            configure(
                Some(Path::new("relative")),
                Some("control"),
                Some(Path::new("/unused"))
            )
            .is_err()
        );
        assert!(configure(Some(Path::new("/socket")), None, None).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn key_file_must_be_exact_private_and_non_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root =
            std::env::temp_dir().join(format!("agentforge-key-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let key = root.join("key");
        std::fs::write(&key, [19u8; 32]).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(configure(Some(Path::new("/socket")), Some("control"), Some(&key)).is_ok());
        let link = root.join("link");
        symlink(&key, &link).unwrap();
        assert!(configure(Some(Path::new("/socket")), Some("control"), Some(&link)).is_err());
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(configure(Some(Path::new("/socket")), Some("control"), Some(&key)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
