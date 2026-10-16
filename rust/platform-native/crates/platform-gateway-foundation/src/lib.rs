//! Internal gateway compilation boundary; gateway reexports the public API.
pub mod native_contract;
pub mod architecture_status_api;
pub mod assistant_catalog;
pub mod assistant_memory_api;
pub mod assistant_memory_store;
pub mod assistant_memory_types;
pub mod assistant_memory_validation;
pub mod auth_config_api;
pub mod balancer_config_api;
pub mod billing;
pub mod billing_scheduler;
pub mod billing_store;
#[doc(hidden)]
pub mod capabilities;
pub mod capacity;
pub mod capacity_admission;
pub mod catalog_http;
pub mod circuit_breaker;
pub mod client_ip;
pub mod config_api_keys_api;
pub mod config_api_keys_store;
pub mod config_audit_postgres;
pub mod config_audit_read;
pub mod config_audit_writer;
pub mod connector_admin_api;
pub mod connector_admin_store;
pub mod connector_oauth_cipher;
pub mod connector_oauth_http;
pub mod connector_oauth_state;
pub mod connector_principal_store;
pub mod connector_principal_validation;
pub mod connector_user_activation;
pub mod connector_user_api;
pub mod connector_user_search;
pub mod connector_user_service;
pub mod connector_user_store;
pub mod connector_user_types;
pub mod dashboard_api;
pub mod dispatch_preflight;
pub mod dispatcher;
pub mod dispatcher_rate_limiter;
pub mod gateway_dispatcher;
pub mod gateway_model_meta;
pub mod generic_rest_adapter;
pub mod guest_session_manager;
pub mod guest_session_redis;
pub mod health_api;
#[doc(hidden)]
pub mod http_content_type;
pub mod invocation_http;
pub mod langgraph_service_validation;
pub mod load_shedder;
pub mod local_nodes_api;
pub mod local_nodes_store;
pub mod local_nodes_types;
pub mod local_nodes_validation;
pub mod mcp_discovery;
pub mod mcp_gateway_broker;
pub mod mcp_http;
pub mod mcp_registry_api;
pub mod mcp_registry_store;
pub mod mcp_registry_validation;
pub mod metrics_admin_api;
pub mod metrics_recorder;
pub mod metrics_redaction;
pub mod model_admin_api;
pub mod model_catalog_sync;
pub mod model_failover;
pub mod model_provider_storage;
pub mod model_service;
#[doc(hidden)]
pub mod postgres_operation;
pub mod presign_api;
pub mod pricing_receipt;
pub mod protocol;
pub mod provider_admin_api;
pub mod provider_cipher;
pub mod provider_health;
pub mod provider_probe_http;
pub mod provider_routing;
pub mod provider_service;
pub mod provider_startup_seeder;
pub mod provider_templates;
pub mod quiz_grading;
#[doc(hidden)]
pub mod quiz_unicode;
pub mod quota_admin_api;
pub mod quota_store;
pub mod rate_config_api;
pub mod rate_limits;
pub mod rate_policy_store;
pub mod realtime_metrics;
pub mod registry_health_monitor;
pub mod registry_load_balancer;
pub mod registry_storage;
pub mod request_validation;
pub mod roles_api;
pub mod roles_store;
pub mod self_service_api_keys;
pub mod self_service_api_keys_api;
pub mod self_service_api_keys_store;
pub mod service_access_policy;
pub mod service_access_resolver;
pub mod service_config_api;
pub mod service_ownership;
pub mod service_registry;
pub mod services_api;
pub mod session_manager;
pub mod session_storage;
pub mod sessions_api;
pub mod setup_api;
pub mod stream_statistics;
pub mod system_status_api;
pub mod task_http;
pub mod task_actor;
pub mod task_delivery;
mod task_durable_worker;
pub mod task_manager;
pub mod task_storage;
pub mod task_worker;
pub mod tenant_policies_api;
pub mod tenant_policies_store;
pub mod tool_inventory_api;
pub mod usage_admin_api;
pub mod usage_parser;
pub mod usage_pricing;
pub mod usage_recorder;
pub mod users_api;
pub mod users_store;

/// Shared source-compatible deterministic JSON codec.
pub mod python_json;

pub mod sdk_agent_config;

pub mod sdk_read_grants;
pub mod sdk_capabilities;

pub mod model_catalog_review;

/// Shared, optional distributed HTTP span recording and W3C propagation.
pub mod distributed_tracing;

pub mod message_queue_transport;

mod generic_rest_multibyte;

pub mod request_governance;

pub mod websocket_transport;

pub mod event_envelope;
