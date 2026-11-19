use super::{
    LocalNodeError as Error, Result,
    credentials::{CredentialStorageStatus, SecureCredentialStore},
    database,
    models::*,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub fn random_bytes(length: usize) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        let mut bytes = vec![0; length];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = length;
        Err(Error::new(
            "driver_unavailable",
            "OS entropy adapter unavailable",
        ))
    }
}
pub fn random_token(length: usize) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(length)?))
}
pub struct DeviceIdentity {
    pub device_id: String,
    key: Vec<u8>,
    pub metadata_path: PathBuf,
    pub credential_storage: CredentialStorageStatus,
}
impl DeviceIdentity {
    pub fn load_or_create(directory: &Path, store: &dyn SecureCredentialStore) -> Result<Self> {
        if store.status() == CredentialStorageStatus::Unavailable {
            return Err(Error::new(
                "pairing_error",
                "native secure credential storage unavailable; pairing disabled",
            ));
        }
        database::private_directory(directory)?;
        let path = directory.join("device-identity.json");
        if path.exists() {
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(Error::new(
                    "pairing_error",
                    "identity metadata cannot be a symlink",
                ));
            }
            let value: Value = serde_json::from_reader(File::open(&path)?)?;
            if value.get("key").is_some()
                || value["credential_storage"] != serde_json::to_value(store.status())?
            {
                return Err(Error::new(
                    "pairing_error",
                    "legacy plaintext identity or mismatched storage rejected",
                ));
            }
            let reference = value["credential_ref"]
                .as_str()
                .ok_or_else(|| Error::new("pairing_error", "credential reference invalid"))?;
            let key = store
                .load(reference)?
                .filter(|k| k.len() == 32)
                .ok_or_else(|| {
                    Error::new("pairing_error", "identity key unavailable or invalid")
                })?;
            let id = format!("dev_{}", &sha256(&key)[..24]);
            if value["device_id"] != id {
                return Err(Error::new(
                    "pairing_error",
                    "identity metadata does not match credential",
                ));
            }
            return Ok(Self {
                device_id: id,
                key,
                metadata_path: path,
                credential_storage: store.status(),
            });
        }
        let key = random_bytes(32)?;
        let id = format!("dev_{}", &sha256(&key)[..24]);
        let reference = format!("device_identity_{}", random_token(18)?);
        store.store(&reference, &key)?;
        let payload =
            json!({"device_id":id,"credential_ref":reference,"credential_storage":store.status()});
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&path)?;
        file.write_all(canonical_json(&payload).as_bytes())?;
        file.sync_all()?;
        Ok(Self {
            device_id: id,
            key,
            metadata_path: path,
            credential_storage: store.status(),
        })
    }
    pub fn sign(&self, payload: &[u8]) -> Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key)
            .map_err(|_| Error::new("pairing_error", "identity signer unavailable"))?;
        mac.update(payload);
        Ok(mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceCredential {
    pub device_id: String,
    pub credential: String,
    pub expires_at: f64,
}
pub struct PairingChallenge {
    pub challenge_id: String,
    pub secret: String,
    pub expires_at: f64,
}
pub struct PairingManager {
    pub identity: Arc<DeviceIdentity>,
    pub max_ttl_seconds: u64,
    challenges: Mutex<HashMap<String, (String, f64)>>,
}
impl PairingManager {
    pub fn new(identity: Arc<DeviceIdentity>) -> Self {
        Self {
            identity,
            max_ttl_seconds: 300,
            challenges: Mutex::new(HashMap::new()),
        }
    }
    pub fn issue(&self, ttl: u64) -> Result<PairingChallenge> {
        if ttl == 0 || ttl > self.max_ttl_seconds {
            return Err(Error::new("pairing_error", "invalid challenge lifetime"));
        }
        let challenge = PairingChallenge {
            challenge_id: format!("pair_{}", random_token(12)?),
            secret: random_token(32)?,
            expires_at: now() + ttl as f64,
        };
        self.challenges
            .lock()
            .map_err(|_| Error::new("pairing_error", "pairing state unavailable"))?
            .insert(
                challenge.challenge_id.clone(),
                (sha256(challenge.secret.as_bytes()), challenge.expires_at),
            );
        Ok(challenge)
    }
    pub fn redeem(&self, id: &str, secret: &str, ttl: u64) -> Result<DeviceCredential> {
        let (expected, expires) = self
            .challenges
            .lock()
            .map_err(|_| Error::new("pairing_error", "pairing state unavailable"))?
            .remove(id)
            .ok_or_else(|| Error::new("pairing_error", "challenge missing or already consumed"))?;
        if now() >= expires
            || !constant_time_equal(expected.as_bytes(), sha256(secret.as_bytes()).as_bytes())
        {
            return Err(Error::new("pairing_error", "challenge invalid or expired"));
        }
        let expiry = now() + ttl as f64;
        let body = format!(
            "{}.{}.{}",
            self.identity.device_id,
            expiry as u64,
            random_token(18)?
        );
        Ok(DeviceCredential {
            device_id: self.identity.device_id.clone(),
            credential: format!("{body}.{}", self.identity.sign(body.as_bytes())?),
            expires_at: expiry,
        })
    }
    pub fn validate_credential(&self, credential: &DeviceCredential) -> bool {
        if credential.device_id != self.identity.device_id || now() >= credential.expires_at {
            return false;
        }
        let Some((body, signature)) = credential.credential.rsplit_once('.') else {
            return false;
        };
        let parts: Vec<_> = body.split('.').collect();
        if parts.len() != 3 || parts[0] != self.identity.device_id {
            return false;
        }
        let Ok(expiry) = parts[1].parse::<f64>() else {
            return false;
        };
        now() < expiry
            && (expiry - credential.expires_at).abs() <= 1.0
            && self
                .identity
                .sign(body.as_bytes())
                .is_ok_and(|s| constant_time_equal(s.as_bytes(), signature.as_bytes()))
    }
}
