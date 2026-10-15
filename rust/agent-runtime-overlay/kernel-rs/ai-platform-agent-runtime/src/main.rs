use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use ai_platform_agent_runtime::AgentKernel;
use ai_platform_agent_runtime::AiPlatformExtensionRegistry;
use ai_platform_agent_runtime::PostgresThreadStore;
use ai_platform_agent_runtime::http_service::RuntimeHttpService;
use clap::Parser;
use codex_app_server_client::EmbeddedNetworkPolicy;
use codex_app_server_client::InProcessClientStartArgs;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::ConfigBuilder;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_protocol::protocol::SessionSource;
use sqlx::postgres::PgConnectOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[arg(
        long,
        env = "AI_PLATFORM_RUNTIME_BIND",
        default_value = "127.0.0.1:8094"
    )]
    bind: SocketAddr,

    #[arg(long, env = "AI_PLATFORM_INTERNAL_TOKEN", hide_env_values = true)]
    internal_token: String,

    #[arg(long, env = "AI_PLATFORM_AGENT_HOME")]
    agent_home: PathBuf,

    #[arg(
        long,
        env = "AI_PLATFORM_RUNTIME_WORKDIR",
        default_value = "/workspace"
    )]
    runtime_workdir: PathBuf,

    #[arg(long, env = "POSTGRES_HOST", default_value = "127.0.0.1")]
    postgres_host: String,

    #[arg(long, env = "POSTGRES_PORT", default_value_t = 5432)]
    postgres_port: u16,

    #[arg(long, env = "POSTGRES_USER")]
    postgres_user: String,

    #[arg(long, env = "POSTGRES_PASSWORD", hide_env_values = true)]
    postgres_password: String,

    #[arg(long, env = "POSTGRES_DB")]
    postgres_database: String,

    #[arg(
        long,
        env = "AI_PLATFORM_RUNTIME_DB_POOL_MAX_SIZE",
        default_value_t = 10
    )]
    postgres_pool_max_size: u32,

    #[arg(
        long,
        env = "AI_PLATFORM_RUNTIME_CHANNEL_CAPACITY",
        default_value_t = 256
    )]
    channel_capacity: usize,
}

fn main() -> anyhow::Result<()> {
    codex_arg0::arg0_dispatch_or_else(async_main)
}

async fn async_main(arg0_paths: Arg0DispatchPaths) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let args = Args::parse();
    prepare_isolated_agent_home(&args.agent_home)?;
    if !args.runtime_workdir.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "AI_PLATFORM_RUNTIME_WORKDIR must be absolute",
        )
        .into());
    }

    let store = Arc::new(
        PostgresThreadStore::connect_with_options(
            PgConnectOptions::new()
                .host(&args.postgres_host)
                .port(args.postgres_port)
                .username(&args.postgres_user)
                .password(&args.postgres_password)
                .database(&args.postgres_database),
            args.postgres_pool_max_size,
        )
        .await?,
    );
    // Use the upstream catalog with the supported Gateway wire selected. This
    // is model metadata, not a new model loop or a guessed config flag.
    let model_catalog_path = prepare_gateway_model_catalog(&args.agent_home)?;
    let cli_overrides = vec![(
        "model_catalog_json".to_string(),
        toml::Value::String(model_catalog_path.to_string_lossy().into_owned()),
    )];
    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    let embedded_network_policy = EmbeddedNetworkPolicy::load(&loader_overrides).await;
    let mut config = ConfigBuilder::default()
        .codex_home(args.agent_home)
        .cli_overrides(cli_overrides.clone())
        .fallback_cwd(Some(args.runtime_workdir))
        .loader_overrides(loader_overrides.clone())
        .build()
        .await?;
    embedded_network_policy.activate(&mut config);
    let config = Arc::new(config);
    let environment_manager = Arc::new(EnvironmentManager::without_environments(
        embedded_network_policy.bind(config.http_client_factory()),
    ));
    let kernel = AgentKernel::start(
        InProcessClientStartArgs {
            arg0_paths,
            config,
            cli_overrides,
            loader_overrides,
            strict_config: true,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
            embedded_network_policy,
            feedback: CodexFeedback::new(),
            log_db: None,
            // The Runtime is a shared multi-tenant process. Do not enable the
            // upstream per-home SQLite state DB here: it would mix tenant
            // memory across sessions. Platform memory is authoritative in the
            // scoped PostgreSQL/capability snapshot until a tenant-keyed state
            // backend is available.
            state_db: None,
            environment_manager,
            config_warnings: Vec::new(),
            session_source: SessionSource::Custom("ai-platform".to_string()),
            enable_codex_api_key_env: false,
            client_name: "ai-platform-agent-runtime".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            // The private host uses turn-scoped Responses metadata for signed
            // model-plane leases. The public HTTP surface remains constrained.
            experimental_api: true,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: args.channel_capacity,
        },
        store.clone(),
        AiPlatformExtensionRegistry::new().with_thread_store(Arc::clone(&store)),
    )
    .await?;
    let runtime = RuntimeHttpService::start(kernel, store, args.internal_token)?;
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    info!(address = %args.bind, "AI Platform Agent runtime is ready");
    let execution_shutdown = runtime.execution_shutdown_token();
    let stop_accepting = tokio_util::sync::CancellationToken::new();
    let stop_http = stop_accepting.clone();
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, runtime.router())
            .with_graceful_shutdown(async move { stop_http.cancelled().await }),
    );
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result,
        () = shutdown_signal() => Ok(()),
        () = execution_shutdown.cancelled() => {
            tracing::warn!("execution ownership lost; retiring Runtime instance");
            Ok(())
        },
    };
    stop_accepting.cancel();
    runtime.shutdown().await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut server).await;
    result?;
    Ok(())
}

fn prepare_isolated_agent_home(path: &Path) -> std::io::Result<()> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "AI_PLATFORM_AGENT_HOME must be absolute",
        ));
    }
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "AI_PLATFORM_AGENT_HOME must not be a symlink",
        ));
    }
    std::fs::create_dir_all(path)?;
    let marker = path.join(".ai-platform-runtime-home");
    if marker.exists() {
        if std::fs::read_to_string(&marker)?.trim() != "ai-platform-agent-home/v1" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "AI_PLATFORM_AGENT_HOME marker is invalid",
            ));
        }
    } else if path.read_dir()?.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "AI_PLATFORM_AGENT_HOME must be empty on first initialization",
        ));
    } else {
        std::fs::write(&marker, "ai-platform-agent-home/v1\n")?;
    }
    for forbidden in ["AGENTS.md", "auth.json", "config.toml"] {
        if path.join(forbidden).exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("isolated Agent Runtime home contains forbidden host state: {forbidden}"),
            ));
        }
    }
    Ok(())
}

fn prepare_gateway_model_catalog(agent_home: &Path) -> std::io::Result<PathBuf> {
    let mut catalog =
        codex_models_manager::bundled_models_response().map_err(std::io::Error::other)?;
    for model in &mut catalog.models {
        model.use_responses_lite = false;
    }
    let path = agent_home.join("gateway-model-catalog.json");
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Gateway model catalog must not be a symlink",
        ));
    }
    std::fs::write(
        &path,
        serde_json::to_vec(&catalog).map_err(std::io::Error::other)?,
    )?;
    Ok(path)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
