//! Linux Secret Service bridge. Secrets only cross pipes to the system client.
use super::{SecureCredentialStore, validate_name};
use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct LinuxSecretServiceStore {
    service: String,
}
impl LinuxSecretServiceStore {
    pub fn new(service: String) -> Result<Self> {
        validate_name(&service)?;
        trusted_binary("/usr/bin/secret-tool")?;
        trusted_binary("/usr/bin/gdbus")?;
        Ok(Self { service })
    }
}
fn trusted_binary(path: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata =
        std::fs::metadata(path).map_err(|_| Error::Denied("Secret Service client unavailable"))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(Error::Denied("untrusted Secret Service client"));
    }
    Ok(())
}
fn invoke(binary: &str, args: &[&str], input: Vec<u8>) -> Result<(bool, Vec<u8>)> {
    trusted_binary(binary)?;
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::Denied("Secret Service unavailable"))?;
    let mut stdin = child.stdin.take().ok_or(Error::Storage)?;
    let mut stdout = child.stdout.take().ok_or(Error::Storage)?;
    use std::os::fd::AsRawFd;
    // Bound pipe operations too: a descendant holding a pipe cannot hold startup.
    for fd in [stdin.as_raw_fd(), stdout.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Storage);
        }
    }
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let reader = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(10);
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => return Ok(bytes),
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.len() > 32768 {
                        return Ok(bytes);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(e),
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let written = writer.join().map_err(|_| Error::Storage)?;
    let bytes = reader.join().map_err(|_| Error::Storage)??;
    let status = status.ok_or(Error::Denied("Secret Service timeout"))?;
    if bytes.len() > 32768 {
        return Err(Error::Denied("Secret Service response too large"));
    }
    if status.success() {
        written?;
    }
    Ok((status.success(), bytes))
}
impl SecureCredentialStore for LinuxSecretServiceStore {
    fn ready(&self) -> bool {
        invoke(
            "/usr/bin/gdbus",
            &[
                "call",
                "--session",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                "org.freedesktop.secrets",
            ],
            vec![],
        )
        .is_ok_and(|(ok, output)| ok && output == b"(true,)\n")
    }
    fn load(&self, reference: &str) -> Result<Option<Vec<u8>>> {
        validate_name(reference)?;
        if !self.ready() {
            return Err(Error::Denied("Secret Service unavailable"));
        }
        let (ok, bytes) = invoke(
            "/usr/bin/secret-tool",
            &[
                "lookup",
                "application",
                &self.service,
                "reference",
                reference,
            ],
            vec![],
        )?;
        if !ok {
            // secret-tool uses failure for a missing item; an existing identity still
            // fails closed when its referenced secret is absent.
            if bytes.is_empty() && self.ready() {
                return Ok(None);
            }
            return Err(Error::Denied("Secret Service lookup failed"));
        }
        let encoded = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Integrity)?
            .trim_end_matches('\n');
        if encoded.is_empty() {
            return Ok(None);
        }
        decode_secret(encoded).map(Some)
    }
    fn store(&self, reference: &str, secret: &[u8]) -> Result<()> {
        validate_name(reference)?;
        if secret.is_empty() || secret.len() > 8192 || !self.ready() {
            return Err(Error::Denied(
                "Secret Service unavailable or invalid secret",
            ));
        }
        let encoded = format!("af-secret-v1:{}", STANDARD.encode(secret));
        let (ok, _) = invoke(
            "/usr/bin/secret-tool",
            &[
                "store",
                "--label=AgentForge Local Node",
                "application",
                &self.service,
                "reference",
                reference,
            ],
            encoded.into_bytes(),
        )?;
        if !ok {
            return Err(Error::Denied("Secret Service store failed"));
        }
        if self.load(reference)?.as_deref() != Some(secret) {
            return Err(Error::Integrity);
        }
        Ok(())
    }
}
fn decode_secret(encoded: &str) -> Result<Vec<u8>> {
    let value = STANDARD
        .decode(
            encoded
                .strip_prefix("af-secret-v1:")
                .ok_or(Error::Integrity)?,
        )
        .map_err(|_| Error::Integrity)?;
    if value.is_empty() || value.len() > 8192 {
        return Err(Error::Integrity);
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_secret_roundtrip_and_plaintext_refusal() {
        let bytes = [0, 255, 10, 13];
        assert_eq!(
            decode_secret(&format!("af-secret-v1:{}", STANDARD.encode(bytes))).unwrap(),
            bytes
        );
        assert!(decode_secret("plaintext credential").is_err());
        assert!(decode_secret("af-secret-v1:%%%").is_err());
        assert!(decode_secret("af-secret-v1:").is_err());
    }
}
