//! Shared Gateway state; no backend failure is converted to process memory.
pub mod affinity;
pub mod chaos;
pub mod context;
pub mod dashboard;
pub mod guest;
pub mod readiness;
use redis::aio::ConnectionManager;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Conflict,
    Limit,
    LeaseLost,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "ha_invalid",
            Self::Unavailable => "ha_authority_unavailable",
            Self::Conflict => "ha_conflict",
            Self::Limit => "ha_limit",
            Self::LeaseLost => "ha_owner_lost",
        })
    }
}
impl std::error::Error for Error {}
pub(crate) fn hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn bounded(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
}
pub(crate) async fn eval<T: redis::FromRedisValue>(
    connection: &ConnectionManager,
    script: &str,
    keys: &[String],
    args: &[String],
) -> Result<T, Error> {
    let mut command = redis::cmd("EVAL");
    command.arg(script).arg(keys.len());
    for key in keys {
        command.arg(key);
    }
    for arg in args {
        command.arg(arg);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        command.query_async(&mut connection.clone()),
    )
    .await
    .map_err(|_| Error::Unavailable)?
    .map_err(|_| Error::Unavailable)
}
#[cfg(test)]
mod tests;
