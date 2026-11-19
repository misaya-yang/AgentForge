//! Inert OS entropy, domain-bound token digests, and injectable unbiased shuffling.
use crate::{
    quiz_grading::{python_trim, unicode_member},
    quiz_types::QuizError,
    quiz_unicode,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;

pub trait QuizEntropy: Send + Sync {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), QuizError>;
}
/// Construction performs no IO. Linux OS CSPRNG is read only upon explicit use.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsQuizEntropy;
impl QuizEntropy for OsQuizEntropy {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), QuizError> {
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(output))
            .map_err(|_| QuizError::Internal)
    }
}
fn input(message: &str) -> QuizError {
    QuizError::detail(
        400,
        json!({"code":"attempt_input_invalid","message":message}),
    )
}
pub fn issue_token(entropy: &dyn QuizEntropy) -> Result<String, QuizError> {
    let mut bytes = [0u8; 32];
    entropy.fill_bytes(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub fn token_hash(token: &str, audience: &str, user_id: Option<&str>) -> Result<String, QuizError> {
    let mut digest = Sha256::new();
    if audience == "internal" {
        let user = user_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| input("A signed-in account is required for this attempt"))?;
        digest.update(b"internal:");
        digest.update(user.as_bytes());
        digest.update([0u8]);
    }
    digest.update(token.as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}
fn below(bound: u64, entropy: &dyn QuizEntropy) -> Result<u64, QuizError> {
    // The accepted interval has size divisible by bound, removing modulo bias.
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let mut bytes = [0u8; 8];
        entropy.fill_bytes(&mut bytes)?;
        let value = u64::from_le_bytes(bytes);
        if value >= threshold {
            return Ok(value % bound);
        }
    }
}
pub fn shuffle(options: &mut [Value], entropy: &dyn QuizEntropy) -> Result<(), QuizError> {
    for i in (1..options.len()).rev() {
        let j = below((i + 1) as u64, entropy)? as usize;
        options.swap(i, j);
    }
    Ok(())
}
/// Caller supplies a cloned public payload. Labels stay attached to their text.
pub fn shuffle_options(payload: &mut Value, entropy: &dyn QuizEntropy) -> Result<(), QuizError> {
    if let Some(questions) = payload.get_mut("questions").and_then(Value::as_array_mut) {
        for question in questions {
            if let Some(options) = question.get_mut("options").and_then(Value::as_array_mut) {
                shuffle(options, entropy)?;
            }
        }
    }
    Ok(())
}
pub fn sanitize_display_name(name: Option<&str>) -> Result<Option<String>, QuizError> {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let stripped = python_trim(name);
    if stripped.chars().count() > 100 {
        return Err(input("Display name is too long"));
    }
    if stripped.is_empty() || !stripped.chars().all(|c| {
        unicode_member(c, quiz_unicode::ALNUM) || unicode_member(c, quiz_unicode::SPACE)
            || matches!(c, '_' | '.' | '-' | '@' | '\u{4e00}'..='\u{9fff}' | '\u{0600}'..='\u{06ff}')
    }) { return Err(input("Display name contains unsupported characters")); }
    // html.escape is currently an identity over the accepted alphabet, retained explicitly.
    Ok(Some(
        stripped
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#x27;"),
    ))
}
