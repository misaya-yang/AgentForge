//! Offline minisign raw-Ed25519 profile. No trust-on-first-use, subprocess,
//! network/key lookup, key embedded by a package, or executable/WASM loading.
//! `Ed` is minisign's legacy raw-message mode (`minisign -l`); unsupported `ED`
//! prehash packets are rejected rather than interpreted as another algorithm.
use super::{PackError, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
fn decoded(text: &str, len: usize) -> Result<Vec<u8>> {
    let bytes = STANDARD.decode(text).map_err(|_| PackError::Untrusted)?;
    if bytes.len() != len || STANDARD.encode(&bytes) != text {
        return Err(PackError::Untrusted);
    }
    Ok(bytes)
}
pub fn public_key(text: &str) -> Result<(VerifyingKey, [u8; 8])> {
    if text.len() > 2048 || text.contains(['\r', '\0']) {
        return Err(PackError::Untrusted);
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() != 2 || !lines[0].starts_with("untrusted comment: ") {
        return Err(PackError::Untrusted);
    }
    let packet = decoded(lines[1], 42)?;
    if &packet[..2] != b"Ed" {
        return Err(PackError::Untrusted);
    }
    let id = packet[2..10].try_into().map_err(|_| PackError::Untrusted)?;
    let key = VerifyingKey::from_bytes(&packet[10..].try_into().map_err(|_| PackError::Untrusted)?)
        .map_err(|_| PackError::Untrusted)?;
    if key.is_weak() {
        return Err(PackError::Untrusted);
    }
    Ok((key, id))
}
pub fn verify(public: &str, signature: &str, artifact: &[u8]) -> Result<()> {
    let (key, id) = public_key(public)?;
    if signature.len() > 8192 || signature.contains(['\r', '\0']) {
        return Err(PackError::Untrusted);
    }
    let lines = signature.lines().collect::<Vec<_>>();
    if lines.len() != 4 || !lines[0].starts_with("untrusted comment: ") {
        return Err(PackError::Untrusted);
    }
    let comment = lines[2]
        .strip_prefix("trusted comment: ")
        .ok_or(PackError::Untrusted)?;
    if comment.is_empty() || comment.len() > 2048 {
        return Err(PackError::Untrusted);
    }
    let packet = decoded(lines[1], 74)?;
    if &packet[..2] != b"Ed" || packet[2..10] != id {
        return Err(PackError::Untrusted);
    }
    let raw = Signature::from_slice(&packet[10..]).map_err(|_| PackError::Untrusted)?;
    key.verify_strict(artifact, &raw)
        .map_err(|_| PackError::Untrusted)?;
    let global =
        Signature::from_slice(&decoded(lines[3], 64)?).map_err(|_| PackError::Untrusted)?;
    let mut payload = packet[10..].to_vec();
    payload.extend(comment.as_bytes());
    key.verify_strict(&payload, &global)
        .map_err(|_| PackError::Untrusted)
}
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    pub(super) fn signed(bytes: &[u8], seed: u8) -> (String, String) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = [seed; 8];
        let raw = key.sign(bytes);
        let mut pubpacket = b"Ed".to_vec();
        pubpacket.extend(id);
        pubpacket.extend(key.verifying_key().as_bytes());
        let mut packet = b"Ed".to_vec();
        packet.extend(id);
        packet.extend(raw.to_bytes());
        let comment = "timestamp:1700000000 file:fixture.tar";
        let mut global = raw.to_bytes().to_vec();
        global.extend(comment.as_bytes());
        (
            format!(
                "untrusted comment: fixture publisher\n{}\n",
                STANDARD.encode(pubpacket)
            ),
            format!(
                "untrusted comment: signature from fixture\n{}\ntrusted comment: {comment}\n{}\n",
                STANDARD.encode(packet),
                STANDARD.encode(key.sign(&global).to_bytes())
            ),
        )
    }
    #[test]
    fn minisign_verifies_primary_and_authenticated_comment_with_pinned_key() {
        let (key, sig) = signed(b"reviewed archive", 7);
        verify(&key, &sig, b"reviewed archive").unwrap();
        assert!(verify(&key, &sig, b"modified archive").is_err());
        assert!(verify(&signed(b"", 8).0, &sig, b"reviewed archive").is_err());
        assert!(
            verify(
                &key,
                &sig.replace("timestamp:1700000000", "timestamp:1800000000"),
                b"reviewed archive"
            )
            .is_err()
        );
        assert!(
            verify(
                &key,
                &sig.replace("trusted comment: ", "untrusted comment: "),
                b"reviewed archive"
            )
            .is_err()
        );
        assert!(verify(&key, &format!("{sig}appended\n"), b"reviewed archive").is_err());
    }
    #[test]
    fn refuses_algorithm_alias_missing_signature_wrong_id_and_noncanonical_encoding() {
        let (key, sig) = signed(b"archive", 9);
        assert!(verify(&key, "", b"archive").is_err());
        let mut lines = sig.lines().map(str::to_owned).collect::<Vec<_>>();
        let mut packet = STANDARD.decode(&lines[1]).unwrap();
        packet[..2].copy_from_slice(b"ED");
        lines[1] = STANDARD.encode(packet);
        assert!(verify(&key, &lines.join("\n"), b"archive").is_err());
        assert!(public_key(&key.replace("untrusted comment:", "trusted comment:")).is_err());
        assert!(verify(&key, &sig.replace('\n', "\r\n"), b"archive").is_err());
    }
}
