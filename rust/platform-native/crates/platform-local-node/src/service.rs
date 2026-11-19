//! Loopback binding and explicit disconnect boundary; never starts a listener.
use crate::{Error,Result,ledger::Ledger,computer::{ComputerController,ComputerDriver},processes::ProcessControl};
use std::collections::HashSet;
pub struct LocalServiceBinding{pub host:String,pub port:u16}
impl Default for LocalServiceBinding{fn default()->Self{Self{host:"127.0.0.1".into(),port:8765}}}
impl LocalServiceBinding{pub fn validate(&self)->Result<()>{if self.port==0||!self.host.parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback()){return Err(Error::Denied("listener must be literal loopback"))}Ok(())}}
pub struct OutboundControlPlane{pub url:String}
impl OutboundControlPlane{pub fn validate(&self)->Result<()>{let u=url::Url::parse(&self.url).map_err(|_|Error::Denied("control endpoint"))?;if !["https","wss"].contains(&u.scheme())||u.host_str().is_none()||!u.username().is_empty()||u.password().is_some()||u.query().is_some()||u.fragment().is_some(){return Err(Error::Denied("secure control endpoint required"))}Ok(())}}
fn disconnect_cleanup(
 computer_stop: impl FnOnce() -> Result<bool>,
 cancel_processes: impl FnOnce() -> Vec<String>,
 interrupt_ledger: impl FnOnce(&HashSet<String>) -> Result<Vec<String>>,
)->Result<Vec<String>>{
 let computer_result=computer_stop();
 let cancelling:HashSet<String>=cancel_processes().into_iter().collect();
 let ledger_result=interrupt_ledger(&cancelling);
 computer_result?;
 ledger_result
}
pub struct LocalNodeRuntime<D:ComputerDriver>{pub ledger:Ledger,pub processes:ProcessControl,pub computer:ComputerController<D>,pub binding:LocalServiceBinding,pub control_plane:Option<OutboundControlPlane>,online:bool}
impl<D:ComputerDriver> LocalNodeRuntime<D>{
 pub fn new(ledger:Ledger,processes:ProcessControl,computer:ComputerController<D>,binding:LocalServiceBinding,control_plane:Option<OutboundControlPlane>)->Result<Self>{binding.validate()?;if let Some(endpoint)=&control_plane{endpoint.validate()?;}Ok(Self{ledger,processes,computer,binding,control_plane,online:false})}
 pub fn online(&self)->bool{self.online}
 pub fn connect(&mut self)->Result<()>{self.control_plane.as_ref().ok_or(Error::Denied("no outbound control plane"))?.validate()?;self.online=true;Ok(())}
 pub fn assert_online(&self)->Result<()>{if self.online{Ok(())}else{Err(Error::Denied("Local Node is offline"))}}
 pub fn disconnect(&mut self)->Result<Vec<String>>{self.online=false;let computer=&mut self.computer;let processes=&self.processes;let ledger=&mut self.ledger;disconnect_cleanup(||computer.stop(None),||processes.cancel_all(),|exclude|ledger.disconnect_except(exclude))}
}
#[cfg(test)]mod tests{use super::*;#[test]fn service_boundaries(){for host in ["0.0.0.0","localhost","192.0.2.1"]{assert!(LocalServiceBinding{host:host.into(),port:8765}.validate().is_err())}assert!(LocalServiceBinding{host:"::1".into(),port:8765}.validate().is_ok());for url in ["http://localhost/","https://u:p@example.org/","https://example.org/?q=1"]{assert!(OutboundControlPlane{url:url.into()}.validate().is_err())}assert!(OutboundControlPlane{url:"wss://example.org/device".into()}.validate().is_ok());}}

#[cfg(test)]
mod disconnect_cleanup_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn computer_stop_failure_does_not_skip_process_or_ledger_cleanup() {
        let process_calls = Cell::new(0);
        let ledger_calls = Cell::new(0);

        let result = disconnect_cleanup(
            || Err(Error::Denied("synthetic computer stop failure")),
            || {
                process_calls.set(process_calls.get() + 1);
                vec!["action-1".to_owned()]
            },
            |exclude| {
                ledger_calls.set(ledger_calls.get() + 1);
                assert_eq!(exclude, &HashSet::from(["action-1".to_owned()]));
                Ok(Vec::new())
            },
        );

        assert!(matches!(
            result,
            Err(Error::Denied("synthetic computer stop failure"))
        ));
        assert_eq!(process_calls.get(), 1);
        assert_eq!(ledger_calls.get(), 1);
    }
}
