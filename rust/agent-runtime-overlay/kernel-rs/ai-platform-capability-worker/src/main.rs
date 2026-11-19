#[path = "lease_verification.rs"]
mod lease_verification;
mod sandbox_bootstrap;
mod startup_admission;

#[path = "../../ai-platform-capability-contract/src/database_connection.rs"]
mod database_connection;
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
    local_node_broker::{LocalNodeBroker, LocalNodeGatewayConfig, ReqwestLocalNodeTransport},
    office_artifact_broker::{OfficeArtifactBrokerConfig, ReqwestOfficeArtifactStore},
    office_capabilities::OfficeCapabilityExecutor,
    python_artifact_broker::{PythonArtifactBrokerConfig, ReqwestPythonArtifactStore},
};
use anyhow::Context;
use clap::Parser;
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

    #[arg(
        long,
        env = "AI_PLATFORM_INTERNAL_TOKEN",
        default_value = "",
        hide_env_values = true
    )]
    internal_token: String,

    #[arg(
        long,
        env = "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
        default_value = "",
        hide_env_values = true
    )]
    proof_secret: String,

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

    /// Optional remote service. All three inputs are required together;
    /// private key bytes stay in this control process and are never env values.
    #[arg(long, env = "AI_PLATFORM_SANDBOX_SOCKET")]
    sandbox_socket: Option<std::path::PathBuf>,
    #[arg(long, env = "AI_PLATFORM_SANDBOX_KEY_ID")]
    sandbox_key_id: Option<String>,
    #[arg(
        long,
        env = "AI_PLATFORM_SANDBOX_SIGNING_KEY_FILE",
        hide_env_values = true
    )]
    sandbox_signing_key_file: Option<std::path::PathBuf>,

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
    let telemetry = ai_platform_capability_worker::telemetry::init("ai-platform-capability-worker")
        .await
        .map_err(anyhow::Error::msg)?;
    let args = Args::parse();
    if args.internal_token.len() < 32
        && !ai_platform_capability_worker::workload_identity::required()
    {
        anyhow::bail!("internal token must be at least 32 bytes");
    }
    lease_verification::validate_production_configuration().map_err(anyhow::Error::msg)?;
    ai_platform_capability_worker::broker_proof::validate_configuration(&args.gateway_url)
        .map_err(anyhow::Error::msg)?;
    if !args.proof_secret.is_empty() {
        anyhow::bail!("worker_broker_signing_secret_forbidden");
    }
    let database = database_connection::ServiceDatabase::from_environment(
        "DATA_CAPABILITY_WORKER",
        database_connection::policy::DatabaseRole::CapabilityWorker,
    )?;
    let postgres_options = database.options();
    startup_admission::require_compatible_schema(postgres_options).await?;
    let store = Arc::new(PostgresExecutionStore::from_pool(database.connect().await?));
    let read_http_client = ai_platform_capability_worker::workload_identity::internal_client(
        &[&args.gateway_url, &args.knowledge_service_url],
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(30),
    )
    .map_err(anyhow::Error::msg)?;
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
    let python_executor = sandbox_bootstrap::configure(
        args.sandbox_socket.as_deref(),
        args.sandbox_key_id.as_deref(),
        args.sandbox_signing_key_file.as_deref(),
    )?;
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
    let local_node_transport = ReqwestLocalNodeTransport::new(
        read_http_client.clone(),
        LocalNodeGatewayConfig {
            gateway_url: args.gateway_url.clone(),
            internal_token: args.internal_token.clone(),
            proof_secret: args.proof_secret.clone(),
        },
    )
    .context("configure Local Node Gateway broker")?;
    let local_node_broker = Arc::new(LocalNodeBroker::new(Arc::new(local_node_transport)));
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
        Vec::new(),
        args.fixtures_enabled,
        args.writes_enabled,
    )?
    .with_read_executor(read_executor);
    let state = if let Some(executor) = python_executor {
        state.with_remote_python_executor(executor)
    } else {
        state
    };
    let state = state
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
    let state = match (
        &args.plugin_store,
        &args.plugin_trust,
        &args.plugin_activation,
    ) {
        (None, None, None) => state,
        (Some(root), Some(trust), Some(activation)) if !args.fixtures_enabled => {
            let policy = ai_platform_capability_worker::plugin_packs::TrustPolicy::load(trust)?;
            ai_platform_capability_worker::plugin_packs::verify_running_worker(&policy)?;
            let active = ai_platform_capability_worker::plugin_packs::load_current_activation(
                root,
                &policy,
                activation,
                args.writes_enabled,
            )?;
            state.with_plugin_activation(active)?
        }
        _ => anyhow::bail!(
            "plugin store, trust and activation must be selected together without fixtures"
        ),
    };
    info!(address = %args.bind, "Agent capability worker is ready");
    let serve_result = ai_platform_capability_worker::workload_identity::serve(
        args.bind,
        router(state),
        shutdown_signal(),
    )
    .await
    .context("serve capability worker");
    if let Some(telemetry) = telemetry {
        telemetry.shutdown().await;
    }
    serve_result?;
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
