//! SDK provenance is explicitly instance-affine: original user JWTs never move
//! to Valkey. Only bounded routing hints and liveness are shared, never authority.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug)]
pub struct InstanceIdentity {
    instance: String,
    boot: String,
}
impl InstanceIdentity {
    pub fn new(instance: &str) -> Result<Self, Error> {
        if instance.is_empty()
            || instance.len() > 128
            || !instance
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            instance: hash(instance)[..8].into(),
            boot: uuid::Uuid::new_v4().simple().to_string()[..8].into(),
        })
    }
    pub fn prefix(&self) -> String {
        format!("{}{}", self.instance, self.boot)
    }
    pub fn token(&self) -> String {
        format!(
            "{}{}{}",
            self.prefix(),
            uuid::Uuid::new_v4().simple(),
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        )
    }
    pub fn owns(&self, token: &str) -> bool {
        valid_token(token) && token.starts_with(&self.prefix())
    }
}
pub fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Replica {
    pub prefix: String,
    pub origin: String,
}
#[derive(Clone)]
pub struct Directory {
    redis: ConnectionManager,
    identity: InstanceIdentity,
    replica: Replica,
    ttl: u64,
}
impl Directory {
    pub fn new(
        redis: ConnectionManager,
        identity: InstanceIdentity,
        origin: &str,
        ttl: std::time::Duration,
    ) -> Result<Self, Error> {
        let uri: axum::http::Uri = origin.parse().map_err(|_| Error::Invalid)?;
        if uri.scheme_str() != Some("https")
            || uri.authority().is_none()
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
            || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
            || origin.ends_with('/')
            || !(15..=300).contains(&ttl.as_secs())
        {
            return Err(Error::Invalid);
        }
        let replica = Replica {
            prefix: identity.prefix(),
            origin: origin.into(),
        };
        Ok(Self {
            redis,
            identity,
            replica,
            ttl: ttl.as_secs(),
        })
    }
    fn key(&self) -> String {
        format!("gateway:ha:{{replicas}}:{}", self.identity.instance)
    }
    pub fn identity(&self) -> &InstanceIdentity {
        &self.identity
    }
    pub fn local_prefix(&self) -> &str {
        &self.replica.prefix
    }
    pub async fn register(&self) -> Result<(), Error> {
        let payload = serde_json::to_string(&self.replica).map_err(|_| Error::Invalid)?;
        let result:i64=eval(&self.redis,"local old=redis.call('GET',KEYS[1]);if old then if old==ARGV[1] then redis.call('EXPIRE',KEYS[1],ARGV[2]);return 1 end;return 0 end;redis.call('SET',KEYS[1],ARGV[1],'EX',ARGV[2]);return 1",&[self.key()],&[payload,self.ttl.to_string()]).await?;
        if result == 1 {
            Ok(())
        } else {
            Err(Error::Conflict)
        }
    }
    pub async fn heartbeat(&self) -> Result<(), Error> {
        let payload = serde_json::to_string(&self.replica).map_err(|_| Error::Invalid)?;
        let result:i64=eval(&self.redis,"if redis.call('GET',KEYS[1])~=ARGV[1] then return 0 end;redis.call('EXPIRE',KEYS[1],ARGV[2]);return 1",&[self.key()],&[payload,self.ttl.to_string()]).await?;
        if result == 1 {
            Ok(())
        } else {
            Err(Error::LeaseLost)
        }
    }
    pub async fn maintain(&self) -> Result<(), Error> {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(self.ttl / 3)).await;
            self.heartbeat().await?;
        }
    }
    pub async fn resolve(&self, token: &str) -> Result<Replica, Error> {
        if !valid_token(token) {
            return Err(Error::Invalid);
        }
        let key = format!("gateway:ha:{{replicas}}:{}", &token[..8]);
        let raw: Option<String> =
            eval(&self.redis, "return redis.call('GET',KEYS[1])", &[key], &[]).await?;
        let replica: Replica =
            serde_json::from_str(&raw.ok_or(Error::LeaseLost)?).map_err(|_| Error::Unavailable)?;
        if replica.prefix != token[..16] {
            return Err(Error::LeaseLost);
        }
        Ok(replica)
    }
    pub async fn release(&self) -> Result<(), Error> {
        let payload = serde_json::to_string(&self.replica).map_err(|_| Error::Invalid)?;
        let _:i64=eval(&self.redis,"if redis.call('GET',KEYS[1])~=ARGV[1] then return 0 end;return redis.call('DEL',KEYS[1])",&[self.key()],&[payload]).await?;
        Ok(())
    }
}
