use crate::{Error,Result,sha256,credentials::SecureCredentialStore};
use serde::{Deserialize,Serialize};
use std::path::Path;
#[derive(Serialize,Deserialize)]#[serde(deny_unknown_fields)]struct Metadata{device_id:String,credential_ref:String,credential_storage:String}
pub struct DeviceIdentity{pub device_id:String,key:Vec<u8>,credential_ref:String}
impl DeviceIdentity{
 pub fn load_or_create(state:&Path,store:&dyn SecureCredentialStore)->Result<Self>{
  if !store.ready(){return Err(Error::Denied("native secure storage required"))}
  std::fs::create_dir_all(state)?;
  #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(state,std::fs::Permissions::from_mode(0o700))?;}
  let path=state.join("device-identity.json");
  if path.exists(){let info=std::fs::symlink_metadata(&path)?;if info.file_type().is_symlink()||!info.is_file()||info.len()>16384{return Err(Error::Denied("device metadata invalid"))}
   let meta:Metadata=serde_json::from_slice(&std::fs::read(path)?).map_err(|_|Error::Protocol)?;if meta.credential_storage!="ready"{return Err(Error::Denied("device storage mismatch"))}let credential_ref=meta.credential_ref;let key=store.load(&credential_ref)?.ok_or(Error::Denied("identity credential missing"))?;let id=format!("dev_{}",&sha256(&key)[..24]);if key.len()!=32||meta.device_id!=id{return Err(Error::Integrity)}return Ok(Self{device_id:id,key,credential_ref})
  }
  let mut key=vec![0u8;32];getrandom::getrandom(&mut key).map_err(|_|Error::Denied("OS entropy unavailable"))?;let id=format!("dev_{}",&sha256(&key)[..24]);let reference=format!("device_identity_{}",uuid::Uuid::new_v4().simple());store.store(&reference,&key)?;
  let meta=Metadata{device_id:id.clone(),credential_ref:reference.clone(),credential_storage:"ready".into()};let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
  #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600).custom_flags(libc::O_NOFOLLOW);}
  use std::io::Write;let mut file=options.open(path)?;file.write_all(&serde_json::to_vec(&meta).map_err(|_|Error::Protocol)?)?;file.sync_all()?;std::fs::File::open(state)?.sync_all()?;Ok(Self{device_id:id,key,credential_ref:reference})
 }
 pub fn sign(&self,payload:&[u8])->String{use hmac::{Hmac,Mac};let mut mac=Hmac::<sha2::Sha256>::new_from_slice(&self.key).expect("HMAC accepts 32-byte key");mac.update(payload);format!("{:x}",mac.finalize().into_bytes())}
 pub fn pairing_signer(&self)->Result<crate::signing::Ed25519PairingSigner>{crate::signing::Ed25519PairingSigner::from_identity_secret(&self.key)}
 pub fn credential_reference(&self)->String{format!("device_outbound_{}",self.device_id)}
 pub fn identity_reference(&self)->&str{&self.credential_ref}
}
