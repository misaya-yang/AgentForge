//! Closed service identities and existing key inventories. No all-commands or
//! all-keys grants; adding a new coordination namespace requires explicit review.
use super::{Instance, ValkeyError};
use std::collections::{BTreeMap, BTreeSet};
pub const USERS: &[&str] = &[
    "gateway-coord",
    "knowledge-api-coord",
    "knowledge-worker-coord",
    "coord-health",
    "gateway-cache",
    "knowledge-api-cache",
    "knowledge-worker-cache",
    "cache-health",
];
const HANDSHAKE: &str = "+ping +hello +info +client|info +client|setname +client|setinfo";
const COORD_COMMANDS:&str="+scan +time +type +get +set +setex +psetex +getdel +del +unlink +exists +expire +pexpire +expireat +pexpireat +ttl +pttl +incr +incrby +decr +decrby +sadd +srem +smembers +scard +sismember +hget +hset +hmget +hmset +hgetall +hdel +hincrby +hexists +zadd +zrem +zremrangebyscore +zremrangebyrank +zcard +zcount +zrange +zrangebyscore +zscore +zpopmin +eval +evalsha +script|load +script|exists +multi +exec +discard";
const GATEWAY_KEYS: &str =
    "~auth:* ~gateway:* ~ratelimit:* ~oauth:* ~guest:* ~guest_session:* ~ai-gateway:* ~quota:* ~usage:* ~mq:* ~queue:* ~config:auth ~metrics:* ~events:* ~agent:channel:* ~lg:quota:*";
const KNOWLEDGE_KEYS: &str = "~ai-gateway:internal:replay:* ~ai-gateway:internal:idem:*";
/// Hashes are SHA256 of the separate high-entropy ACL password files. Passwords
/// and ACL-file values must never be included in diagnostics or return summaries.
pub fn render(
    instance: Instance,
    hashes: &BTreeMap<String, String>,
) -> Result<String, ValkeyError> {
    if hashes.keys().map(String::as_str).collect::<BTreeSet<_>>() != USERS.iter().copied().collect()
        || hashes.values().any(|h| {
            h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || hashes.values().collect::<BTreeSet<_>>().len() != USERS.len()
    {
        return Err(ValkeyError::Configuration);
    }
    let mut lines = vec!["user default reset off".to_owned()];
    for &user in USERS {
        if !user.ends_with(instance.name()) && user != format!("{}-health", instance.name()) {
            continue;
        }
        let grants = if user.ends_with("-health") {
            HANDSHAKE.to_owned()
        } else if instance == Instance::Cache {
            format!(
                "{HANDSHAKE} +getrange +set +del ~af-cache:{}:*",
                user.trim_end_matches("-cache")
            )
        } else if user == "gateway-coord" {
            format!("{HANDSHAKE} {COORD_COMMANDS} +publish +subscribe +unsubscribe +xadd +xread +xreadgroup +xgroup|create +xgroup|destroy +xgroup|setid +xgroup|delconsumer +xgroup|createconsumer +xack +xpending +xclaim +xautoclaim +xrange +xrevrange +xlen +xinfo|groups +xtrim +xdel {GATEWAY_KEYS} &gateway:* &ai-gateway:*")
        } else {
            format!("{HANDSHAKE} +get +set +setex +del +exists +expire +eval +evalsha +script|load +script|exists {KNOWLEDGE_KEYS}")
        };
        lines.push(format!(
            "user {user} reset on #{} -@all resetkeys resetchannels {grants}",
            hashes[user]
        ));
    }
    Ok(lines.join("\n") + "\n")
}

#[cfg(test)]
pub(crate) fn gateway_key_allowed(key: &str) -> bool {
    GATEWAY_KEYS.split_whitespace().any(|p| {
        let p = p.trim_start_matches('~');
        match p.strip_suffix('*') {
            Some(prefix) => key.starts_with(prefix),
            None => key == p,
        }
    })
}
