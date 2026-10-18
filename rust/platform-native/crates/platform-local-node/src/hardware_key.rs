//! Explicit non-exportable P-256 device keys. Hardware-required selection never
//! silently downgrades to a vault software seed. Hardware class is local evidence,
//! not remote attestation; Gateway policy must independently admit registration.
use crate::{Error, Result, transport::PairingSigner};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKeyPolicy {
    #[default]
    Software,
    HardwareRequired,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u32,
    name: String,
    key_class: String,
    public_key: String,
}
pub fn select(
    policy: DeviceKeyPolicy,
    state: &Path,
    identity: &crate::identity::DeviceIdentity,
) -> Result<Arc<dyn PairingSigner>> {
    if policy == DeviceKeyPolicy::Software {
        return Ok(Arc::new(identity.pairing_signer()?));
    }
    let path = state.join("hardware-key.json");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(m) => {
            if !m.is_file() || m.file_type().is_symlink() || m.len() > 4096 {
                return Err(Error::Integrity);
            }
            let metadata: Metadata =
                serde_json::from_slice(&std::fs::read(&path)?).map_err(|_| Error::Integrity)?;
            if metadata.version != 1
                || !metadata.name.starts_with("agentforge-")
                || metadata.name.len() > 100
                || !metadata
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(Error::Integrity);
            }
            Some(metadata)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(Error::Storage),
    };
    let name = metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| format!("agentforge-{}", uuid::Uuid::new_v4()));
    #[cfg(target_os = "macos")]
    let signer: Arc<dyn PairingSigner> = Arc::new(macos::open(&name, metadata.is_none())?);
    #[cfg(target_os = "windows")]
    let signer: Arc<dyn PairingSigner> = Arc::new(windows::open(&name, metadata.is_none())?);
    #[cfg(target_os = "linux")]
    let signer: Arc<dyn PairingSigner> = Arc::new(linux::open(state, &name, metadata.is_none())?);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    return Err(Error::Denied("hardware device key unavailable"));
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let public = URL_SAFE_NO_PAD
            .decode(signer.public_key())
            .map_err(|_| Error::Integrity)?;
        if public.len() != 65 || public[0] != 4 {
            return Err(Error::Integrity);
        }
        if let Some(metadata) = metadata {
            if metadata.key_class != signer.key_class()
                || metadata.public_key != signer.public_key()
            {
                return Err(Error::Integrity);
            }
        } else {
            let value = Metadata {
                version: 1,
                name,
                key_class: signer.key_class().into(),
                public_key: signer.public_key().into(),
            };
            let mut opts = std::fs::OpenOptions::new();
            opts.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            use std::io::Write;
            let mut file = opts.open(&path)?;
            file.write_all(&serde_json::to_vec(&value).map_err(|_| Error::Protocol)?)?;
            file.sync_all()?;
        }
        // Verify backend signatures before using the key for pairing or requests.
        let payload = b"ai-platform.local-node.hardware-key.self-test.v1";
        let signature = URL_SAFE_NO_PAD
            .decode(signer.sign(payload)?)
            .map_err(|_| Error::Integrity)?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, public)
            .verify(payload, &signature)
            .map_err(|_| Error::Integrity)?;
        Ok(signer)
    }
}
#[cfg(any(target_os = "windows", test))]
fn raw_signature_to_der(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() != 64 {
        return Err(Error::Integrity);
    }
    let integer = |bytes: &[u8]| {
        let first = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        let mut out = bytes[first..].to_vec();
        if out[0] & 0x80 != 0 {
            out.insert(0, 0);
        }
        let mut value = vec![2, out.len() as u8];
        value.extend(out);
        value
    };
    let mut body = integer(&raw[..32]);
    body.extend(integer(&raw[32..]));
    let mut output = vec![0x30, body.len() as u8];
    output.extend(body);
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn p256_signature_encoding_is_minimal_and_bounded() {
        let mut raw = [0u8; 64];
        raw[0] = 0x80;
        raw[63] = 1;
        let der = raw_signature_to_der(&raw).unwrap();
        assert_eq!(&der[..5], &[0x30, 38, 2, 33, 0]);
        assert_eq!(&der[der.len() - 3..], &[2, 1, 1]);
        assert!(raw_signature_to_der(&raw[..63]).is_err());
        assert_eq!(
            serde_json::from_str::<DeviceKeyPolicy>("\"hardware_required\"").unwrap(),
            DeviceKeyPolicy::HardwareRequired
        );
        assert!(
            serde_json::from_str::<DeviceKeyPolicy>("\"prefer_hardware_then_plaintext\"").is_err()
        );
    }
}
