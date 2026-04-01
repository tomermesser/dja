use std::sync::Arc;
use std::time::Duration;

use super::index::IndexClient;

/// Configuration required by the heartbeat task.
///
/// These fields are a subset of the full P2P config — only what `run_heartbeat`
/// needs so callers don't have to pull in the entire config type.
pub struct P2pConfig {
    pub peer_id: String,
    pub display_name: String,
    pub public_addr: String,
}

/// Spawn a background heartbeat loop that re-registers this peer in the
/// central Turso index every 60 seconds.
///
/// Errors from individual heartbeat calls are silently ignored so that
/// transient network failures (e.g. Turso unavailable) don't crash the proxy.
/// The task runs until the process exits or the `IndexClient` / `P2pConfig`
/// `Arc`s are dropped.
pub async fn run_heartbeat(index: Arc<IndexClient>, config: Arc<P2pConfig>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let _ = index
            .heartbeat(
                &config.peer_id,
                &config.display_name,
                &config.public_addr,
                env!("CARGO_PKG_VERSION"),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p2p_config_fields() {
        let cfg = P2pConfig {
            peer_id: "peer-test".to_string(),
            display_name: "Test Node".to_string(),
            public_addr: "127.0.0.1:9843".to_string(),
        };
        assert_eq!(cfg.peer_id, "peer-test");
        assert_eq!(cfg.display_name, "Test Node");
        assert_eq!(cfg.public_addr, "127.0.0.1:9843");
    }
}
