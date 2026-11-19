//! Errors of bounded HTTP/framing clients, without Gateway dispatch/admission policy.
use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterFailure {
    ConnectError,
    RemoteProtocolError,
    PoolTimeout,
    ReadTimeout,
    WriteTimeout,
    ReadError,
    WriteError,
    HttpStatus(u16),
    OperationTimeout,
    Other,
}

#[derive(Clone,Debug,PartialEq)]
pub enum DispatchError {Adapter(AdapterFailure),AdapterValidationFailed(&'static str),AdapterValidationDetail(String)}
impl fmt::Display for DispatchError {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result {match self {Self::Adapter(_)=>f.write_str("upstream_dispatch_failed"),Self::AdapterValidationFailed(s)=>f.write_str(s),Self::AdapterValidationDetail(s)=>f.write_str(s)}}}
#[derive(Clone, Debug, PartialEq)]
pub enum AdapterError {
    Dispatch(DispatchError),
    BulkheadExhausted { service_id: String },
    UnsupportedOperation,
}
impl From<DispatchError> for AdapterError {
    fn from(error: DispatchError) -> Self {
        Self::Dispatch(error)
    }
}
impl From<AdapterFailure> for AdapterError {
    fn from(error: AdapterFailure) -> Self {
        Self::Dispatch(DispatchError::Adapter(error))
    }
}
impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dispatch(error) => error.fmt(f),
            Self::BulkheadExhausted { service_id } => {
                write!(f, "Adapter bulkhead exhausted for {service_id}")
            }
            Self::UnsupportedOperation => f.write_str("adapter_operation_not_implemented"),
        }
    }
}
impl std::error::Error for AdapterError {}
