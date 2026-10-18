//! Native local capability boundary. No listener or ambient filesystem authority.
pub mod computer;
pub mod config;
#[cfg(unix)]
pub mod macos;
pub mod credentials;
pub mod credential_lock;
pub mod signing;
pub mod identity;
pub mod hardware_key;
pub mod approvals;
pub mod envelope;
pub mod ledger;
pub mod outbox;
pub mod transport;
pub mod control_stream;
pub mod runner;
#[cfg(unix)]
pub mod workspace;
#[cfg(unix)]
pub mod files;
#[cfg(unix)]
pub mod processes;
#[cfg(unix)]
pub mod file_dispatch;
#[cfg(unix)]
pub mod watcher;
#[cfg(unix)]
pub mod service;
use serde_json::Value;
use unicode_casefold::UnicodeCaseFold;
use sha2::{Digest, Sha256};
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("local capability denied: {0}")]
    Denied(&'static str),
    #[error("action identity or terminal result conflict")]
    Conflict,
    #[error("local evidence integrity failure")]
    Integrity,
    #[error("local storage unavailable")]
    Storage,
    #[error("local target changed")]
    Stale,
    #[error("outbound transport unavailable")]
    Transport,
    #[error("invalid control-plane protocol")]
    Protocol,
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<rusqlite::Error> for Error { fn from(_: rusqlite::Error) -> Self { Self::Storage } }
impl From<std::io::Error> for Error { fn from(_: std::io::Error) -> Self { Self::Storage } }
pub fn now() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64() }
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(o) => { let mut keys: Vec<_> = o.keys().collect(); keys.sort(); format!("{{{}}}", keys.into_iter().map(|k| format!("{}:{}", serde_json::to_string(k).unwrap(), canonical(&o[k]))).collect::<Vec<_>>().join(",")) },
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        Value::Number(n) if n.is_f64() => python_float(n.as_f64().expect("finite JSON number")),
        _ => value.to_string(),
    }
}
pub fn sha256(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
pub fn digest(value: &Value) -> String { sha256(canonical(value).as_bytes()) }
pub fn sanitize(value: &Value) -> Value {
    match value {
        Value::Object(o) => Value::Object(o.iter().map(|(k,v)| { let low=k.chars().case_fold().collect::<String>(); let sensitive=["secret","token","password","passwd","credential","authorization","cookie","private_key","error_detail"].iter().any(|s|low.contains(s)) || ["stdout","stderr","content","raw_content"].contains(&low.as_str()); (k.clone(), if sensitive { Value::String("[REDACTED]".into()) } else { sanitize(v) }) }).collect()),
        Value::Array(a) => Value::Array(a.iter().map(sanitize).collect()),
        Value::String(s) if ["-----begin private key-----","-----begin rsa private key-----","bearer ","sk-","ghp_","secret_canary","secret-canary"].iter().any(|m|s.chars().case_fold().collect::<String>().contains(m)) => Value::String("[REDACTED]".into()),
        _ => value.clone(),
    }
}

// Same canonical float normalization used by the native gateway catalog.
pub fn python_float(value: f64) -> String {
    let raw = serde_json::to_string(&value).expect("JSON values contain finite numbers");
    let (sign, unsigned) = if let Some(value) = raw.strip_prefix('-') {
        ("-", value)
    } else {
        ("", raw.as_str())
    };
    let (mantissa, exponent) = unsigned.split_once('e').map_or((unsigned, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("serializer exponent"))
    });
    let decimal_at = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let zeros = digits.bytes().take_while(|c| *c == b'0').count();
    if zeros == digits.len() {
        return format!("{sign}0.0");
    }
    let scientific_exponent = exponent + decimal_at - zeros as i32 - 1;
    digits.drain(..zeros);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    if !(-4..16).contains(&scientific_exponent) {
        let fraction = if digits.len() == 1 {
            String::new()
        } else {
            format!(".{}", &digits[1..])
        };
        return format!("{sign}{}{fraction}e{scientific_exponent:+03}", &digits[..1]);
    }
    let decimal = scientific_exponent + 1;
    if decimal <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-decimal) as usize), digits)
    } else if decimal as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(decimal as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..decimal as usize],
            &digits[decimal as usize..]
        )
    }
}

#[cfg(unix)]
pub mod acceptance;

#[cfg(unix)]
pub mod macos_acceptance;
