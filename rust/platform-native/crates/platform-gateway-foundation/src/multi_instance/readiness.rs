//! HA is an explicit construction profile. Legacy/local implementations remain
//! usable only outside that profile; source completion never retires release gates.
use super::*;
use crate::{
    dashboard_api::DashboardApi, guest_session_manager::GuestSessionManager,
    rate_limits::QuotaRpmLimiter, sdk_read_grants::SdkReadGrants, task_manager::TaskManager,
};
pub fn require_shared(
    rpm: &QuotaRpmLimiter,
    guests: &GuestSessionManager,
    grants: &SdkReadGrants,
    tasks: &TaskManager,
    dashboard: &DashboardApi,
    context_shared: bool,
    directory: &super::affinity::Directory,
) -> Result<(), Error> {
    require_durable_tasks(tasks)?;
    if !rpm.shared_authority_configured()
        || !guests.shared_authority_configured()
        || !grants.affinity_configured()
        || !dashboard.shared_websocket_authority_configured()
        || !context_shared
        || grants.affinity_prefix().as_deref() != Some(directory.local_prefix())
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub fn require_durable_tasks(manager: &TaskManager) -> Result<(), Error> {
    if manager.requires_durable_actor() {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
/// Uses the already-reviewed PG delivery authority, not Redis ZPOPMIN (which
/// loses a popped task on crash) and never the generic in-memory fallback queue.
pub async fn durable_tasks(
    data: sqlx::PgPool,
    control: sqlx::PgPool,
    callbacks: std::sync::Arc<dyn crate::task_manager::SafeCallbackTransport>,
) -> Result<std::sync::Arc<TaskManager>, Error> {
    let storage = std::sync::Arc::new(
        crate::task_storage::PgTaskStorage::new(data.clone(), control.clone())
            .map_err(|_| Error::Unavailable)?,
    );
    let ledger = std::sync::Arc::new(
        crate::task_delivery::PgTaskDeliveryLedger::new(data, control)
            .map_err(|_| Error::Unavailable)?,
    );
    ledger
        .verify_ready()
        .await
        .map_err(|_| Error::Unavailable)?;
    Ok(std::sync::Arc::new(
        TaskManager::durable(storage, ledger).with_safe_callback_transport(callbacks),
    ))
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RetirementEvidence {
    pub source_sha256: String,
    pub instances: u32,
    pub runtime_executed: bool,
    pub kill9_failover_passed: bool,
    pub lease_fencing_passed: bool,
    pub duplicate_executions: u64,
    pub all_state_contracts_passed: bool,
    pub measured_rps: f64,
    pub capacity_contract_passed: bool,
    pub adr_approved: bool,
}
impl RetirementEvidence {
    pub fn permits_retirement(&self) -> bool {
        self.source_sha256.len() == 64
            && self.source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && self.instances >= 2
            && self.runtime_executed
            && self.kill9_failover_passed
            && self.lease_fencing_passed
            && self.duplicate_executions == 0
            && self.all_state_contracts_passed
            && self.capacity_contract_passed
            && self.measured_rps.is_finite()
            && self.measured_rps > 0.0
            && self.adr_approved
    }
}
