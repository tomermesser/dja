use crate::cache::CacheDb;
use crate::config::Config;
use crate::embedding::EmbeddingModel;
use crate::proxy::handler;
use anyhow::{Context, Result};
use axum::Router;
use std::future::Future;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Shared state available to all handlers.
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub embedding: Mutex<EmbeddingModel>,
    pub cache: CacheDb,
}

/// Start the proxy server and run until the shutdown signal fires.
pub async fn run(config: Config, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
    let addr = format!("127.0.0.1:{}", config.port);

    let http_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Load embedding model
    let model_dir = Config::data_dir().join("models").join("all-MiniLM-L6-v2");
    let embedding_model = EmbeddingModel::load(&model_dir)
        .with_context(|| format!("Failed to load embedding model from {}", model_dir.display()))?;
    tracing::info!("Loaded embedding model from {}", model_dir.display());

    // Open cache database
    let db_path = Config::data_dir().join("cache.db");
    let cache = CacheDb::open(&db_path)
        .await
        .with_context(|| format!("Failed to open cache database at {}", db_path.display()))?;
    tracing::info!("Opened cache database at {}", db_path.display());

    let state = Arc::new(AppState {
        config,
        http_client,
        embedding: Mutex::new(embedding_model),
        cache,
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
