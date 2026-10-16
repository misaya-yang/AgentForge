//! Fail-closed, owner/origin-bound cfcred:v1 envelope; legacy reads never rewrite.
use super::{Error, Result, owner, text};
use ai_platform_gateway::{
    provider_cipher::NativeProviderCipher, provider_service::ProviderCipher,
};
use serde_json::{Value, json};
pub const MARKER: &str = "encrypted:cfcred:v1";
const PREFIX: &str = "cfcred:v1:";
pub struct Credentials {
    pub domain: String,
    pub email: String,
    pub token: String,
}
fn scope(row: &Value) -> Value {
    json!({"connection_id":row["connection_id"],"tenant_id":row["tenant_id"],"domain":row["domain"],"owner_id":owner(row)})
}
pub fn seal(row: &Value, email: &str, token: &str, key: &str) -> Result<String> {
    if key.trim().is_empty() {
        return Err(Error::unavailable(
            "Credential encryption is unavailable; check GATEWAY_ENCRYPTION_KEY",
        ));
    }
    let plain = json!({"scope":scope(row),"email":email,"api_token":token}).to_string();
    let cipher = NativeProviderCipher::new();
    let encrypted = cipher
        .encrypt(&plain, key)
        .map_err(|_| Error::unavailable("Credential encryption is unavailable"))?;
    if !encrypted.starts_with("enc:")
        || encrypted == plain
        || cipher.decrypt(&encrypted, key).ok().as_deref() != Some(&plain)
    {
        return Err(Error::unavailable("Credential encryption is unavailable"));
    }
    Ok(format!("{PREFIX}{encrypted}"))
}
pub fn open(row: &Value, key: &str) -> Result<Credentials> {
    let stored = text(row, "api_token");
    let marker = text(row, "email");
    let failure = || {
        Error::unavailable(
            "Saved credentials cannot be decrypted; check the configured encryption key",
        )
    };
    if stored.is_empty() {
        return Err(failure());
    }
    let (email, token) =
        if marker == MARKER || stored.starts_with("cfcred:") || stored.starts_with("enc:") {
            if key.trim().is_empty() || marker != MARKER || !stored.starts_with("cfcred:v1:enc:") {
                return Err(failure());
            }
            let encrypted = &stored[PREFIX.len()..];
            let plain = NativeProviderCipher::new()
                .decrypt(encrypted, key)
                .map_err(|_| failure())?;
            if plain == encrypted {
                return Err(failure());
            }
            let data: Value = serde_json::from_str(&plain).map_err(|_| failure())?;
            if data["scope"] != scope(row)
                || text(&data, "email").is_empty()
                || text(&data, "api_token").is_empty()
            {
                return Err(failure());
            }
            (
                text(&data, "email").to_owned(),
                text(&data, "api_token").to_owned(),
            )
        } else {
            if marker.is_empty() || marker.starts_with("encrypted:") {
                return Err(failure());
            }
            (marker.to_owned(), stored.to_owned())
        };
    Ok(Credentials {
        domain: text(row, "domain").into(),
        email,
        token,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_rejects_every_scope_change_key_loss_and_corrupt_marker() {
        let mut row = json!({"connection_id":"c","tenant_id":"t","owner_id":"u","domain":"example.atlassian.net","email":MARKER});
        row["api_token"] = json!(
            seal(
                &row,
                "user@example.invalid",
                "synthetic-token",
                "synthetic-key"
            )
            .unwrap()
        );
        for (changes, key) in [
            (json!({}), ""),
            (json!({}), "wrong-key"),
            (json!({"domain":"other.atlassian.net"}), "synthetic-key"),
            (json!({"tenant_id":"other"}), "synthetic-key"),
            (json!({"owner_id":"other"}), "synthetic-key"),
            (json!({"connection_id":"other"}), "synthetic-key"),
            (json!({"api_token":"plain-corruption"}), "synthetic-key"),
            (json!({"email":"user@example.invalid"}), "synthetic-key"),
            (json!({"api_token":"cfcred:v2:enc:bad"}), "synthetic-key"),
            (
                json!({"api_token":"enc:bad","email":"user@example.invalid"}),
                "synthetic-key",
            ),
        ] {
            let mut changed = row.clone();
            changed
                .as_object_mut()
                .unwrap()
                .extend(changes.as_object().unwrap().clone());
            assert!(open(&changed, key).is_err());
        }
        let mut legacy = row;
        legacy["email"] = json!("user@example.invalid");
        legacy["api_token"] = json!("synthetic-token");
        let before = legacy.clone();
        assert_eq!(open(&legacy, "").unwrap().token, "synthetic-token");
        assert_eq!(legacy, before);
    }
    #[test]
    fn encrypted_envelope_is_bound_to_owner_and_origin() {
        let mut row = json!({"connection_id":"c","tenant_id":"t","owner_id":"u","domain":"example.atlassian.net","email":MARKER});
        row["api_token"] =
            json!(seal(&row, "user@example.invalid", "test-token", "test-key").unwrap());
        assert_eq!(
            open(&row, "test-key").unwrap().email,
            "user@example.invalid"
        );
        row["owner_id"] = json!("different-owner");
        assert!(open(&row, "test-key").is_err());
    }
    #[test]
    fn no_fail_open_or_implicit_legacy_writes() {
        let row = json!({"domain":"example.atlassian.net","email":"user@example.invalid","api_token":"legacy"});
        assert_eq!(open(&row, "").unwrap().token, "legacy");
        assert!(seal(&row, "e", "t", "").is_err());
        let mut corrupt = row.clone();
        corrupt["email"] = json!(MARKER);
        assert!(open(&corrupt, "key").is_err());
    }
}
