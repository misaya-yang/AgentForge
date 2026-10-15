use std::sync::Arc;

use codex_app_server_client::InProcessClientStartArgs;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::ConfigBuilder;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_protocol::protocol::SessionSource;
use codex_thread_store::InMemoryThreadStore;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::AgentKernel;
use super::AiPlatformExtensionRegistry;
use super::test_support::run_with_agent_stack;

#[test]
fn kernel_uses_agent_thread_lifecycle_with_platform_store() {
    run_with_agent_stack("ai-platform-kernel-lifecycle", async {
        let agent_home = TempDir::new().expect("temp agent home");
        let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
        let config = Arc::new(
            ConfigBuilder::default()
                .codex_home(agent_home.path().to_path_buf())
                .fallback_cwd(Some(agent_home.path().to_path_buf()))
                .loader_overrides(loader_overrides.clone())
                .build()
                .await
                .expect("test config should build"),
        );
        let thread_store = Arc::new(InMemoryThreadStore::default());
        let reserved_thread_id = codex_protocol::ThreadId::new();
        let kernel = AgentKernel::start(
            InProcessClientStartArgs {
                arg0_paths: Arg0DispatchPaths::default(),
                config,
                cli_overrides: Vec::new(),
                loader_overrides,
                strict_config: true,
                cloud_config_bundle: CloudConfigBundleLoader::default(),
            embedded_network_policy: Default::default(),
                feedback: CodexFeedback::new(),
                log_db: None,
                state_db: None,
                environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
                config_warnings: Vec::new(),
                session_source: SessionSource::Cli,
                enable_codex_api_key_env: false,
                client_name: "ai-platform-agent-runtime-test".to_string(),
                client_version: "0.0.0-test".to_string(),
                experimental_api: true,
                mcp_server_openai_form_elicitation: false,
                opt_out_notification_methods: Vec::new(),
                channel_capacity: 8,
            },
            thread_store.clone(),
            AiPlatformExtensionRegistry::new(),
        )
        .await
        .expect("kernel should start");

        let response = kernel
            .request_thread_start(
                ClientRequest::ThreadStart {
                    request_id: RequestId::Integer(1),
                    params: ThreadStartParams::default(),
                },
                reserved_thread_id,
            )
            .await
            .expect("request should reach Codex")
            .expect("thread/start should succeed");
        let thread: ThreadStartResponse =
            serde_json::from_value(response).expect("thread/start response should decode");

        assert_eq!(thread.thread.path, None);
        assert_eq!(thread.thread.id, reserved_thread_id.to_string());
        assert_eq!(thread_store.calls().await.create_thread, 1);

        let reserved_turn_id = uuid::Uuid::now_v7().to_string();
        let response = kernel
            .request_turn_start(
                ClientRequest::TurnStart {
                    request_id: RequestId::Integer(2),
                    params: TurnStartParams {
                        thread_id: reserved_thread_id.to_string(),
                        input: vec![UserInput::Text {
                            text: "hello".to_string(),
                            text_elements: Vec::new(),
                        }],
                        ..Default::default()
                    },
                },
                reserved_turn_id.clone(),
            )
            .await
            .expect("request should reach Codex")
            .expect("turn/start should succeed");
        let turn: TurnStartResponse =
            serde_json::from_value(response).expect("turn/start response should decode");
        assert_eq!(turn.turn.id, reserved_turn_id);

        kernel.shutdown().await.expect("kernel should stop cleanly");
    });
}
