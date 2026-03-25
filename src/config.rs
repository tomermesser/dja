use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 9842,
            upstream: "https://api.anthropic.com".to_string(),
            threshold: 0.95,
            ttl: "30d".to_string(),
            max_entries: 10000,
            max_response_size: 102400,
            log_level: "info".to_string(),
            match_system_prompt: false,
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
        assert_eq!(config.max_response_size, 102400);
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
}
