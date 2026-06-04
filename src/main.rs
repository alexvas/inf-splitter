use inf_splitter::config::Config;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("inf_splitter=info".parse()?))
        .init();

    let config = Config::load().map_err(|err| {
        eprintln!("configuration error: {err}");
        err
    })?;

    info!(
        listen = %config.listen_addr,
        upstream_timeout_secs = config.upstream_timeout.as_secs(),
        max_request_body = config.max_request_body,
        models = ?config.sorted_model_ids(),
        "starting inf-splitter"
    );

    let listen_addr = config.listen_addr;
    let app = inf_splitter::build_app(config).await?;
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    info!(addr = %listen_addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received Ctrl+C, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
