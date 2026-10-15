use std::net::SocketAddr;
use std::sync::Arc;

use ai_platform_capability_worker::http_service::{WorkerState, router};
use ai_platform_capability_worker::postgres_store::PostgresExecutionStore;
use ai_platform_capability_worker::quiz_capabilities::QuizPersistenceAdapter;
use ai_platform_capability_worker::read_capabilities::{
    PostgresSessionMemoryReadAdapter, ReadCapabilityConfig, ReadCapabilityExecutor,
    ReqwestReadHttpAdapter,
};
use ai_platform_capability_worker::write_capabilities::{
    PostgresMemoryWriteAdapter, WriteCapabilityExecutor,
};
use ai_platform_capability_worker::{
    attachment_capabilities::{AttachmentCapabilityBroker, AttachmentCapabilityConfig},
    confluence_write_broker::{ConfluenceWriteBrokerConfig, ReqwestConfluenceWriteBroker},
    external_write_capabilities::{ExternalWriteExecutor, GatewayWriteBrokerRouter},
    image_write_broker::{ImageWriteBrokerConfig, ReqwestImageWriteBroker},
    local_node_broker::{LocalNodeBroker, UnavailableLocalNodeTransport},
    office_artifact_broker::{OfficeArtifactBrokerConfig, ReqwestOfficeArtifactStore},
    office_capabilities::OfficeCapabilityExecutor,
    python_artifact_broker::{PythonArtifactBrokerConfig, ReqwestPythonArtifactStore},
    python_code_execution::{LocalPythonSandboxBroker, LocalPythonSandboxConfig},
};
use anyhow::Context;
use clap::Parser;
use sqlx::postgres::PgConnectOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_WORKER_BIND",
        default_value = "127.0.0.1:8095"
    )]
    bind: SocketAddr,

    #[arg(long, env = "AI_PLATFORM_INTERNAL_TOKEN", hide_env_values = true)]
    internal_token: String,

    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
        hide_env_values = true
    )]
    lease_signing_secret: String,

    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
        hide_env_values = true
    )]
    proof_secret: String,

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
        env = "AI_PLATFORM_CAPABILITY_WORKER_DB_POOL_MAX_SIZE",
        default_value_t = 4
    )]
    postgres_pool_max_size: u32,

    #[arg(
        long,
        env = "AI_PLATFORM_KNOWLEDGE_SERVICE_URL",
        default_value = "http://knowledge-service:8092"
    )]
    knowledge_service_url: String,

    #[arg(
        long,
        env = "AI_PLATFORM_GATEWAY_INTERNAL_URL",
        default_value = "http://gateway:8080"
    )]
    gateway_url: String,

    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_WORKSPACE_ROOT",
        default_value = "/workspace"
    )]
    workspace_root: std::path::PathBuf,

    #[arg(long, env = "AI_PLATFORM_OFFICE_FONT_PATHS", value_delimiter = ',')]
    office_font_paths: Vec<std::path::PathBuf>,

    /// Exact immutable generation; all three inputs are required together.
    #[arg(long, env = "AI_PLATFORM_PLUGIN_STORE")]
    plugin_store: Option<std::path::PathBuf>,
    #[arg(long, env = "AI_PLATFORM_PLUGIN_TRUST")]
    plugin_trust: Option<std::path::PathBuf>,
    #[arg(long, env = "AI_PLATFORM_PLUGIN_ACTIVATION")]
    plugin_activation: Option<std::path::PathBuf>,

    /// Enables deterministic contract fixtures for isolated smoke tests only.
    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_WORKER_FIXTURES_ENABLED",
        default_value_t = false
    )]
    fixtures_enabled: bool,

    /// Enables the two durable memory writers only after their Runtime policy
    /// and PostgreSQL approval path are deployed together.
    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED",
        default_value_t = false
    )]
    writes_enabled: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let args = Args::parse();
    if args.internal_token.len() < 32
        || args.lease_signing_secret.len() < 32
        || args.proof_secret.len() < 32
    {
        anyhow::bail!(
            "internal token, lease signing secret, and capability proof secret must be at least 32 bytes"
        );
    }
    let store = Arc::new(
        PostgresExecutionStore::connect_with_options(
            PgConnectOptions::new()
                .host(&args.postgres_host)
                .port(args.postgres_port)
                .username(&args.postgres_user)
                .password(&args.postgres_password)
                .database(&args.postgres_database),
            args.postgres_pool_max_size,
        )
        .await
        .context("connect capability execution store")?,
    );
    let read_http_client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build capability HTTP client")?;
    let external_write_executor = if args.writes_enabled {
        let mut brokers = GatewayWriteBrokerRouter::default();
        brokers.register(
            "confluence_write",
            Arc::new(
                ReqwestConfluenceWriteBroker::new(
                    read_http_client.clone(),
                    ConfluenceWriteBrokerConfig {
                        gateway_url: args.gateway_url.clone(),
                        internal_token: args.internal_token.clone(),
                        proof_secret: args.proof_secret.clone(),
                    },
                )
                .context("configure Confluence write broker")?,
            ),
        )?;
        brokers.register(
            "generate_image",
            Arc::new(
                ReqwestImageWriteBroker::new(
                    read_http_client.clone(),
                    ImageWriteBrokerConfig {
                        gateway_url: args.gateway_url.clone(),
                        internal_token: args.internal_token.clone(),
                        proof_secret: args.proof_secret.clone(),
                    },
                )
                .context("configure image write broker")?,
            ),
        )?;
        Some(Arc::new(ExternalWriteExecutor::new(Arc::new(brokers))))
    } else {
        None
    };
    let office_executor = if args.writes_enabled {
        let artifact_store = ReqwestOfficeArtifactStore::new(
            read_http_client.clone(),
            OfficeArtifactBrokerConfig {
                gateway_url: args.gateway_url.clone(),
                internal_token: args.internal_token.clone(),
                proof_secret: args.proof_secret.clone(),
            },
        )
        .context("configure Office artifact broker")?;
        let executor = OfficeCapabilityExecutor::new(Arc::new(artifact_store))
            .with_external_fonts(load_controlled_fonts(&args.office_font_paths)?)
            .context("configure Office controlled fonts")?;
        Some(Arc::new(executor))
    } else {
        None
    };
    let read_executor = Arc::new(
        ReadCapabilityExecutor::new(
            ReadCapabilityConfig {
                knowledge_base_url: args.knowledge_service_url,
                gateway_url: args.gateway_url.clone(),
                workspace_root: args.workspace_root.clone(),
                internal_token: args.internal_token.clone(),
                proof_secret: args.proof_secret.clone(),
            },
            Arc::new(ReqwestReadHttpAdapter::new(read_http_client.clone())),
        )
        .context("configure read capability adapters")?
        .with_session_memory(Arc::new(PostgresSessionMemoryReadAdapter::new(
            store.pool(),
        ))),
    );
    let python_executor = Arc::new(
        LocalPythonSandboxBroker::new(LocalPythonSandboxConfig {
            workspace_root: args.workspace_root.clone(),
            ..LocalPythonSandboxConfig::default()
        })
        .context("configure Python sandbox")?,
    );
    let python_artifact_store = Arc::new(
        ReqwestPythonArtifactStore::new(
            read_http_client.clone(),
            PythonArtifactBrokerConfig {
                gateway_url: args.gateway_url.clone(),
                internal_token: args.internal_token.clone(),
                proof_secret: args.proof_secret.clone(),
            },
        )
        .context("configure Python artifact broker")?,
    );
    let attachment_executor = Arc::new(
        AttachmentCapabilityBroker::new(
            read_http_client.clone(),
            AttachmentCapabilityConfig {
                gateway_url: args.gateway_url.clone(),
                internal_token: args.internal_token.clone(),
                proof_secret: args.proof_secret.clone(),
            },
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?,
    );
    let local_node_broker = Arc::new(LocalNodeBroker::new(Arc::new(
        UnavailableLocalNodeTransport,
    )));
    let write_executor = if args.writes_enabled {
        Some(Arc::new(WriteCapabilityExecutor::new(Arc::new(
            PostgresMemoryWriteAdapter::new(store.pool()),
        ))))
    } else {
        None
    };
    let quiz_executor = args
        .writes_enabled
        .then(|| Arc::new(QuizPersistenceAdapter::new(store.pool())));
    let state = WorkerState::try_new_with_writes(
        store,
        args.internal_token,
        args.lease_signing_secret.into_bytes(),
        args.fixtures_enabled,
        args.writes_enabled,
    )?
    .with_read_executor(read_executor);
    let state = state
        .with_python_executor(python_executor)
        .with_python_artifact_store(python_artifact_store)
        .with_attachment_executor(attachment_executor)
        .with_local_node_broker(local_node_broker);
    let state = if let Some(write_executor) = write_executor {
        state.with_write_executor(write_executor)
    } else {
        state
    };
    let state = if let Some(quiz_executor) = quiz_executor {
        state.with_quiz_executor(quiz_executor)
    } else {
        state
    };
    let state = if let Some(external_write_executor) = external_write_executor {
        state.with_external_write_executor(external_write_executor)
    } else {
        state
    };
    let state = if let Some(office_executor) = office_executor {
        state.with_office_executor(office_executor)
    } else {
        state
    };
    let state = match (&args.plugin_store, &args.plugin_trust, &args.plugin_activation) {
        (None, None, None) => state,
        (Some(root), Some(trust), Some(activation)) if !args.fixtures_enabled => {
            let policy = ai_platform_capability_worker::plugin_packs::TrustPolicy::load(trust)?;
            ai_platform_capability_worker::plugin_packs::verify_running_worker(&policy)?;
            let active = ai_platform_capability_worker::plugin_packs::load_current_activation(
                root, &policy, activation, args.writes_enabled,
            )?;
            state.with_plugin_activation(active)?
        }
        _ => anyhow::bail!("plugin store, trust and activation must be selected together without fixtures"),
    };
    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .context("bind capability worker")?;
    info!(address = %args.bind, "Agent capability worker is ready");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve capability worker")?;
    Ok(())
}

fn load_controlled_fonts(paths: &[std::path::PathBuf]) -> anyhow::Result<Vec<Vec<u8>>> {
    const MAX_FONTS: usize = 8;
    const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;
    const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
    if paths.len() > MAX_FONTS {
        anyhow::bail!("too many Office font files");
    }
    let mut fonts = Vec::with_capacity(paths.len());
    let mut total = 0_usize;
    for path in paths {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspect Office font {}", path.display()))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
            || metadata.len() > MAX_FONT_BYTES
        {
            anyhow::bail!("Office font file is invalid: {}", path.display());
        }
        let bytes =
            std::fs::read(path).with_context(|| format!("read Office font {}", path.display()))?;
        total = total
            .checked_add(bytes.len())
            .context("Office font pack size overflow")?;
        if total > MAX_TOTAL_BYTES {
            anyhow::bail!("Office font pack is too large");
        }
        fonts.push(bytes);
    }
    Ok(fonts)
}

async fn shutdown_signal() {
    let interrupt = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install terminate handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}
