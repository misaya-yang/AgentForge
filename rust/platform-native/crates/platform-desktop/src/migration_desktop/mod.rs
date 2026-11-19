//! Native desktop API and state surfaces migrated from web/src/api.
//! The existing chat client remains responsible for Agent turn SSE/recovery.
pub mod api;
pub mod catalog;
pub mod endpoints;
pub mod state;
pub mod stream;
pub mod views;
pub mod workflows;
pub use api::DesktopApi;
pub use state::DesktopWorkspace;
