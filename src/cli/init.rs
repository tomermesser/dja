use crate::cache::CacheDb;
use crate::config::Config;
use crate::embedding;
use anyhow::Result;
use uuid::Uuid;

/// Run the `dja init` command.
pub async fn run(global: bool) -> Result<()> {
    use std::io::{self, Write};

    // 1. Create config dir and write default config if missing
    Config::ensure_config_dir()?;
    let config_path = Config::config_path();

    if !config_path.exists() {
        let mut config = Config::default();

        // Auto-generate a unique peer_id for this machine
        config.p2p.peer_id = format!("dja_{}", &Uuid::new_v4().to_string()[..8]);

        // Prompt for a display name
        print!("Enter a display name for this peer (e.g. \"MacBook Pro\"): ");
        io::stdout().flush()?;
        let mut name = String::new();
        io::stdin().read_line(&mut name)?;
        let name = name.trim().to_string();
        config.p2p.display_name = if !name.is_empty() {
            name
        } else {
            config.p2p.peer_id.clone()
        };

        // Auto-detect local IP for public_addr
        if let Some(ip) = detect_local_ip() {
            config.p2p.public_addr = format!("{}:{}", ip, config.p2p.listen_port);
            println!("Detected local IP: {} (update public_addr in config if using Tailscale)", ip);
        }

        // Enable P2P by default
        config.p2p.enabled = true;

        let toml_str = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, &toml_str)?;
        println!("Created config at {}", config_path.display());
        println!("Peer ID:      {}", config.p2p.peer_id);
        println!("Display name: {}", config.p2p.display_name);
        println!("Public addr:  {}", config.p2p.public_addr);
        println!("Index:        {}", config.p2p.index_url);
    } else {
        // Backfill any missing P2P fields (existing installs upgrading to P2P)
        let mut config = Config::load()?;
        let mut changed = false;

        if config.p2p.peer_id.is_empty() {
            config.p2p.peer_id = format!("dja_{}", &Uuid::new_v4().to_string()[..8]);
            changed = true;
        }
        if config.p2p.display_name.is_empty() {
            config.p2p.display_name = config.p2p.peer_id.clone();
            changed = true;
        }
        if config.p2p.public_addr.is_empty() {
            if let Some(ip) = detect_local_ip() {
                config.p2p.public_addr = format!("{}:{}", ip, config.p2p.listen_port);
                println!("Detected local IP: {}", ip);
                changed = true;
            }
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
            println!("Updated config at {}", config_path.display());
        } else {
            println!("Config already exists at {}", config_path.display());
        }

        println!("Peer ID:      {}", config.p2p.peer_id);
        println!("Display name: {}", config.p2p.display_name);
        println!("Public addr:  {}", config.p2p.public_addr);
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

    // 5. Install shell integration (always, unless already present)
    let config = Config::load()?;
    println!();
    install_shell_integration(config.port, global)?;

    Ok(())
}

/// Detect the machine's primary outbound local IP using a UDP socket trick.
/// No data is actually sent — the OS just determines which interface would be used.
fn detect_local_ip() -> Option<String> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// Installs a shell function wrapper into the user's shell profile.
///
/// The wrapper intercepts `dja start` and `dja stop` to automatically
/// set/unset ANTHROPIC_BASE_URL in the current shell — something a
/// background daemon process cannot do on its own.
fn install_shell_integration(port: u16, explicit: bool) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;

    let shell = std::env::var("SHELL").unwrap_or_default();
    let profile_path = if shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bashrc")
    };

    let marker = "# dja shell integration";

    if let Ok(contents) = std::fs::read_to_string(&profile_path) {
        if contents.contains(marker) {
            println!("Shell integration already installed in {}.", profile_path.display());
            println!("Run `source {}` if you haven't reloaded your shell yet.", profile_path.display());
            return Ok(());
        }
    }

    // If not explicitly requested with --global, prompt the user
    if !explicit {
        print!("Install shell integration? (auto-sets ANTHROPIC_BASE_URL on `dja start`) [Y/n]: ");
        use std::io::{self, Write};
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().to_lowercase() == "n" {
            println!("Skipped. Run `dja init --global` later to install.");
            println!("Setup complete!");
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
        echo "dja: ANTHROPIC_BASE_URL=http://127.0.0.1:{port}"
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
    println!("Activate now with:");
    println!("  source {}", profile_path.display());
    println!();
    println!("After that, `dja start` sets ANTHROPIC_BASE_URL automatically.");

    Ok(())
}
