//! Independent signed Knowledge service. Configuration is read only on execution.
use ai_platform_gateway::migration_host::knowledge_bootstrap::{
    KnowledgeError, Result, process::ProcessSettings,
};
use std::{process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("knowledge-service runtime unavailable");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run());
    // Final process boundary, never a claim that a timed-out host can be reused.
    runtime.shutdown_timeout(Duration::from_secs(5));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Error variants contain stage names only, never DSNs, keys or bodies.
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let settings = ProcessSettings::from_environment()?;
    let role = settings.configuration.role.as_str();
    let (graph, host, port, gaps) = settings.construct().await?;
    for gap in gaps.iter().chain(graph.degraded_components.iter()) {
        eprintln!("knowledge-service capability unavailable or limited: {gap}");
    }
    let lifecycle = graph.lifecycle.clone();
    let router = match graph.into_router() {
        Ok(router) => router,
        Err(error) => {
            lifecycle.stop().await.ok();
            return Err(error);
        }
    };
    let listener = match tokio::net::TcpListener::bind((host.as_str(), port)).await {
        Ok(listener) => listener,
        Err(_) => {
            lifecycle.stop().await.ok();
            return Err(KnowledgeError::Startup("listener_bind"));
        }
    };
    // Worker mode serves health only; All/API expose the same signed HTTP routes.
    // Register SIGTERM before startup so container shutdown isn't lost.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| KnowledgeError::Startup("signal_registration"))?;
    lifecycle.start().await?;
    eprintln!("knowledge-service started with role {role}");
    let (shutdown, receive_shutdown) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = receive_shutdown.await;
    });
    let server = std::future::IntoFuture::into_future(server);
    tokio::pin!(server);
    let signal = async {
        #[cfg(unix)]
        {
            tokio::select! {
                value = tokio::signal::ctrl_c() => value.map_err(|_|KnowledgeError::Shutdown("signal")),
                value = terminate.recv() => value.ok_or(KnowledgeError::Shutdown("signal")),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .map_err(|_| KnowledgeError::Shutdown("signal"))
        }
    };
    tokio::select! {
        result = &mut server => {
            let stop = tokio::time::timeout(Duration::from_secs(30), lifecycle.stop()).await.map_err(|_|KnowledgeError::Shutdown("lifecycle_timeout"))?;
            result.map_err(|_|KnowledgeError::Shutdown("http_server"))?;
            stop
        }
        signal_result = signal => {
            lifecycle.drain.begin_drain();
            let _ = shutdown.send(());
            let served = tokio::time::timeout(Duration::from_secs(30), &mut server).await;
            let stopped = tokio::time::timeout(Duration::from_secs(30), lifecycle.stop()).await.map_err(|_|KnowledgeError::Shutdown("lifecycle_timeout"))?;
            signal_result?;
            stopped?;
            served.map_err(|_|KnowledgeError::Shutdown("http_drain_timeout"))?
                .map_err(|_|KnowledgeError::Shutdown("http_server"))
        }
    }
}
