//! Gateway executable. Input is a bounded explicit JSON document on stdin.
//! No dotenv, ambient environment credentials or credential files are discovered.
use ai_platform_gateway::migration_host::bootstrap::{
    BootstrapError, configuration::GatewayDocument,
};
use std::{io::Read, process::ExitCode, time::Duration};
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let check_only = args == ["--check-config-stdin"];
    if args != ["--config-stdin"] && !check_only {
        eprintln!("Usage: gateway --config-stdin | --check-config-stdin (explicit JSON input)");
        return ExitCode::FAILURE;
    }
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > 4 * 1024 * 1024
    {
        eprintln!("Invalid or oversized gateway configuration input");
        return ExitCode::FAILURE;
    }
    let document = match GatewayDocument::parse(&bytes) {
        Ok(v) => v,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if check_only {
        let checked = document.settings().and_then(|settings| {
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
    let result = runtime.block_on(run(document));
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
