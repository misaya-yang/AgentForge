//! Raw-body HMAC plus timestamp and delivery-id replay binding. Never log input.
use super::super::AgentError;
use hmac::{Hmac, Mac};
use sha2::Sha256;
fn denied() -> AgentError {
    AgentError::code(401, "AUTOMATION_WEBHOOK_AUTH_INVALID")
}
pub fn verify(
    secret: &str,
    timestamp: &str,
    delivery: &str,
    signature: &str,
    body: &[u8],
    now: i64,
) -> Result<(), AgentError> {
    let at = timestamp.parse::<i64>().map_err(|_| denied())?;
    if secret.len() < 32
        || at.abs_diff(now) > 300
        || delivery.is_empty()
        || delivery.len() > 128
        || !delivery
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(denied());
    }
    let hex = signature.strip_prefix("sha256=").ok_or_else(denied)?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(denied());
    }
    let bytes = (0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| denied()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| denied())?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(delivery.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(&bytes).map_err(|_| denied())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_binds_raw_body_time_and_delivery_and_rejects_expired_or_future_replay() {
        let secret = "synthetic-webhook-fixture-secret-for-tests";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"1000.delivery-1.{\"x\":1}");
        let hex = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let signature = format!("sha256={hex}");
        assert!(verify(secret, "1000", "delivery-1", &signature, b"{\"x\":1}", 1000).is_ok());
        for (at, id, body, now) in [
            ("1000", "delivery-1", b"{\"x\":2}".as_slice(), 1000),
            ("1000", "delivery-2", b"{\"x\":1}".as_slice(), 1000),
            ("1000", "delivery-1", b"{\"x\":1}".as_slice(), 1301),
            ("1000", "delivery-1", b"{\"x\":1}".as_slice(), 699),
        ] {
            assert!(verify(secret, at, id, &signature, body, now).is_err());
        }
    }
    #[test]
    fn malformed_unicode_or_short_signatures_are_rejected_without_panicking() {
        let secret = "synthetic-webhook-fixture-secret-for-tests";
        for signature in [
            "sha256=短".to_owned(),
            format!("sha256={}", "é".repeat(32)),
            "sha256=0".to_owned(),
            "not-a-signature".to_owned(),
        ] {
            assert!(verify(secret, "1000", "delivery-1", &signature, b"{}", 1000).is_err());
        }
    }
}
