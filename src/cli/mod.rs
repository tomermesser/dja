pub mod start;

use crate::config::Config;
use anyhow::{Context, Result};
use std::fs;

/// Stop the running daemon by sending SIGTERM to the PID in the PID file.
pub fn stop() -> Result<()> {
    let pid_path = Config::pid_path();

    if !pid_path.exists() {
        println!("dja is not running (no PID file found).");
        return Ok(());
    }

    let pid_str = fs::read_to_string(&pid_path).context("reading PID file")?;
    let pid: i32 = pid_str
        .trim()
        .parse()
        .context("parsing PID from PID file")?;

    // Check if process is alive
    let alive = unsafe { libc::kill(pid, 0) == 0 };
    if !alive {
        // Stale PID file — clean up
        fs::remove_file(&pid_path).ok();
        println!("dja is not running (stale PID file cleaned up).");
        return Ok(());
    }

    // Send SIGTERM
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result == 0 {
        println!("Sent stop signal to dja (PID {pid}).");
        // Give it a moment, then clean PID file if still present
        fs::remove_file(&pid_path).ok();
    } else {
        anyhow::bail!("Failed to send stop signal to PID {pid}");
    }

    Ok(())
}

/// Check the status of the daemon.
pub fn status() -> Result<()> {
    let pid_path = Config::pid_path();

    if !pid_path.exists() {
        println!("dja is not running.");
        return Ok(());
    }

    let pid_str = fs::read_to_string(&pid_path).context("reading PID file")?;
    let pid: i32 = pid_str
        .trim()
        .parse()
        .context("parsing PID from PID file")?;

    let alive = unsafe { libc::kill(pid, 0) == 0 };
    if alive {
        println!("dja is running (PID {pid}).");
    } else {
        fs::remove_file(&pid_path).ok();
        println!("dja is not running (stale PID file cleaned up).");
    }

    Ok(())
}
