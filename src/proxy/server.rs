use crate::config::Config;
use crate::proxy::handler;
use anyhow::Result;
use axum::Router;
use std::future::Future;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Shared state available to all handlers.
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
}

/// Start the proxy server and run until the shutdown signal fires.
pub async fn run(config: Config, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
    let addr = format!("127.0.0.1:{}", config.port);

    let http_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let state = Arc::new(AppState {
        config,
        http_client,
    });

    let app = Router::new()
        .fallback(handler::proxy_handler)
        .with_state(state);

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    Ok(())
}
