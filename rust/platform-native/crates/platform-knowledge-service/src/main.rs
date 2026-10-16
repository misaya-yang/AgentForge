//! Independent signed Knowledge service. Configuration is read only on execution.
use ai_platform_knowledge_service::{KnowledgeError, Result, process::ProcessSettings};
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
    let result = runtime.block_on(async {
        let _provider = ai_platform_knowledge_service::tracing::initialize();
        let result = run().await;
        let _ =
            tokio::task::spawn_blocking(ai_platform_gateway::distributed_tracing::shutdown).await;
        result
    });
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
    let rpc = settings.rpc.clone();
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
    let rpc_listener = match rpc.address {
        Some(address) => match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => Some(listener),
            Err(_) => {
                lifecycle.stop().await.ok();
                return Err(KnowledgeError::Startup("internal_rpc_listener_bind"));
            }
        },
        None => None,
    };
    // Worker mode serves health only; All/API expose the same signed HTTP routes.
    // Register SIGTERM before startup so container shutdown isn't lost.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| KnowledgeError::Startup("signal_registration"))?;
    lifecycle.start().await?;
    eprintln!("knowledge-service started with role {role}");
    let (shutdown, receive_shutdown) = tokio::sync::watch::channel(false);
    let mut servers = tokio::task::JoinSet::new();
    if let Some(listener) = rpc_listener {
        let rpc_router = router.clone();
        let receive = receive_shutdown.clone();
        servers.spawn(async move {
            ai_platform_knowledge_service::rpc::serve(listener, rpc_router, rpc, receive).await
        });
    }
    servers.spawn(async move {
        let mut receive = receive_shutdown;
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            if *receive.borrow() {
                return;
            }
            while receive.changed().await.is_ok() {
                if *receive.borrow() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| KnowledgeError::Shutdown("http_server"))
    });
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
    let first = tokio::select! {
        result = servers.join_next() => match result {
            Some(Ok(result)) => result,
            _ => Err(KnowledgeError::Shutdown("server_task")),
        },
        signal_result = signal => signal_result,
    };
    lifecycle.drain.begin_drain();
    let _ = shutdown.send(true);
    let served = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(result) = servers.join_next().await {
            result.map_err(|_| KnowledgeError::Shutdown("server_task"))??;
        }
        Ok::<_, KnowledgeError>(())
    })
    .await;
    if served.is_err() {
        servers.abort_all();
    }
    let stopped = tokio::time::timeout(Duration::from_secs(30), lifecycle.stop())
        .await
        .map_err(|_| KnowledgeError::Shutdown("lifecycle_timeout"))?;
    first?;
    stopped?;
    served.map_err(|_| KnowledgeError::Shutdown("server_drain_timeout"))?
}
