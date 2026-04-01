use crate::cache::CacheDb;
use crate::config::Config;
use crate::embedding;
use anyhow::Result;
use uuid::Uuid;

/// Run the `dja init` command.
pub async fn run(global: bool) -> Result<()> {
    // 1. Create config dir and write default config if missing
    Config::ensure_config_dir()?;
    let config_path = Config::config_path();
    if !config_path.exists() {
        let mut default_config = Config::default();

        // Auto-generate a unique peer_id for this machine
        default_config.p2p.peer_id = format!("dja_{}", &Uuid::new_v4().to_string()[..8]);

        // Prompt for a display name
        print!("Enter a display name for this peer (e.g. \"MacBook Pro\"): ");
        use std::io::{self, Write};
        io::stdout().flush()?;
        let mut name = String::new();
        io::stdin().read_line(&mut name)?;
        let name = name.trim().to_string();
        if !name.is_empty() {
            default_config.p2p.display_name = name;
        } else {
            default_config.p2p.display_name = default_config.p2p.peer_id.clone();
        }

        let toml_str = toml::to_string_pretty(&default_config)?;
        std::fs::write(&config_path, &toml_str)?;
        println!("Created config at {}", config_path.display());
        println!("P2P peer ID: {}", default_config.p2p.peer_id);
        println!("Shared index: {}", default_config.p2p.index_url);
    } else {
        // Backfill peer_id if missing (existing installs upgrading to P2P)
        let mut config = Config::load()?;
        let mut changed = false;
        if config.p2p.peer_id.is_empty() {
            config.p2p.peer_id = format!("dja_{}", &Uuid::new_v4().to_string()[..8]);
            changed = true;
        }
        if config.p2p.index_url.is_empty() {
            config.p2p.index_url = env!("DJA_TURSO_URL").to_string();
            changed = true;
        }
        if config.p2p.index_token.is_empty() {
            config.p2p.index_token = env!("DJA_TURSO_TOKEN").to_string();
            changed = true;
        }
        if changed {
            let toml_str = toml::to_string_pretty(&config)?;
            std::fs::write(&config_path, &toml_str)?;
            println!("Updated config with P2P defaults at {}", config_path.display());
        } else {
            println!("Config already exists at {}", config_path.display());
        }
    }

    // 2. Create data dir
    Config::ensure_data_dir()?;
    println!("Data directory: {}", Config::data_dir().display());

    // 3. Download embedding model
    println!("Downloading embedding model (this may take a moment)...");
    let model_dir = embedding::download_model().await?;
    println!("Model ready at {}", model_dir.display());

    // 4. Init database
    let db_path = Config::data_dir().join("cache.db");
    let _db = CacheDb::open(&db_path).await?;
    println!("Database initialized at {}", db_path.display());

    // 5. Install shell integration
    let config = Config::load()?;
    println!();
    if global {
        install_shell_integration(config.port)?;
    } else {
        println!("Setup complete!");
        println!();
        println!("Run `dja init --global` to install shell integration (recommended).");
        println!("This makes ANTHROPIC_BASE_URL auto-set when you run `dja start`");
        println!("and auto-unset when you run `dja stop`.");
    }

    Ok(())
}

/// Installs a shell function wrapper into the user's shell profile.
///
/// The wrapper intercepts `dja start` and `dja stop` to automatically
/// set/unset ANTHROPIC_BASE_URL in the current shell — something a
/// background daemon process cannot do on its own.
fn install_shell_integration(port: u16) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;

    let shell = std::env::var("SHELL").unwrap_or_default();
    let profile_path = if shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bashrc")
    };

    // Marker so we can detect existing installation and avoid duplicates
    let marker = "# dja shell integration";

    if let Ok(contents) = std::fs::read_to_string(&profile_path) {
        if contents.contains(marker) {
            println!("Shell integration already installed in {}.", profile_path.display());
            println!("Restart your shell or run: source {}", profile_path.display());
            return Ok(());
        }
    }

    let snippet = format!(
        r#"
{marker}
dja() {{
  command dja "$@"
  local _exit=$?
  case "$1" in
    start)
      if [ $_exit -eq 0 ]; then
        export ANTHROPIC_BASE_URL=http://127.0.0.1:{port}
        echo "dja: ANTHROPIC_BASE_URL set to http://127.0.0.1:{port}"
      fi
      ;;
    stop)
      unset ANTHROPIC_BASE_URL
      echo "dja: ANTHROPIC_BASE_URL unset"
      ;;
  esac
  return $_exit
}}
"#
    );

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&profile_path)?;
    file.write_all(snippet.as_bytes())?;

    println!("Shell integration installed in {}.", profile_path.display());
    println!();
    println!("Reload your shell to activate:");
    println!("  source {}", profile_path.display());
    println!();
    println!("After that, `dja start` will automatically set ANTHROPIC_BASE_URL");
    println!("and `dja stop` will automatically unset it.");

    Ok(())
}
