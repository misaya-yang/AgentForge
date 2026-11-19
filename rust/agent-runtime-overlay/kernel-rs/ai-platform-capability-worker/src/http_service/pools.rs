//! Process-local bounded admission and unit-cost deficit round robin.
//! Tickets own both queue membership and running capacity, including on abort.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use ai_platform_capability_contract::{
    CapabilityDescriptorV2, CapabilityEffect, CapabilityScopeV2,
};
use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ExecutionClass {
    Read,
    Write,
    Office,
    Sandbox,
}

impl ExecutionClass {
    pub fn for_descriptor(descriptor: &CapabilityDescriptorV2) -> Self {
        match descriptor.id.as_str() {
            "execute_python_code" => Self::Sandbox,
            id if id.starts_with("mcp_docgen__") => Self::Office,
            _ if matches!(descriptor.effect, CapabilityEffect::Read) => Self::Read,
            _ => Self::Write,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PoolLimit {
    pub concurrency: usize,
    /// Maximum waiting requests, in addition to running requests.
    pub queue_capacity: usize,
}

#[derive(Clone, Debug)]
pub struct ExecutionPoolConfig {
    pub reads: PoolLimit,
    pub writes: PoolLimit,
    pub office: PoolLimit,
    pub sandbox: PoolLimit,
}
impl Default for ExecutionPoolConfig {
    fn default() -> Self {
        Self {
            reads: PoolLimit {
                concurrency: 32,
                queue_capacity: 256,
            },
            writes: PoolLimit {
                concurrency: 8,
                queue_capacity: 64,
            },
            office: PoolLimit {
                concurrency: 1,
                queue_capacity: 16,
            },
            sandbox: PoolLimit {
                concurrency: 2,
                queue_capacity: 16,
            },
        }
    }
}

#[derive(Clone)]
pub struct ExecutionPools(Arc<Mutex<Scheduler>>);
struct Scheduler {
    next: u64,
    pools: BTreeMap<ExecutionClass, Pool>,
    entries: BTreeMap<u64, Entry>,
    targets: BTreeSet<String>,
}
struct Pool {
    limit: PoolLimit,
    running: usize,
    waiting: usize,
    tenants: BTreeMap<String, Tenant>,
    ring: VecDeque<String>,
}
#[derive(Default)]
struct Tenant {
    deficit: usize,
    jobs: VecDeque<u64>,
}
struct Entry {
    class: ExecutionClass,
    tenant: String,
    target: Option<String>,
    granted: bool,
    sender: Option<oneshot::Sender<()>>,
}

pub struct ExecutionTicket {
    pools: ExecutionPools,
    id: u64,
    ready: oneshot::Receiver<()>,
    class: ExecutionClass,
    admitted_at: std::time::Instant,
}

impl ExecutionPools {
    pub fn new(config: ExecutionPoolConfig) -> Result<Self, &'static str> {
        let limits = [
            (ExecutionClass::Read, config.reads),
            (ExecutionClass::Write, config.writes),
            (ExecutionClass::Office, config.office),
            (ExecutionClass::Sandbox, config.sandbox),
        ];
        if limits.iter().any(|(_, limit)| {
            limit.concurrency == 0
                || limit
                    .concurrency
                    .checked_add(limit.queue_capacity)
                    .is_none()
        }) {
            return Err(
                "execution pool concurrency must be positive and capacity must not overflow",
            );
        }
        Ok(Self(Arc::new(Mutex::new(Scheduler {
            next: 0,
            entries: BTreeMap::new(),
            targets: BTreeSet::new(),
            pools: limits
                .into_iter()
                .map(|(class, limit)| {
                    (
                        class,
                        Pool {
                            limit,
                            running: 0,
                            waiting: 0,
                            tenants: BTreeMap::new(),
                            ring: VecDeque::new(),
                        },
                    )
                })
                .collect(),
        }))))
    }

    /// A capacity rejection occurs before a durable reservation commits or dispatches.
    /// A ticket may temporarily occupy a running slot during reservation I/O.
    pub fn admit(
        &self,
        class: ExecutionClass,
        tenant: &str,
        target: Option<String>,
    ) -> Result<ExecutionTicket, &'static str> {
        let mut scheduler = self.0.lock().unwrap();
        let pool = &scheduler.pools[&class];
        // A blocked target cannot borrow idle concurrency as extra queue space.
        let can_run = pool.running < pool.limit.concurrency
            && target
                .as_ref()
                .is_none_or(|key| !scheduler.targets.contains(key))
            && pool.waiting == 0;
        if !can_run && pool.waiting >= pool.limit.queue_capacity {
            return Err("capacity_exhausted");
        }
        let id = scheduler.next;
        scheduler.next = scheduler.next.checked_add(1).ok_or("capacity_exhausted")?;
        let (sender, ready) = oneshot::channel();
        scheduler.entries.insert(
            id,
            Entry {
                class,
                tenant: tenant.into(),
                target,
                granted: false,
                sender: Some(sender),
            },
        );
        let pool = scheduler.pools.get_mut(&class).unwrap();
        if !pool.tenants.contains_key(tenant) {
            pool.ring.push_back(tenant.into());
        }
        pool.tenants
            .entry(tenant.into())
            .or_default()
            .jobs
            .push_back(id);
        pool.waiting += 1;
        scheduler.pump();
        Ok(ExecutionTicket {
            pools: self.clone(),
            id,
            ready,
            class,
            admitted_at: std::time::Instant::now(),
        })
    }
}

impl ExecutionTicket {
    pub(super) async fn wait(&mut self) -> Result<(), oneshot::error::RecvError> {
        let result = (&mut self.ready).await;
        tracing::debug!(execution_class = ?self.class,
            queue_wait_ms = self.admitted_at.elapsed().as_millis() as u64,
            "capability execution pool admission ready");
        result
    }
}
impl Drop for ExecutionTicket {
    fn drop(&mut self) {
        let mut scheduler = self.pools.0.lock().unwrap();
        let Some(entry) = scheduler.entries.remove(&self.id) else {
            return;
        };
        if entry.granted {
            if let Some(key) = entry.target {
                scheduler.targets.remove(&key);
            }
            scheduler.pools.get_mut(&entry.class).unwrap().running -= 1;
        } else {
            let pool = scheduler.pools.get_mut(&entry.class).unwrap();
            pool.waiting -= 1;
            if let Some(tenant) = pool.tenants.get_mut(&entry.tenant) {
                tenant.jobs.retain(|id| *id != self.id);
                if tenant.jobs.is_empty() {
                    pool.tenants.remove(&entry.tenant);
                    pool.ring.retain(|name| name != &entry.tenant);
                }
            }
        }
        scheduler.pump();
    }
}

impl Scheduler {
    fn pump(&mut self) {
        for class in [
            ExecutionClass::Read,
            ExecutionClass::Write,
            ExecutionClass::Office,
            ExecutionClass::Sandbox,
        ] {
            let pool = self.pools.get_mut(&class).unwrap();
            let mut stalled = 0;
            while pool.running < pool.limit.concurrency && !pool.ring.is_empty() {
                let name = pool.ring.pop_front().unwrap();
                let tenant = pool.tenants.get_mut(&name).unwrap();
                // Every execution has cost one; each visit grants one quantum.
                // Blocked resources earn no unbounded credit and cannot hog a pool.
                tenant.deficit = tenant.deficit.saturating_add(1).min(1);
                let position = tenant.jobs.iter().position(|id| {
                    self.entries[id]
                        .target
                        .as_ref()
                        .is_none_or(|key| !self.targets.contains(key))
                });
                if let Some(position) = position {
                    let id = tenant.jobs.remove(position).unwrap();
                    tenant.deficit -= 1;
                    let entry = self.entries.get_mut(&id).unwrap();
                    entry.granted = true;
                    if let Some(key) = &entry.target {
                        self.targets.insert(key.clone());
                    }
                    // Receiver cannot be dropped while its ticket holds this mutex.
                    let _ = entry.sender.take().unwrap().send(());
                    pool.running += 1;
                    pool.waiting -= 1;
                    stalled = 0;
                } else {
                    stalled += 1;
                }
                if tenant.jobs.is_empty() {
                    pool.tenants.remove(&name);
                } else {
                    pool.ring.push_back(name);
                }
                if stalled >= pool.ring.len() && stalled > 0 {
                    break;
                }
            }
        }
    }
}

/// Conservative, server-bound lock scopes. Never use caller-supplied resource
/// hints to allow concurrent writes. Connector/unclassified writes serialize the
/// tenant; memory uses its authoritative principal, and todos their owner scope.
pub(super) fn write_target(
    descriptor: &CapabilityDescriptorV2,
    scope: &CapabilityScopeV2,
    binding: &Value,
) -> Option<String> {
    if matches!(descriptor.effect, CapabilityEffect::Read) {
        return None;
    }
    let target = match descriptor.id.as_str() {
        "todo_write" => {
            serde_json::json!(["todo", scope.tenant_id, scope.user_id, scope.session_id])
        }
        "update_user_memory" => serde_json::json!([
            "memory",
            scope.tenant_id,
            binding["memory_policy"]["memory_principal"]
        ]),
        // These brokers create execution-ID-scoped artifacts/receipts, rather
        // than mutating an existing target. Idempotency/fencing already prevent
        // two dispatches of the same execution. Do not couple sandbox or Office
        // capacity to unrelated external writes in the same tenant.
        "execute_python_code"
        | "generate_image"
        | "generate_quiz"
        | "mcp_docgen__generate_document"
        | "mcp_docgen__modify_document" => return None,
        _ => {
            // Whole tenant is deliberate: unknown/dynamic targets cannot bypass a
            // lock by varying argument keys or connector descriptor formatting.
            serde_json::json!(["write", scope.tenant_id])
        }
    };
    Some(target.to_string())
}

#[cfg(test)]
#[path = "pools_tests.rs"]
mod tests;
