use super::{
    LocalNodeError as Error, Result, computer::ComputerController, ledger::ActionLedger,
    processes::ProcessRunner,
};
use serde::{Deserialize, Serialize};
use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
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
        self.computer.emergency_stop()?;
        let cancelling = self.process_runner.cancel_all()?;
        self.ledger
            .interrupt_running(&cancelling.into_iter().collect())
    }
}
