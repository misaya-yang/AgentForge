//! Representation upgrade only: preserve password revision and active sessions.
// SPDX-License-Identifier: Apache-2.0
use crate::accounts::{PgAccounts, StoreError};
impl PgAccounts {
    pub async fn rehash_password(
        &self,
        user: &str,
        tenant: &str,
        old: &str,
        new: &str,
    ) -> Result<(), StoreError> {
        if !old.starts_with("$2") || !new.starts_with("$argon2id$") {
            return Err(StoreError::InvalidInput);
        }
        let changed=sqlx::query("UPDATE gateway.users SET password_hash=$4 WHERE user_id=$1 AND tenant_id=$2 AND password_hash=$3 AND status='active' AND (locked_until IS NULL OR locked_until<=NOW())")
            .bind(user).bind(tenant).bind(old).bind(new).execute(&self.pool).await.map_err(|_|StoreError::Unavailable)?.rows_affected();
        if changed != 1 {
            return Err(StoreError::Changed);
        }
        Ok(())
    }
}
