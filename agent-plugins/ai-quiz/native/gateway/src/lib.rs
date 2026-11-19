//! ai-quiz HTTP/storage plugin. Gateway generic authorities enter through ports.
pub use ai_platform_gateway_foundation::{
    client_ip, http_content_type, postgres_operation, rate_config_api, rate_limits,
    service_registry,
};
pub use ai_platform_plugin_quiz::{quiz_grading, quiz_unicode};
pub mod ports;
pub mod quiz_api;
pub mod quiz_entropy;
pub mod quiz_rate;
pub mod quiz_scope;
pub mod quiz_share_service;
pub mod quiz_share_store;
pub mod quiz_store;
pub mod quiz_types;
