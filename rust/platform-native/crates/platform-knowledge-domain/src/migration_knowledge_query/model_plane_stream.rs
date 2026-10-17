//! Bounded Gateway model SSE; vendor deltas never constitute a settled receipt.
use super::{
    model_plane::{ExecutionState, ModelCall, ModelLease, ModelStreamEvent, check_execution},
    types::{QueryError, Result},
};
use crate::migration_adapters::{HttpResponse, Lines};
use serde_json::Value;
pub struct ModelStream {
    lines: Lines,
    call: ModelCall,
    lease: ModelLease,
    data: String,
    settled: bool,
}
impl ModelStream {
    pub fn new(call: ModelCall, lease: ModelLease, response: reqwest::Response) -> Result<Self> {
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return Err(QueryError::InvalidProviderResponse);
        }
        let lines = Lines::new(HttpResponse {
            response,
            permit: None,
        })
        .map_err(|_| QueryError::InvalidProviderResponse)?;
        Ok(Self {
            lines,
            call,
            lease,
            data: String::new(),
            settled: false,
        })
    }
    pub async fn next(&mut self) -> Result<Option<Value>> {
        if self.settled {
            return Ok(None);
        }
        loop {
            let Some(line) = self
                .lines
                .next()
                .await
                .map_err(|_| QueryError::DependenciesFailed)?
            else {
                return Err(QueryError::DependenciesFailed);
            };
            if !line.is_empty() {
                if let Some(data) = line.strip_prefix("data:") {
                    if self.data.len() + data.len() > 8 * 1024 * 1024 {
                        return Err(QueryError::InvalidProviderResponse);
                    }
                    self.data.push_str(data.trim_start());
                    self.data.push('\n');
                } else if !line.starts_with(':')
                    && !line.starts_with("event:")
                    && !line.starts_with("id:")
                    && !line.starts_with("retry:")
                {
                    return Err(QueryError::InvalidProviderResponse);
                }
                continue;
            }
            if self.data.is_empty() {
                continue;
            }
            let event: ModelStreamEvent = serde_json::from_str(&std::mem::take(&mut self.data))
                .map_err(|_| QueryError::InvalidProviderResponse)?;
            match event {
                ModelStreamEvent::Delta { output } => return Ok(Some(output)),
                ModelStreamEvent::Receipt { execution } => {
                    check_execution(&self.call, &self.lease, &execution)?;
                    if execution.state != ExecutionState::Completed
                        || !(200..300).contains(&execution.upstream_status)
                    {
                        return Err(QueryError::Provider(execution.upstream_status));
                    }
                    self.settled = true;
                    self.lines.close();
                    return Ok(None);
                }
                ModelStreamEvent::Error { .. } => return Err(QueryError::DependenciesFailed),
            }
        }
    }
    pub fn close(&mut self) {
        self.lines.close();
    }
}
