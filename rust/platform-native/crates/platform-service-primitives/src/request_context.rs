//! Verified host-generated request correlation shared across service boundaries.
#[derive(Clone, Debug)]
pub struct RequestContext {
    pub request_id: String,
    pub trace_id: String,
    pub span_id: String,
    pub traceparent: String,
    pub anonymous_id: Option<String>,
}
