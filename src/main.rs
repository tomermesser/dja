mod cache;
mod cli;
mod config;
mod embedding;
mod proxy;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dja", version, about = "Semantic cache proxy for AI coding tools")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the proxy daemon
    Start,
    /// Stop the proxy daemon
    Stop,
    /// Show daemon status
    Status,
    /// Initialize dja: config, model, database
    Init {
        /// Add ANTHROPIC_BASE_URL export to shell profile
        #[arg(long)]
        global: bool,
    },
    /// Show cache statistics
    Stats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Clear cache entries
    Clear {
        /// Delete entries older than duration (e.g. "30d", "24h")
        #[arg(long)]
        older_than: Option<String>,
    },
    /// View or modify configuration
    Config {
        /// Config key to get or set
        key: Option<String>,
        /// Value to set (requires key)
        value: Option<String>,
    },
    /// Test embedding and cache lookup for a prompt
    Test {
        /// The prompt text to test
        prompt: String,
    },
    /// Show recent log output
    Log,
    /// Verify installation health
    Verify,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start => cli::start::run().await?,
        Commands::Stop => cli::stop()?,
        Commands::Status => cli::status()?,
        Commands::Init { global } => cli::init::run(global).await?,
        Commands::Stats { json } => cli::stats::run(json).await?,
        Commands::Clear { older_than } => cli::clear::run(older_than).await?,
        Commands::Config { key, value } => cli::config_cmd::run(key, value)?,
        Commands::Test { prompt } => cli::test_cmd::run(prompt).await?,
        Commands::Log => cli::log::run()?,
        Commands::Verify => cli::verify::run()?,
    }

    Ok(())
}
