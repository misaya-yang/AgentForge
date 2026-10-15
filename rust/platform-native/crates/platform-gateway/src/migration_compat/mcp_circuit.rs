//! MCP-specific single-probe circuit; gateway service breakers have a different contract.
use super::common::CompatError;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}
struct Inner {
    state: State,
    failures: u32,
    opened: Option<Instant>,
    probe: Option<uuid::Uuid>,
}
pub struct McpCircuit {
    inner: Mutex<Inner>,
    threshold: u32,
    cooldown: Duration,
}
pub struct CircuitLease {
    circuit: Arc<McpCircuit>,
    probe: Option<uuid::Uuid>,
    finished: bool,
}
impl McpCircuit {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                state: State::Closed,
                failures: 0,
                opened: None,
                probe: None,
            }),
            threshold: 3,
            cooldown: Duration::from_secs(30),
        })
    }
    pub fn acquire(self: &Arc<Self>) -> Result<CircuitLease, CompatError> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let probe = match inner.state {
            State::Closed => None,
            State::Open if inner.opened.is_some_and(|at| at.elapsed() >= self.cooldown) => {
                let token = uuid::Uuid::new_v4();
                inner.probe = Some(token);
                inner.state = State::HalfOpen;
                Some(token)
            }
            _ => return Err(CompatError::new(502, "MCP_CIRCUIT_OPEN")),
        };
        Ok(CircuitLease {
            circuit: self.clone(),
            probe,
            finished: false,
        })
    }
}
impl CircuitLease {
    pub fn finish(mut self, result: &Result<serde_json::Value, CompatError>) {
        let mut inner = self.circuit.inner.lock().unwrap_or_else(|p| p.into_inner());
        let failure = result.as_ref().err().is_some_and(|e| {
            let code = e.detail.as_str().unwrap_or("MCP_ERROR");
            !matches!(
                code,
                "MCP_REMOTE_ERROR"
                    | "MCP_REMOTE_TOOL_ERROR"
                    | "MCP_UPSTREAM_REJECTED"
                    | "MCP_SECRET_UNAVAILABLE"
                    | "MCP_ORIGIN_DENIED"
                    | "MCP_OAUTH_AUDIENCE_MISMATCH"
                    | "MCP_DNS_REBINDING_BLOCKED"
                    | "MCP_SSRF_BLOCKED"
                    | "MCP_TLS_REQUIRED"
                    | "MCP_URL_INVALID"
            ) && !code.contains("AUTH")
                && !code.contains("DENIED")
                && !code.contains("CANCELLED")
        });
        if failure {
            if self.probe.is_some() {
                if self.probe == inner.probe {
                    inner.state = State::Open;
                    inner.opened = Some(Instant::now());
                    inner.probe = None;
                    inner.failures = self.circuit.threshold;
                }
            } else if inner.state == State::Closed {
                inner.failures += 1;
                if inner.failures >= self.circuit.threshold {
                    inner.state = State::Open;
                    inner.opened = Some(Instant::now());
                }
            }
        } else if !(inner.state == State::HalfOpen && self.probe.is_none()
            || inner.state == State::Open && self.probe.is_none()
            || self.probe.is_some() && self.probe != inner.probe)
        {
            inner.state = State::Closed;
            inner.failures = 0;
            inner.opened = None;
            inner.probe = None;
        }
        self.finished = true;
    }
}
impl Drop for CircuitLease {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut inner = self.circuit.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.state == State::HalfOpen && self.probe.is_some() && self.probe == inner.probe {
            inner.state = State::Open;
            inner.probe = None;
        }
    }
}
