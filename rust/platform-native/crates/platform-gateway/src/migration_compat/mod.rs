//! Native compatibility boundaries. Factories are inert until the host mounts them.
pub mod capability;
pub mod common;
pub mod confluence;
pub mod eval;
pub mod langgraph;
pub mod langgraph_body;
pub mod mcp_invocation;
pub mod responses;
pub mod transparent_proxy;

pub mod billing_stream;
pub mod eval_behavior;
pub mod eval_candidate;
pub mod eval_clients;
pub mod eval_comparison;
pub mod eval_executor;
pub mod eval_gate;
pub mod eval_http;
pub mod eval_live;
pub mod eval_observation;
pub mod eval_online;
pub mod eval_operations;
pub mod eval_outbox_dispatch;
pub mod eval_queue;
pub mod eval_scoring;
pub mod proxy_trace;

pub mod response_cache;

pub mod eval_export;
pub mod eval_trace_http;

pub mod eval_revisions;

pub mod eval_knowledge;

pub mod eval_feedback;

pub mod mcp_circuit;

pub mod proxy_http;

pub mod route_pattern;

pub mod sse_heartbeat;
