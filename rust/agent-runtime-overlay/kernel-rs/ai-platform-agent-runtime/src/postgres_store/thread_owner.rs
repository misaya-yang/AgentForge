//! Optional Ph3 thread routing. Enabling requires the admitted owner functions and ADR.
use super::{PostgresThreadStore, store_error, thread_uuid};
use codex_protocol::ThreadId;
use codex_thread_store::{ThreadStoreError, ThreadStoreResult};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) struct OwnerRouting {
    pub address: String,
    pub peers: Vec<String>,
}
impl OwnerRouting {
    pub fn from_environment() -> Result<Option<Self>, &'static str> {
        let address = std::env::var("AI_PLATFORM_RUNTIME_OWNER_ADDRESS").ok();
        let peers = std::env::var("AI_PLATFORM_RUNTIME_OWNER_PEERS").ok();
        match (address, peers) {
            (None, None) => Ok(None),
            (Some(address), Some(peers)) => Self::parse(address, &peers).map(Some),
            _ => Err("runtime_owner_configuration_incomplete"),
        }
    }
    fn parse(address: String, peers: &str) -> Result<Self, &'static str> {
        let peers: Vec<String> = peers.split(',').map(str::to_owned).collect();
        if peers.is_empty()
            || peers.len() > 64
            || !peers.contains(&address)
            || peers.iter().any(|peer| !valid_address(peer))
        {
            return Err("runtime_owner_address_invalid");
        }
        Ok(Self { address, peers })
    }
    pub fn admits(&self, address: &str) -> bool {
        self.peers.iter().any(|peer| peer == address)
    }
}
fn valid_address(address: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(address) else {
        return false;
    };
    address.len() <= 2048
        && address.trim() == address
        && !address.contains('\\')
        && !address.chars().any(char::is_control)
        && (url.scheme() == "https"
            || url.scheme() == "http"
                && matches!(
                    url.host_str(),
                    Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
                ))
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/")
        && !address.ends_with('/')
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ThreadOwner {
    pub owner_id: Uuid,
    pub address: String,
    pub fence: i64,
}
impl PostgresThreadStore {
    pub(crate) fn owner_routing(&self) -> Option<OwnerRouting> {
        self.owner_routing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn configure_owner_routing(&self, routing: Option<OwnerRouting>) {
        *self
            .owner_routing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = routing;
    }
    pub(crate) async fn thread_owner_ready(&self) -> bool {
        if self.owner_routing().is_none() {
            return true;
        }
        sqlx::query_scalar::<_,bool>("SELECT to_regclass('assistant.assistant_runtime_thread_owners') IS NOT NULL AND COALESCE(has_table_privilege(current_user,to_regclass('assistant.assistant_runtime_thread_owners'),'SELECT'),false) AND to_regprocedure('assistant.claim_runtime_thread_owner(uuid,uuid,text)') IS NOT NULL AND to_regprocedure('assistant.assert_runtime_thread_owner_for_run(uuid,uuid,text)') IS NOT NULL AND to_regprocedure('assistant.heartbeat_runtime_thread_owner(uuid,uuid)') IS NOT NULL AND COALESCE(has_function_privilege(current_user,to_regprocedure('assistant.claim_runtime_thread_owner(uuid,uuid,text)'),'EXECUTE'),false) AND COALESCE(has_function_privilege(current_user,to_regprocedure('assistant.assert_runtime_thread_owner_for_run(uuid,uuid,text)'),'EXECUTE'),false) AND COALESCE(has_function_privilege(current_user,to_regprocedure('assistant.heartbeat_runtime_thread_owner(uuid,uuid)'),'EXECUTE'),false)")
            .fetch_one(&self.pool).await.unwrap_or(false)
    }
    pub(crate) async fn route_thread_owner(
        &self,
        thread: ThreadId,
    ) -> ThreadStoreResult<Option<ThreadOwner>> {
        let Some(config) = self.owner_routing() else {
            return Ok(None);
        };
        let root_thread_id = thread_uuid(thread)?;
        let scope = self.root_scope(root_thread_id).await?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        let row =
            sqlx::query("SELECT owner_id,address,fence FROM claim_runtime_thread_owner($1,$2,$3)")
                .bind(root_thread_id)
                .bind(self.instance_id)
                .bind(&config.address)
                .fetch_one(&mut *transaction)
                .await
                .map_err(store_error)?;
        let owner = ThreadOwner {
            owner_id: row.try_get("owner_id").map_err(store_error)?,
            address: row.try_get("address").map_err(store_error)?,
            fence: row.try_get("fence").map_err(store_error)?,
        };
        if owner.fence <= 0 || !config.admits(&owner.address) {
            return Err(ThreadStoreError::Conflict {
                message: "runtime thread owner address is not admitted".into(),
            });
        }
        transaction.commit().await.map_err(store_error)?;
        Ok(Some(owner))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_requires_explicit_bounded_peer_set_and_private_tls_destinations() {
        assert!(
            OwnerRouting::parse(
                "https://runtime-a:8094".into(),
                "https://runtime-a:8094,https://runtime-b:8094"
            )
            .unwrap()
            .admits("https://runtime-b:8094")
        );
        for address in [
            "http://runtime-a:8094",
            "https://user:secret@runtime-a",
            "https://runtime-a/path",
            "https://runtime-a?x=y",
            "https://runtime-a/",
            "https://runtime-a#fragment",
        ] {
            assert!(OwnerRouting::parse(address.into(), address).is_err());
        }
        assert!(OwnerRouting::parse("https://runtime-a".into(), "https://runtime-b").is_err());
    }
}
