//! Durable one-use approval gate; native signature verification stays independently injected.
use crate::{Result,sha256,envelope::ApprovalVerifier};
use rusqlite::{Connection,params};
use std::{path::Path,sync::Mutex};
pub struct OneUseApproval<F:Fn(&[u8],&str)->bool+Send+Sync>{device:String,db:Mutex<Connection>,verify:F}
impl<F:Fn(&[u8],&str)->bool+Send+Sync> OneUseApproval<F>{
 pub fn open(device:String,path:&Path,verify:F)->Result<Self>{if device.is_empty(){return Err(crate::Error::Denied("device id required"))}let db=Connection::open(path)?;
 #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600))?;}
 db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS consumed_local_approvals(nonce TEXT PRIMARY KEY,payload_digest TEXT NOT NULL,consumed_at REAL NOT NULL);")?;Ok(Self{device,db:Mutex::new(db),verify})}
}
impl<F:Fn(&[u8],&str)->bool+Send+Sync> ApprovalVerifier for OneUseApproval<F>{
 fn device_id(&self)->&str{&self.device}
 fn verify_and_consume(&self,payload:&[u8],signature:&str,nonce:&str)->bool{
  if nonce.is_empty()||nonce.len()>512||signature.is_empty()||signature.len()>8192||!(self.verify)(payload,signature){return false}
  let Ok(db)=self.db.lock() else{return false};
  db.execute("INSERT INTO consumed_local_approvals VALUES(?,?,?)",params![nonce,sha256(payload),crate::now()]).is_ok()
 }
}
