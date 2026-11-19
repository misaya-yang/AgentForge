use super::{
    LocalNodeError as Error, Result, computer::ComputerController, ledger::ActionLedger,
    processes::ProcessRunner,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn disconnect_cleanup(
    computer_stop: impl FnOnce() -> Result<bool>,
    cancel_processes: impl FnOnce() -> Result<Vec<String>>,
    interrupt_ledger: impl FnOnce(&HashSet<String>) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let computer_result = computer_stop();
    let process_result = cancel_processes();
    let excluded = process_result
        .as_ref()
        .map(|ids| ids.iter().cloned().collect())
        .unwrap_or_default();
    let ledger_result = interrupt_ledger(&excluded);

    computer_result?;
    process_result?;
    ledger_result
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalServiceBinding {
    pub host: String,
    pub port: u16,
}
impl Default for LocalServiceBinding {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8765,
        }
    }
}
impl LocalServiceBinding {
    pub fn validate(&self) -> Result<()> {
        if !self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) || self.port == 0 {
            return Err(Error::new(
                "boundary_violation",
                "Local Node listener must use a literal loopback address and valid port",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct OutboundControlPlane {
    pub url: String,
}
impl OutboundControlPlane {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.url)
            .map_err(|_| Error::new("boundary_violation", "control-plane URL invalid"))?;
        if !["https", "wss"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
        {
            return Err(Error::new(
                "boundary_violation",
                "control plane requires HTTPS/WSS without credentials, query or fragment",
            ));
        }
        Ok(())
    }
}
pub struct LocalNodeRuntime {
    pub ledger: Arc<ActionLedger>,
    pub process_runner: Arc<ProcessRunner>,
    pub computer: Arc<ComputerController>,
    pub binding: LocalServiceBinding,
    pub control_plane: Option<OutboundControlPlane>,
    online: AtomicBool,
}
impl LocalNodeRuntime {
    pub fn new(
        ledger: Arc<ActionLedger>,
        process_runner: Arc<ProcessRunner>,
        computer: Arc<ComputerController>,
        binding: LocalServiceBinding,
        control_plane: Option<OutboundControlPlane>,
    ) -> Result<Self> {
        binding.validate()?;
        if let Some(cp) = &control_plane {
            cp.validate()?;
        }
        Ok(Self {
            ledger,
            process_runner,
            computer,
            binding,
            control_plane,
            online: AtomicBool::new(false),
        })
    }
    pub fn online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }
    pub fn connect(&self) -> Result<()> {
        self.control_plane
            .as_ref()
            .ok_or_else(|| {
                Error::new("boundary_violation", "no outbound control plane configured")
            })?
            .validate()?;
        self.online.store(true, Ordering::Release);
        Ok(())
    }
    pub fn assert_online(&self) -> Result<()> {
        if !self.online() {
            return Err(Error::denied("Local Node is offline"));
        }
        Ok(())
    }
    pub fn disconnect(&self) -> Result<Vec<String>> {
        self.online.store(false, Ordering::Release);
        disconnect_cleanup(
            || self.computer.emergency_stop(),
            || self.process_runner.cancel_all(),
            |exclude| self.ledger.interrupt_running(exclude),
        )
    }
}

#[cfg(test)]
mod disconnect_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn computer_stop_failure_does_not_skip_process_or_ledger_cleanup() {
        let computer_calls = Cell::new(0);
        let process_calls = Cell::new(0);
        let ledger_calls = Cell::new(0);
        let mut observed_excluded = HashSet::new();

        let result = disconnect_cleanup(
            || {
                computer_calls.set(computer_calls.get() + 1);
                Err(Error::new("computer_stop_failed", "synthetic failure"))
            },
            || {
                process_calls.set(process_calls.get() + 1);
                Ok(vec!["action-1".to_owned()])
            },
            |exclude| {
                ledger_calls.set(ledger_calls.get() + 1);
                observed_excluded = exclude.clone();
                Ok(Vec::new())
            },
        );

        assert_eq!(result.unwrap_err().code, "computer_stop_failed");
        assert_eq!(computer_calls.get(), 1);
        assert_eq!(process_calls.get(), 1);
        assert_eq!(ledger_calls.get(), 1);
        assert_eq!(observed_excluded, HashSet::from(["action-1".to_owned()]));
    }
}
