pub mod clear;
pub mod config_cmd;
pub mod export;
pub mod import;
pub mod init;
pub mod log;
pub mod monitor;
pub mod start;
pub mod stats;
pub mod test_cmd;
pub mod verify;

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
    if result != 0 {
        anyhow::bail!("Failed to send stop signal to PID {pid}");
    }

    println!("Sent stop signal to dja (PID {pid}).");

    // Poll up to 3 seconds to confirm the process has exited.
    let mut exited = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        if !alive {
            exited = true;
            break;
        }
    }

    if exited {
        println!("dja stopped.");
    } else {
        println!("dja did not exit within 3 seconds; it may still be shutting down.");
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
