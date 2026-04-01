use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

/// Configuration for the P2P cache-sharing feature.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct P2pConfig {
    /// Enable P2P cache sharing. Default: false.
    pub enabled: bool,
    /// Turso URL for the central P2P index database.
    pub index_url: String,
    /// Turso auth token for the central P2P index database.
    pub index_token: String,
    /// Auto-generated UUID identifying this peer.
    pub peer_id: String,
    /// Port this peer's HTTP API listens on. Default: 9843.
    pub listen_port: u16,
    /// Human-readable name for this peer.
    pub display_name: String,
    /// Publicly reachable address for this peer (e.g. "myhost.tailnet:9843").
    pub public_addr: String,
}

impl Default for P2pConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            index_url: String::new(),
            index_token: String::new(),
            peer_id: String::new(),
            listen_port: 9843,
            display_name: String::new(),
            public_addr: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Config {
    /// Port the proxy listens on
    pub port: u16,
    /// Upstream API URL
    pub upstream: String,
    /// Similarity threshold for cache hits (0.0 - 1.0)
    pub threshold: f64,
    /// Cache TTL (e.g. "30d")
    pub ttl: String,
    /// Maximum number of cache entries
    pub max_entries: usize,
    /// Maximum response size to cache (bytes)
    pub max_response_size: usize,
    /// Log level
    pub log_level: String,
    /// Whether to require system_hash match in cache lookups.
    /// Set to true if your system prompts are stable across sessions.
    /// Default false (best for Claude Code, which has dynamic system prompts).
    pub match_system_prompt: bool,
    /// Whether to enable multi-turn caching (cache based on last user message).
    /// Default true.
    pub multi_turn_caching: bool,
    /// Whether to auto-inject Anthropic cache_control breakpoints on forwarded requests.
    /// Default true.
    pub auto_cache_control: bool,
    /// Whether to coalesce identical in-flight requests (singleflight).
    /// Default true.
    pub request_coalescing: bool,
    /// P2P cache-sharing configuration.
    pub p2p: P2pConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 9842,
            upstream: "https://api.anthropic.com".to_string(),
            threshold: 0.95,
            ttl: "30d".to_string(),
            max_entries: 10000,
            max_response_size: 1_048_576,
            log_level: "info".to_string(),
            match_system_prompt: false,
            multi_turn_caching: true,
            auto_cache_control: true,
            request_coalescing: true,
            p2p: P2pConfig::default(),
        }
    }
}

impl Config {
    /// Load config from ~/.config/dja/config.toml, falling back to defaults.
    pub fn load() -> Result<Self> {
        Self::ensure_config_dir()?;
        let path = Self::config_path();

        if path.exists() {
            let contents =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let config: Config =
                toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    /// Return the path to the config file (~/.config/dja/config.toml).
    pub fn config_path() -> PathBuf {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("dja");
        dir.join("config.toml")
    }

    /// Ensure the config directory exists.
    pub fn ensure_config_dir() -> Result<()> {
        let dir = Self::config_path()
            .parent()
            .expect("config path has parent")
            .to_path_buf();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("creating config dir {}", dir.display()))?;
        }
        Ok(())
    }

    /// Return the data directory (~/.local/share/dja/).
    pub fn data_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("dja")
    }

    /// Ensure the data directory exists.
    pub fn ensure_data_dir() -> Result<()> {
        let dir = Self::data_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("creating data dir {}", dir.display()))?;
        }
        Ok(())
    }

    /// Path to the PID file.
    pub fn pid_path() -> PathBuf {
        Self::data_dir().join("dja.pid")
    }

    /// Path to the log file.
    pub fn log_path() -> PathBuf {
        Self::data_dir().join("dja.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.port, 9842);
        assert_eq!(config.upstream, "https://api.anthropic.com");
        assert!((config.threshold - 0.95).abs() < f64::EPSILON);
        assert_eq!(config.ttl, "30d");
        assert_eq!(config.max_entries, 10000);
        assert_eq!(config.max_response_size, 1_048_576);
        assert_eq!(config.log_level, "info");
    }

    #[test]
    fn test_parse_partial_toml() {
        let toml_str = r#"
            port = 8080
            threshold = 0.9
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 8080);
        assert!((config.threshold - 0.9).abs() < f64::EPSILON);
        // Defaults should fill in the rest
        assert_eq!(config.upstream, "https://api.anthropic.com");
        assert_eq!(config.max_entries, 10000);
    }

    #[test]
    fn test_parse_full_toml() {
        let toml_str = r#"
            port = 3000
            upstream = "https://custom.api.com"
            threshold = 0.8
            ttl = "7d"
            max_entries = 5000
            max_response_size = 50000
            log_level = "debug"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 3000);
        assert_eq!(config.upstream, "https://custom.api.com");
        assert!((config.threshold - 0.8).abs() < f64::EPSILON);
        assert_eq!(config.ttl, "7d");
        assert_eq!(config.max_entries, 5000);
        assert_eq!(config.max_response_size, 50000);
        assert_eq!(config.log_level, "debug");
    }

    #[test]
    fn test_default_match_system_prompt_is_false() {
        let config = Config::default();
        assert!(!config.match_system_prompt);
    }

    #[test]
    fn test_parse_match_system_prompt_true() {
        let toml_str = r#"
            match_system_prompt = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.match_system_prompt);
    }

    #[test]
    fn test_default_multi_turn_caching_is_true() {
        let config = Config::default();
        assert!(config.multi_turn_caching);
    }

    #[test]
    fn test_parse_multi_turn_caching_false() {
        let toml_str = r#"
            multi_turn_caching = false
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.multi_turn_caching);
    }

    #[test]
    fn test_default_auto_cache_control_is_true() {
        let config = Config::default();
        assert!(config.auto_cache_control);
    }

    #[test]
    fn test_parse_auto_cache_control_false() {
        let toml_str = r#"
            auto_cache_control = false
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.auto_cache_control);
    }

    #[test]
    fn test_default_p2p_config() {
        let config = Config::default();
        assert!(!config.p2p.enabled);
        assert_eq!(config.p2p.listen_port, 9843);
        assert!(config.p2p.index_url.is_empty());
        assert!(config.p2p.peer_id.is_empty());
    }

    #[test]
    fn test_parse_p2p_config() {
        let toml_str = r#"
            [p2p]
            enabled = true
            index_url = "libsql://mydb.turso.io"
            index_token = "secret"
            peer_id = "550e8400-e29b-41d4-a716-446655440000"
            listen_port = 9844
            display_name = "my-node"
            public_addr = "myhost.tailnet:9844"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.p2p.enabled);
        assert_eq!(config.p2p.index_url, "libsql://mydb.turso.io");
        assert_eq!(config.p2p.index_token, "secret");
        assert_eq!(config.p2p.peer_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(config.p2p.listen_port, 9844);
        assert_eq!(config.p2p.display_name, "my-node");
        assert_eq!(config.p2p.public_addr, "myhost.tailnet:9844");
    }
}
