//! Secure storage is explicit; there is no plaintext fallback.
use crate::{Error,Result};
pub const DEVICE_IDENTITY_KEYCHAIN_SERVICE:&str="ai-platform.local-node.device-identity.v1";
pub const DEVICE_CREDENTIAL_KEYCHAIN_SERVICE:&str="ai-platform.local-node.device-credential.v2";
pub trait SecureCredentialStore:Send+Sync{fn ready(&self)->bool;fn load(&self,reference:&str)->Result<Option<Vec<u8>>>;fn store(&self,reference:&str,secret:&[u8])->Result<()>;}
pub struct UnavailableStore;
impl SecureCredentialStore for UnavailableStore{fn ready(&self)->bool{false}fn load(&self,_:&str)->Result<Option<Vec<u8>>>{Err(Error::Denied("secure credential storage unavailable"))}fn store(&self,_:&str,_:&[u8])->Result<()>{Err(Error::Denied("secure credential storage unavailable"))}}
#[cfg(target_os="macos")]
pub struct MacKeychainStore{service:String}
#[cfg(target_os="macos")]
impl MacKeychainStore{pub fn new(service:String)->Result<Self>{if service.is_empty()||service.len()>512{return Err(Error::Denied("keychain service"))}Ok(Self{service})}}
#[cfg(target_os="macos")]
impl SecureCredentialStore for MacKeychainStore{
 fn ready(&self)->bool{true}
 fn load(&self,reference:&str)->Result<Option<Vec<u8>>>{match security_framework::passwords::get_generic_password(&self.service,reference){Ok(v)=>Ok(Some(v)),Err(e)if e.code()==-25300=>Ok(None),Err(_)=>Err(Error::Denied("keychain unavailable"))}}
 fn store(&self,reference:&str,secret:&[u8])->Result<()>{security_framework::passwords::set_generic_password(&self.service,reference,secret).map_err(|_|Error::Denied("keychain store failed"))}
}
