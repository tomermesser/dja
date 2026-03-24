use crate::cache::CacheDb;
use crate::config::Config;
use crate::embedding;
use anyhow::Result;

/// Run the `dja init` command.
pub async fn run(global: bool) -> Result<()> {
    // 1. Create config dir and write default config if missing
    Config::ensure_config_dir()?;
    let config_path = Config::config_path();
    if !config_path.exists() {
        let default_config = Config::default();
        let toml_str = toml::to_string_pretty(&default_config)?;
        std::fs::write(&config_path, &toml_str)?;
        println!("Created default config at {}", config_path.display());
    } else {
        println!("Config already exists at {}", config_path.display());
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

    // 5. Print setup instructions
    let config = Config::load()?;
    println!();
    println!("Setup complete! To use dja, set your base URL:");
    println!();
    println!(
        "  export ANTHROPIC_BASE_URL=http://127.0.0.1:{}",
        config.port
    );
    println!();

    // 6. If --global, append to shell profile
    if global {
        append_to_shell_profile(config.port)?;
    } else {
        println!("Tip: run `dja init --global` to add this to your shell profile automatically.");
    }

    Ok(())
}

fn append_to_shell_profile(port: u16) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;

    // Determine shell profile
    let shell = std::env::var("SHELL").unwrap_or_default();
    let profile_path = if shell.contains("zsh") {
        home.join(".zshrc")
    } else {
        home.join(".bashrc")
    };

    let export_line = format!(
        "\n# dja semantic cache proxy\nexport ANTHROPIC_BASE_URL=http://127.0.0.1:{}\n",
        port
    );

    // Check if already present
    if let Ok(contents) = std::fs::read_to_string(&profile_path) {
        if contents.contains("ANTHROPIC_BASE_URL") {
            println!(
                "ANTHROPIC_BASE_URL already set in {}. Skipping.",
                profile_path.display()
            );
            return Ok(());
        }
    }

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&profile_path)?;
    file.write_all(export_line.as_bytes())?;

    println!(
        "Added ANTHROPIC_BASE_URL to {}. Restart your shell or run:",
        profile_path.display()
    );
    println!("  source {}", profile_path.display());

    Ok(())
}
