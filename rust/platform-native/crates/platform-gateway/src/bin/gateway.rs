//! Gateway executable. Explicit JSON stdin and opt-in source-compatible env modes.
//! Configuration remains in memory; resolved secrets are never printed.
use ai_platform_gateway::migration_host::bootstrap::{
    BootstrapError, configuration::GatewayDocument,
};
use std::{io::Read, process::ExitCode, time::Duration};
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--print-config-schema"] {
        println!("{}", ai_platform_config_loader::versioned::schema());
        return ExitCode::SUCCESS;
    }
    let options = match ai_platform_gateway::migration_host::worker_roles::Arguments::parse(&args) {
        Ok(options) => options,
        Err(_) => {
            eprintln!(
                "Usage: gateway (--config-stdin | --check-config-stdin | --config-env | --check-config-env) [--role api|worker|eval|all]"
            );
            return ExitCode::FAILURE;
        }
    };
    let check_only = options.check_only;
    let environment_mode = options.environment_mode;
    let mut bytes = Vec::new();
    if environment_mode {
        let resolved = match ai_platform_config_loader::load_gateway_document_from_process() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        bytes = match serde_json::to_vec(&resolved) {
            Ok(value) => value,
            Err(_) => {
                eprintln!("Invalid gateway configuration");
                return ExitCode::FAILURE;
            }
        };
    } else if std::io::stdin()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > 4 * 1024 * 1024
    {
        eprintln!("Invalid or oversized gateway configuration input");
        return ExitCode::FAILURE;
    }
    let mut document = match GatewayDocument::parse(&bytes) {
        Ok(v) => v,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(role) = options.role {
        document
            .environment
            .insert("GATEWAY_PROCESS_ROLE".into(), role.name().into());
    }
    if ai_platform_gateway::migration_host::worker_roles::ProcessRole::parse(
        document
            .environment
            .get("GATEWAY_PROCESS_ROLE")
            .map(String::as_str)
            .unwrap_or("all"),
    )
    .is_err()
    {
        eprintln!("Invalid gateway_process_role");
        return ExitCode::FAILURE;
    }
    if check_only {
        let checked = document.settings().and_then(|settings| {
            let role = ai_platform_gateway::migration_host::worker_roles::ProcessRole::parse(
                document
                    .environment
                    .get("GATEWAY_PROCESS_ROLE")
                    .map(String::as_str)
                    .unwrap_or("all"),
            )
            .map_err(BootstrapError::Configuration)?;
            role.validate_delivery(&settings.task_delivery_mode)
                .map_err(BootstrapError::Configuration)?;
            document.project(&settings)?;
            document.policies()?;
            Ok(())
        });
        return match checked {
            Ok(()) => {
                println!("Configuration projection valid; runtime dependencies were not checked");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(v) => v,
        Err(_) => {
            eprintln!("Gateway runtime unavailable");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        // Before constructing pools, matching the shared source startup order.
        let tracing = ai_platform_gateway_foundation::distributed_tracing::initialize("gateway");
        let result = run(document).await;
        let _ = tokio::task::spawn_blocking(move || tracing.shutdown()).await;
        result
    });
    // Process boundary: remaining async connection tasks are dropped with this
    // owned runtime. A timed-out library host must not be reused as if drained.
    runtime.shutdown_timeout(Duration::from_secs(5));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
async fn run(document: GatewayDocument) -> Result<(), BootstrapError> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| BootstrapError::Construction("signal_registration"))?;
    let (host, address) = document.construct().await?;
    let shutdown = async move {
        #[cfg(unix)]
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
    host.serve(address, shutdown)
        .await
        .map_err(|_| BootstrapError::Construction("gateway_host_lifecycle"))
}
