//! Linux baseline boundary. No fallback to rlimits-only execution is permitted.
use super::*;

#[cfg(target_os = "linux")]
#[path = "python_isolation_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{CgroupJob, ChildPolicy};

pub(super) fn available(config: &LocalPythonSandboxConfig) -> Result<(), CodeExecutionError> {
    if !config.require_network_isolation {
        return Err(CodeExecutionError::IsolationUnavailable);
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe(config).map_err(|_| CodeExecutionError::IsolationUnavailable)
    }
    #[cfg(not(target_os = "linux"))]
    Err(CodeExecutionError::IsolationUnavailable)
}

pub(super) fn verify_supervised(
    path: &Path,
    limits: &PythonSandboxLimits,
) -> Result<(), CodeExecutionError> {
    #[cfg(target_os = "linux")]
    {
        linux::verify_supervised(path, limits).map_err(|_| CodeExecutionError::IsolationUnavailable)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, limits);
        Err(CodeExecutionError::IsolationUnavailable)
    }
}
