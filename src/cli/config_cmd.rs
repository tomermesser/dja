use crate::config::Config;
use anyhow::{bail, Result};

/// Run the `dja config` command.
pub fn run(key: Option<String>, value: Option<String>) -> Result<()> {
    match (key, value) {
        // No args: print all config
        (None, None) => {
            let config = Config::load()?;
            let toml_str = toml::to_string_pretty(&config)?;
            print!("{}", toml_str);
        }
        // Key only: print that field
        (Some(key), None) => {
            let config = Config::load()?;
            let val = get_config_field(&config, &key)?;
            println!("{}", val);
        }
        // Key + value: update field
        (Some(key), Some(value)) => {
            update_config_field(&key, &value)?;
            println!("Updated {} = {}", key, value);
        }
        // Value without key shouldn't happen with clap, but handle gracefully
        (None, Some(_)) => {
            bail!("Cannot set a value without specifying a key");
        }
    }

    Ok(())
}

fn get_config_field(config: &Config, key: &str) -> Result<String> {
    match key {
        "port" => Ok(config.port.to_string()),
        "upstream" => Ok(config.upstream.clone()),
        "threshold" => Ok(config.threshold.to_string()),
        "ttl" => Ok(config.ttl.clone()),
        "max_entries" => Ok(config.max_entries.to_string()),
        "max_response_size" => Ok(config.max_response_size.to_string()),
        "log_level" => Ok(config.log_level.clone()),
        _ => bail!("Unknown config key: '{}'. Valid keys: port, upstream, threshold, ttl, max_entries, max_response_size, log_level", key),
    }
}

fn update_config_field(key: &str, value: &str) -> Result<()> {
    Config::ensure_config_dir()?;
    let config_path = Config::config_path();

    // Load existing config or defaults
    let mut config = Config::load()?;

    // Update the field
    match key {
        "port" => {
            config.port = value.parse().map_err(|_| anyhow::anyhow!("invalid port: {}", value))?;
        }
        "upstream" => {
            config.upstream = value.to_string();
        }
        "threshold" => {
            config.threshold = value
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid threshold: {}", value))?;
        }
        "ttl" => {
            config.ttl = value.to_string();
        }
        "max_entries" => {
            config.max_entries = value
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid max_entries: {}", value))?;
        }
        "max_response_size" => {
            config.max_response_size = value
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid max_response_size: {}", value))?;
        }
        "log_level" => {
            config.log_level = value.to_string();
        }
        _ => bail!("Unknown config key: '{}'. Valid keys: port, upstream, threshold, ttl, max_entries, max_response_size, log_level", key),
    }

    // Validate before writing
    config.validate()?;

    // Write back
    let toml_str = toml::to_string_pretty(&config)?;
    std::fs::write(&config_path, toml_str)?;

    Ok(())
}
