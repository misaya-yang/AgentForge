use super::{
    Result, database,
    models::{TrustedLocalApprovalVerifier, now, sha256},
};
use rusqlite::{Connection, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
pub type ApprovalSignatureCheck = dyn Fn(&[u8], &str) -> bool + Send + Sync;
pub struct OneUseTrustedLocalApprovalVerifier {
    device_id: String,
    verify_signature: Arc<ApprovalSignatureCheck>,
    db: Mutex<Connection>,
}
impl OneUseTrustedLocalApprovalVerifier {
    pub fn open(
        device_id: String,
        path: &Path,
        verify_signature: Arc<ApprovalSignatureCheck>,
    ) -> Result<Self> {
        if device_id.is_empty() {
            return Err(super::LocalNodeError::approval(
                "trusted local approval device id is required",
            ));
        }
        let db = database::open(path)?;
        database::lock(&db)?.execute_batch("CREATE TABLE IF NOT EXISTS consumed_local_approvals(nonce TEXT PRIMARY KEY,payload_digest TEXT NOT NULL,consumed_at REAL NOT NULL);")?;
        Ok(Self {
            device_id,
            verify_signature,
            db,
        })
    }
}
impl TrustedLocalApprovalVerifier for OneUseTrustedLocalApprovalVerifier {
    fn device_id(&self) -> &str {
        &self.device_id
    }
    fn verify_and_consume(&self, payload: &[u8], signature: &str, nonce: &str) -> Result<bool> {
        if nonce.is_empty()
            || nonce.len() > 512
            || signature.is_empty()
            || signature.len() > 8192
            || !(self.verify_signature)(payload, signature)
        {
            return Ok(false);
        }
        let mut db = database::lock(&self.db)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO consumed_local_approvals VALUES(?1,?2,?3)",
            params![nonce, sha256(payload), now()],
        )?;
        tx.commit()?;
        Ok(n == 1)
    }
}
