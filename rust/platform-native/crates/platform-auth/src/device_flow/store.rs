//! Atomic device-flow authority, using the existing restricted auth namespace.
use super::RedisSessions;
use crate::device_flow::{DeviceError, Flow, RefreshGrant, digest};
use serde_json::Value;
#[derive(Clone)]
pub(crate) struct Store {
    connection: redis::aio::ConnectionManager,
}
impl RedisSessions {
    pub(crate) fn device_store(&self) -> Store {
        Store {
            connection: self.connection.clone(),
        }
    }
}
impl Store {
    pub(crate) async fn eval<T: redis::FromRedisValue>(
        &self,
        script: &str,
        keys: &[String],
        args: &[String],
    ) -> Result<T, DeviceError> {
        let mut command = redis::cmd("EVAL");
        command.arg(script).arg(keys.len());
        for key in keys {
            command.arg(key);
        }
        for arg in args {
            command.arg(arg);
        }
        command
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| DeviceError::Unavailable)
    }
    pub(crate) async fn rate(&self, identity: &str, max: u64) -> Result<(), DeviceError> {
        let count:u64=self.eval("local n=redis.call('INCR',KEYS[1]);if n==1 then redis.call('EXPIRE',KEYS[1],60) end;return n", &[format!("auth:device:rate:{}",digest(identity))], &[]).await?;
        if count > max {
            Err(DeviceError::Rate)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn create(
        &self,
        code: &str,
        user_code: &str,
        flow: &Flow,
    ) -> Result<(), DeviceError> {
        let result:i64=self.eval("if redis.call('EXISTS',KEYS[1])==1 or redis.call('EXISTS',KEYS[2])==1 then return 0 end;redis.call('SET',KEYS[1],ARGV[1],'EX',ARGV[3]);redis.call('SET',KEYS[2],ARGV[2],'EX',ARGV[3]);return 1", &[flow_key(code),user_key(user_code)], &[serialize(flow)?,digest(code),crate::device_flow::DEVICE_TTL.to_string()]).await?;
        if result == 1 {
            Ok(())
        } else {
            Err(DeviceError::Unavailable)
        }
    }
    pub(crate) async fn by_user(&self, code: &str) -> Result<(String, String, Flow), DeviceError> {
        let values:Vec<String>=self.eval("local id=redis.call('GET',KEYS[1]);if not id then return {} end;local raw=redis.call('GET',ARGV[1]..id);if not raw then return {} end;return {id,raw}", &[user_key(code)], &["auth:device:flow:".into()]).await?;
        if values.len() != 2 {
            return Err(DeviceError::Expired);
        }
        Ok((
            values[0].clone(),
            values[1].clone(),
            serde_json::from_str(&values[1]).map_err(|_| DeviceError::Unavailable)?,
        ))
    }
    pub(crate) async fn decide(
        &self,
        id: &str,
        old: &str,
        flow: &Flow,
        now: u64,
    ) -> Result<(), DeviceError> {
        let result:i64=self.eval("local raw=redis.call('GET',KEYS[1]);if raw~=ARGV[1] then return 0 end;local f=cjson.decode(raw);if f.status~='pending' or f.expires_at<=tonumber(ARGV[3]) then return 0 end;redis.call('SET',KEYS[1],ARGV[2],'KEEPTTL');return 1", &[format!("auth:device:flow:{id}")], &[old.into(),serialize(flow)?,now.to_string()]).await?;
        if result == 1 {
            Ok(())
        } else {
            Err(DeviceError::InvalidGrant)
        }
    }
    pub(crate) async fn poll(
        &self,
        code: &str,
        challenge: &str,
        now: u64,
    ) -> Result<(String, Flow), DeviceError> {
        let result: Vec<String> = self
            .eval(
                POLL,
                &[flow_key(code)],
                &[challenge.into(), now.to_string()],
            )
            .await?;
        match result.first().map(String::as_str) {
            Some("approved") if result.len() == 2 => Ok((
                result[1].clone(),
                serde_json::from_str(&result[1]).map_err(|_| DeviceError::Unavailable)?,
            )),
            Some("pending") => Err(DeviceError::Pending),
            Some("slow_down") => Err(DeviceError::SlowDown),
            Some("denied") => Err(DeviceError::Denied),
            Some("expired") => Err(DeviceError::Expired),
            _ => Err(DeviceError::InvalidGrant),
        }
    }
    pub(crate) async fn complete(
        &self,
        code: &str,
        old: &str,
        flow: &Flow,
        grant: &RefreshGrant,
        record: &Value,
        parent: &Value,
        now: u64,
    ) -> Result<(), DeviceError> {
        let result: i64 = self
            .eval(
                COMPLETE,
                &[
                    flow_key(code),
                    format!("auth:device:user:{}", flow.user_code_hash),
                    grant_key(&grant.id),
                    RedisSessions::token_key(&grant.jti).map_err(|_| DeviceError::InvalidGrant)?,
                    RedisSessions::token_key(&grant.binding.parent_jti)
                        .map_err(|_| DeviceError::InvalidGrant)?,
                    format!("auth:user_tokens:{}", grant.binding.user_id),
                ],
                &[
                    old.into(),
                    serialize(grant)?,
                    serialize(record)?,
                    serialize(parent)?,
                    grant.binding.expires_at.saturating_sub(now).to_string(),
                    grant.jti.clone(),
                    now.to_string(),
                ],
            )
            .await?;
        if result == 1 {
            Ok(())
        } else {
            Err(DeviceError::InvalidGrant)
        }
    }
    pub(crate) async fn grant(&self, id: &str) -> Result<(String, RefreshGrant), DeviceError> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(grant_key(id))
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| DeviceError::Unavailable)?;
        let raw = raw.ok_or(DeviceError::InvalidGrant)?;
        let grant = serde_json::from_str(&raw).map_err(|_| DeviceError::Unavailable)?;
        Ok((raw, grant))
    }
    pub(crate) async fn rotate(
        &self,
        old: &str,
        grant: &RefreshGrant,
        parent: &Value,
        now: u64,
    ) -> Result<(), DeviceError> {
        let result:i64=self.eval("if redis.call('GET',KEYS[1])~=ARGV[1] or redis.call('EXISTS',KEYS[2])~=1 or redis.call('GET',KEYS[3])~=ARGV[3] then return 0 end;local g=cjson.decode(ARGV[1]);if g.binding.expires_at<=tonumber(ARGV[4]) then return 0 end;redis.call('SET',KEYS[1],ARGV[2],'KEEPTTL');return 1", &[grant_key(&grant.id),RedisSessions::token_key(&grant.jti).map_err(|_|DeviceError::InvalidGrant)?,RedisSessions::token_key(&grant.binding.parent_jti).map_err(|_|DeviceError::InvalidGrant)?], &[old.into(),serialize(grant)?,serialize(parent)?,now.to_string()]).await?;
        if result == 1 {
            Ok(())
        } else {
            Err(DeviceError::InvalidGrant)
        }
    }
    pub(crate) async fn revoke(&self, id: &str, hash: &str) -> Result<(), DeviceError> {
        // All issued refresh credentials remain valid revocation handles until
        // family expiry, including rotated handles. They cannot refresh again.
        let _:i64=self.eval("local raw=redis.call('GET',KEYS[1]);if not raw then return 0 end;local g=cjson.decode(raw);local found=false;for _,h in ipairs(g.revocation_hashes) do if h==ARGV[1] then found=true end end;if not found then return 0 end;redis.call('DEL','auth:token:'..g.jti);redis.call('DEL',KEYS[1]);redis.call('SREM','auth:user_tokens:'..g.binding.user_id,g.jti);return 1", &[grant_key(id)], &[hash.into()]).await?;
        Ok(())
    }
}
fn serialize(value: &impl serde::Serialize) -> Result<String, DeviceError> {
    serde_json::to_string(value).map_err(|_| DeviceError::Unavailable)
}
fn flow_key(code: &str) -> String {
    format!("auth:device:flow:{}", digest(code))
}
fn user_key(code: &str) -> String {
    format!("auth:device:user:{}", digest(code))
}
fn grant_key(id: &str) -> String {
    format!("auth:device:grant:{id}")
}
pub(crate) const POLL: &str = r#"
local raw=redis.call('GET',KEYS[1]);if not raw then return {'expired'} end
local f=cjson.decode(raw);local now=tonumber(ARGV[2]);if f.challenge~=ARGV[1] then return {'invalid'} end
if f.expires_at<=now then return {'expired'} end
if f.last_poll>0 and now-f.last_poll<f.interval then f.interval=math.min(f.interval+5,600);f.last_poll=now;redis.call('SET',KEYS[1],cjson.encode(f),'KEEPTTL');return {'slow_down'} end
f.last_poll=now;raw=cjson.encode(f);redis.call('SET',KEYS[1],raw,'KEEPTTL')
if f.status=='approved' then return {'approved',raw} end
if f.status=='denied' then return {'denied'} end
return {'pending'}
"#;
pub(crate) const COMPLETE: &str = r#"
local raw=redis.call('GET',KEYS[1]);if raw~=ARGV[1] or redis.call('GET',KEYS[5])~=ARGV[4] then return 0 end
local f=cjson.decode(raw);if f.status~='approved' or f.expires_at<=tonumber(ARGV[7]) or tonumber(ARGV[5])<=0 or redis.call('EXISTS',KEYS[3])==1 or redis.call('EXISTS',KEYS[4])==1 then return 0 end
redis.call('SET',KEYS[3],ARGV[2],'EX',ARGV[5]);redis.call('SET',KEYS[4],ARGV[3],'EX',ARGV[5]);redis.call('SADD',KEYS[6],ARGV[6]);redis.call('EXPIRE',KEYS[6],10800);redis.call('DEL',KEYS[1],KEYS[2]);return 1
"#;
