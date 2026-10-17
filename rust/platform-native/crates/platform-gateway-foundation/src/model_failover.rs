//! Typed failover domain and frozen public wire compatibility adapter.
//! The selection/ordering implementation is shared with provider routing.
//! JSON coercion and secret-bearing runtime payloads stay inside `wire`.
#[path = "model_failover_wire.rs"]
mod wire;
pub use crate::provider_routing::{
    CatalogSnapshot, FailoverPlan, FailoverPolicy, FailoverWarning, FallbackRef, ModelRef,
    RoutePolicy, RouteTarget, RoutingFailure,
};
pub use wire::*;
