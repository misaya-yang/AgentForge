//! Cluster-wide dashboard admission. Sockets remain connection-affine; lost
//! renewals disconnect them, and TTL reclaims capacity after process death.
use super::*;
use std::time::Duration;
#[derive(Clone)]
pub struct SocketLeases {
    redis: ConnectionManager,
    global: u64,
    tenant: u64,
    ttl: u64,
}
impl SocketLeases {
    pub fn new(
        redis: ConnectionManager,
        global: u64,
        tenant: u64,
        ttl: Duration,
    ) -> Result<Self, Error> {
        let ttl = ttl.as_secs();
        if global == 0
            || tenant == 0
            || tenant > global
            || global > 1_000_000
            || !(15..=300).contains(&ttl)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            redis,
            global,
            tenant,
            ttl,
        })
    }
    pub async fn acquire(&self, tenant: &str, user: &str) -> Result<SocketLease, Error> {
        bounded(tenant)?;
        bounded(user)?;
        let id = uuid::Uuid::new_v4().to_string();
        let keys = [
            "gateway:ha:{dashboard}:all".into(),
            format!("gateway:ha:{{dashboard}}:tenant:{}", hash(tenant)),
        ];
        let result: i64 = eval(
            &self.redis,
            ACQUIRE,
            &keys,
            &[
                id.clone(),
                self.global.to_string(),
                self.tenant.to_string(),
                self.ttl.to_string(),
            ],
        )
        .await?;
        if result != 1 {
            return Err(Error::Limit);
        }
        Ok(SocketLease {
            redis: self.redis.clone(),
            keys,
            id,
            ttl: self.ttl,
        })
    }
}
pub struct SocketLease {
    redis: ConnectionManager,
    keys: [String; 2],
    id: String,
    ttl: u64,
}
impl SocketLease {
    pub async fn renew(&self) -> Result<(), Error> {
        let result: i64 = eval(
            &self.redis,
            RENEW,
            &self.keys,
            &[self.id.clone(), self.ttl.to_string()],
        )
        .await?;
        if result != 1 {
            Err(Error::LeaseLost)
        } else {
            Ok(())
        }
    }
    pub async fn maintain(&self) -> Result<(), Error> {
        loop {
            tokio::time::sleep(Duration::from_secs(self.ttl / 3)).await;
            self.renew().await?;
        }
    }
    pub async fn release(&self) -> Result<(), Error> {
        let _: i64 = eval(
            &self.redis,
            "redis.call('ZREM',KEYS[1],ARGV[1]);return redis.call('ZREM',KEYS[2],ARGV[1])",
            &self.keys,
            &[self.id.clone()],
        )
        .await?;
        Ok(())
    }
}
const ACQUIRE: &str = r#"local t=redis.call('TIME');local now=tonumber(t[1])*1000+math.floor(tonumber(t[2])/1000);for _,key in ipairs(KEYS) do redis.call('ZREMRANGEBYSCORE',key,'-inf',now) end;if redis.call('ZCARD',KEYS[1])>=tonumber(ARGV[2]) or redis.call('ZCARD',KEYS[2])>=tonumber(ARGV[3]) then return 0 end;for _,key in ipairs(KEYS) do redis.call('ZADD',key,now+tonumber(ARGV[4])*1000,ARGV[1]);redis.call('EXPIRE',key,tonumber(ARGV[4])+5) end;return 1"#;
const RENEW: &str = r#"local t=redis.call('TIME');local now=tonumber(t[1])*1000+math.floor(tonumber(t[2])/1000);for _,key in ipairs(KEYS) do local score=redis.call('ZSCORE',key,ARGV[1]);if not score or tonumber(score)<=now then return 0 end end;for _,key in ipairs(KEYS) do redis.call('ZADD',key,'XX',now+tonumber(ARGV[2])*1000,ARGV[1]);redis.call('EXPIRE',key,tonumber(ARGV[2])+5) end;return 1"#;
