use crate::cache::CacheDb;
use crate::config::Config;
use crate::embedding::EmbeddingModel;
use crate::p2p::client::PeerClient;
use crate::p2p::index::IndexClient;
use crate::p2p::server::{start_peer_server, PeerServerState};
use crate::config::P2pConfig;
use crate::proxy::handler;
use crate::proxy::inflight::InflightMap;
use crate::proxy::internal;
use crate::proxy::metrics::{self, SessionStats};
use anyhow::{Context, Result};
use libsql;
use axum::routing::get;
use axum::Router;
use std::future::Future;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};

fn get_local_hostname() -> String {
    let mut buf = [0u8; 256];
    let result = unsafe {
        libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len())
    };
    if result == 0 {
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len]).to_string()
    } else {
        "local".to_string()
    }
}

/// Shared state available to all handlers.
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub embedding: Mutex<EmbeddingModel>,
    pub cache: CacheDb,
    pub stats: SessionStats,
    pub event_tx: broadcast::Sender<metrics::RequestEvent>,
    pub inflight: InflightMap,
    /// Hostname of this machine — stored with cache entries as the response source.
    pub hostname: String,
    /// P2P peer client, present when P2P is enabled.
    pub p2p_client: Option<Arc<PeerClient>>,
    /// P2P configuration, present when P2P is enabled.
    pub p2p_config: Option<Arc<P2pConfig>>,
    /// P2P central index client, present when P2P is enabled.
    pub p2p_index: Option<Arc<IndexClient>>,
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

    let (event_tx, _rx) = metrics::event_channel();

    // Build optional P2P state
    let (p2p_client, p2p_config, p2p_index) = if config.p2p.enabled {
        let p2p_cfg = Arc::new(config.p2p.clone());

        // Open a Turso (or local) connection for the central index.
        let index_conn = if p2p_cfg.index_url.is_empty() {
            // Fallback to in-memory DB when no Turso URL is configured (dev/test).
            libsql::Builder::new_local(":memory:")
                .build()
                .await
                .context("Failed to open in-memory index DB")?
                .connect()
                .context("Failed to connect to in-memory index DB")?
        } else {
            libsql::Builder::new_remote(p2p_cfg.index_url.clone(), p2p_cfg.index_token.clone())
                .build()
                .await
                .context("Failed to open Turso index DB")?
                .connect()
                .context("Failed to connect to Turso index DB")?
        };

        let index = Arc::new(IndexClient::new(index_conn));
        (Some(Arc::new(PeerClient::new())), Some(p2p_cfg), Some(index))
    } else {
        (None, None, None)
    };

    let state = Arc::new(AppState {
        config,
        http_client,
        embedding: Mutex::new(embedding_model),
        cache,
        stats: SessionStats::new(),
        event_tx,
        inflight: InflightMap::new(),
        hostname: get_local_hostname(),
        p2p_client,
        p2p_config: p2p_config.clone(),
        p2p_index,
    });

    // Spawn peer server as a background task when P2P is enabled
    if let Some(cfg) = p2p_config {
        let peer_state = PeerServerState {
            db: Arc::new(
                // We need to open a second handle to the DB for the peer server.
                // For now, share an in-memory-compatible approach: open a new
                // connection to the same on-disk path.
                CacheDb::open(&Config::data_dir().join("cache.db"))
                    .await
                    .context("Failed to open cache database for peer server")?,
            ),
            config: cfg,
        };
        tokio::spawn(async move {
            if let Err(e) = start_peer_server(peer_state).await {
                tracing::error!("Peer server exited with error: {e}");
            }
        });
    }

    let app = Router::new()
        .route("/internal/stats", get(internal::stats_handler))
        .route("/internal/events", get(internal::events_handler))
        .route("/internal/p2p/friends", get(internal::p2p_friends_handler))
        .fallback(handler::proxy_handler)
        .layer(tower::limit::ConcurrencyLimitLayer::new(20))
        .with_state(state);

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    Ok(())
}
