//! TPM2 OpenSSL provider with a fixed kernel-device TCTI. No software provider
//! fallback, simulator endpoint, environment-selected module or private-key export.
use super::*;
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const SPKI_PREFIX: &[u8] = &[
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];
fn trusted(path: &Path) -> Result<()> {
    let m = std::fs::metadata(path)
        .map_err(|_| Error::Denied("trusted TPM2 OpenSSL provider unavailable"))?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(Error::Denied("untrusted TPM2 OpenSSL input"));
    }
    Ok(())
}
fn provider() -> Result<PathBuf> {
    trusted(Path::new("/usr/bin/openssl"))?;
    if !std::fs::metadata("/dev/tpmrm0").is_ok_and(|m| m.file_type().is_char_device()) {
        return Err(Error::Denied("kernel TPM resource manager unavailable"));
    }
    for path in [
        "/usr/lib/x86_64-linux-gnu/ossl-modules/tpm2.so",
        "/usr/lib/aarch64-linux-gnu/ossl-modules/tpm2.so",
        "/usr/lib64/ossl-modules/tpm2.so",
        "/usr/lib/ossl-modules/tpm2.so",
    ] {
        let path = Path::new(path);
        if trusted(path).is_ok() {
            return Ok(path.parent().unwrap().into());
        }
    }
    Err(Error::Denied("TPM2 OpenSSL provider unavailable"))
}
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn invoke(module: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut command = Command::new("/usr/bin/openssl");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("OPENSSL_CONF", "/dev/null")
        .env("TPM2OPENSSL_TCTI", "device:/dev/tpmrm0")
        .args(args)
        .arg("-provider-path")
        .arg(module)
        .args(["-provider", "tpm2", "-provider", "default"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = ChildGuard(
        command
            .spawn()
            .map_err(|_| Error::Denied("TPM2 OpenSSL unavailable"))?,
    );
    let stdin = child.0.stdin.take().ok_or(Error::Storage)?;
    let mut stdout = child.0.stdout.take().ok_or(Error::Storage)?;
    for fd in [stdin.as_raw_fd(), stdout.as_raw_fd()] {
        let old = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if old < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, old | libc::O_NONBLOCK) } < 0 {
            return Err(Error::Storage);
        }
    }
    let mut stdin = Some(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut sent = 0;
    let mut bytes = Vec::new();
    let mut eof = false;
    let mut exited = None;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Denied("TPM2 operation timeout"));
        }
        if sent < input.len() {
            match stdin.as_mut().ok_or(Error::Storage)?.write(&input[sent..]) {
                Ok(0) => return Err(Error::Storage),
                Ok(n) => sent += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(Error::Storage),
            }
        }
        if sent == input.len() {
            drop(stdin.take());
        }
        let mut buffer = [0u8; 4096];
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.len() > 65536 {
                    return Err(Error::Protocol);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(Error::Storage),
        }
        if exited.is_none() {
            exited = child.0.try_wait().map_err(|_| Error::Storage)?;
        }
        if eof {
            if let Some(exit) = exited {
                return if exit.success() && sent == input.len() {
                    Ok(bytes)
                } else {
                    Err(Error::Denied("TPM2 operation failed"))
                };
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub struct Signer {
    module: PathBuf,
    path: PathBuf,
    public: String,
    gate: std::sync::Mutex<()>,
}
pub fn open(state: &Path, name: &str, create: bool) -> Result<Signer> {
    let module = provider()?;
    let path = state.join(format!("{name}.tss2.pem"));
    if create {
        let wrapped = invoke(
            &module,
            &[
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "group:P-256",
                "-propquery",
                "provider=tpm2",
            ],
            &[],
        )?;
        if !wrapped.starts_with(b"-----BEGIN TSS2 PRIVATE KEY-----") || wrapped.len() > 16384 {
            return Err(Error::Denied("TPM-wrapped key required"));
        }
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        file.write_all(&wrapped)?;
        file.sync_all()?;
    }
    let m = std::fs::symlink_metadata(&path)?;
    if !m.is_file()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.len() > 16384
        || !std::fs::read(&path)?.starts_with(b"-----BEGIN TSS2 PRIVATE KEY-----")
    {
        return Err(Error::Integrity);
    }
    let encoded = path.to_str().ok_or(Error::Protocol)?;
    let public = invoke(
        &module,
        &["pkey", "-in", encoded, "-pubout", "-outform", "DER"],
        &[],
    )?;
    if public.len() != 91 || !public.starts_with(SPKI_PREFIX) || public[26] != 4 {
        return Err(Error::Integrity);
    }
    Ok(Signer {
        module,
        path,
        public: URL_SAFE_NO_PAD.encode(&public[26..]),
        gate: std::sync::Mutex::new(()),
    })
}
impl PairingSigner for Signer {
    fn algorithm(&self) -> &str {
        "ES256"
    }
    fn key_class(&self) -> &str {
        "linux-tpm2-p256"
    }
    fn public_key(&self) -> &str {
        &self.public
    }
    fn sign(&self, payload: &[u8]) -> Result<String> {
        let _guard = self.gate.lock().map_err(|_| Error::Integrity)?;
        let signature = invoke(
            &self.module,
            &[
                "pkeyutl",
                "-sign",
                "-inkey",
                self.path.to_str().ok_or(Error::Protocol)?,
                "-rawin",
                "-digest",
                "sha256",
            ],
            payload,
        )?;
        if signature.len() > 80 {
            return Err(Error::Integrity);
        }
        Ok(URL_SAFE_NO_PAD.encode(signature))
    }
}
