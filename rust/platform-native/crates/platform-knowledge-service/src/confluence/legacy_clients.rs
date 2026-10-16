//! Legacy per-connection credential/client identity cache with source TTL and
//! per-key double-checked refresh. Shared reqwest transport owns connection pools.
use super::*;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};
pub struct LegacyClients {
    ttl: Duration,
    cached: RwLock<BTreeMap<String, (Instant, Arc<credentials::Credentials>)>>,
    gates: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}
impl LegacyClients {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            cached: RwLock::new(BTreeMap::new()),
            gates: Mutex::new(BTreeMap::new()),
        }
    }
    async fn fresh(&self, id: &str) -> Option<Arc<credentials::Credentials>> {
        self.cached
            .read()
            .await
            .get(id)
            .filter(|(created, _)| created.elapsed() < self.ttl)
            .map(|(_, c)| c.clone())
    }
    pub async fn get(&self, sources: &Sources, id: &str) -> Result<Arc<credentials::Credentials>> {
        if let Some(c) = self.fresh(id).await {
            return Ok(c);
        }
        let gate = self
            .gates
            .lock()
            .await
            .entry(id.into())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _gate = gate.lock().await;
        if let Some(c) = self.fresh(id).await {
            return Ok(c);
        }
        let row = sources
            .connection(id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))?;
        let c = Arc::new(credentials::open(&row, &sources.encryption_key)?);
        self.cached
            .write()
            .await
            .insert(id.into(), (Instant::now(), c.clone()));
        Ok(c)
    }
    pub async fn invalidate(&self, id: &str) {
        let gate = self
            .gates
            .lock()
            .await
            .entry(id.into())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _gate = gate.lock().await;
        self.cached.write().await.remove(id);
    }
    pub async fn close(&self) {
        let gates = self
            .gates
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for gate in gates {
            let _gate = gate.lock().await;
        }
        self.cached.write().await.clear();
    }
}
