use crate::config::Config;
use crate::proxy;
use anyhow::{Context, Result};
use std::fs;
use tokio::signal::unix::{signal, SignalKind};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize tracing with both console and file output.
fn init_logging(config: &Config) -> Result<WorkerGuard> {
    Config::ensure_data_dir()?;

    let log_path = Config::log_path();
    let log_dir = log_path.parent().expect("log path has parent");
    let log_filename = log_path.file_name().expect("log path has filename");

    let file_appender = tracing_appender::rolling::never(log_dir, log_filename);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_new(&config.log_level)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(non_blocking),
        )
        .init();

    Ok(guard)
}

/// Write the current process PID to the PID file with an exclusive lock.
/// Returns the locked file handle — the caller must keep it alive so the lock
/// is held for the daemon's lifetime.
fn write_pid_file() -> Result<fs::File> {
    use fs2::FileExt;
    use std::io::Write;

    Config::ensure_data_dir()?;
    let pid = std::process::id();
    let pid_path = Config::pid_path();

    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&pid_path)
        .with_context(|| format!("opening PID file {}", pid_path.display()))?;

    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("Another dja instance is already running"))?;

    write!(file, "{}", pid)
        .with_context(|| format!("writing PID file {}", pid_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pid_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", pid_path.display()))?;
    }
    Ok(file)
}

/// Remove the PID file on shutdown.
fn remove_pid_file() {
    let pid_path = Config::pid_path();
    fs::remove_file(&pid_path).ok();
}

/// Check if a daemon is already running, cleaning stale PID files.
fn check_already_running() -> Result<bool> {
    let pid_path = Config::pid_path();
    if !pid_path.exists() {
        return Ok(false);
    }

    let pid_str = fs::read_to_string(&pid_path).context("reading PID file")?;
    let pid: i32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            // Invalid PID file — remove it
            fs::remove_file(&pid_path).ok();
            return Ok(false);
        }
    };

    let alive = unsafe { libc::kill(pid, 0) == 0 };
    if alive {
        Ok(true)
    } else {
        // Stale PID file
        fs::remove_file(&pid_path).ok();
        Ok(false)
    }
}

/// Main entry point for the `start` subcommand.
pub async fn run() -> Result<()> {
    let config = Config::load()?;

    if check_already_running()? {
        anyhow::bail!("dja is already running. Use `dja stop` first.");
    }

    // Keep the guard alive for the duration of the process so logs flush.
    let _log_guard = init_logging(&config)?;

    #[cfg(unix)]
    {
        let log_path = Config::log_path();
        if log_path.exists() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("setting permissions on {}", log_path.display()))?;
        }
    }

    // Keep the locked file handle alive so the exclusive lock is held for the
    // daemon's lifetime.  Dropping it would release the lock.
    let _pid_lock = write_pid_file()?;

    tracing::info!("dja starting on 127.0.0.1:{}", config.port);

    // Set up graceful shutdown — remove PID file on Ctrl-C / SIGTERM.
    let shutdown = async {
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to listen for SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
        tracing::info!("Shutdown signal received");
    };

    let result = proxy::server::run(config, shutdown).await;

    remove_pid_file();
    tracing::info!("dja stopped");

    result
}
