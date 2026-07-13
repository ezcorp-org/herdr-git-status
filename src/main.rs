//! Git Status — per-space git working-tree status in the herdr sidebar.
//!
//! For every workspace herdr reports, we take the first pane's cwd, run
//! `git status --porcelain=v1 --branch` there, and reduce it to a compact token
//! (`+staged ~modified ?untracked !conflicts`, or `✓` for a fully clean repo
//! with nothing to commit, push, or pull). The background updater surfaces that
//! token as a "git" pseudo-agent in the agents panel (default, stock herdr) or
//! as spaces-card metadata (sidebar mode, patched builds); the read modes print
//! a dashboard / report.
//!
//! Modes (argv flags):
//!   --once            print a single snapshot and exit (used by the action)
//!   --interval N      live watch, refreshing every N seconds (used by the pane)
//!   --json            emit machine-readable JSON and exit
//!   --enable          start the sidebar status updater daemon
//!   --disable         stop the daemon and clear statuses
//!   --toggle          enable/disable depending on daemon state
//!   --daemon          internal: run the updater loop (spawned by --enable)
//!
//! No `/proc` dependency — the status comes entirely from `git`. herdr injects
//! HERDR_BIN_PATH / HERDR_PLUGIN_* / HERDR_SOCKET_PATH.

mod collect;
mod config;
mod daemon;
mod git;
mod herdr;
mod model;
mod render;

use std::process;

/// Crate-wide fallible result; boxed error keeps the scaffold dependency-light.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Default live-watch refresh window when `--interval` is absent or invalid.
const DEFAULT_INTERVAL_MS: u64 = 3000;

fn main() {
    if let Err(err) = run() {
        eprintln!("git-status: {err}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Daemon / control modes manage their own socket connection internally.
    if has_flag(&args, "--daemon") {
        return daemon::run_daemon();
    }
    if has_flag(&args, "--enable") {
        return daemon::enable_updater();
    }
    if has_flag(&args, "--disable") {
        return daemon::disable_updater();
    }
    if has_flag(&args, "--toggle") {
        return daemon::toggle_updater();
    }

    // Read modes share one socket connection.
    let mut client = herdr::connect()?;
    if has_flag(&args, "--json") {
        return render::run_json(&mut client);
    }
    if has_flag(&args, "--once") {
        return render::run_once(&mut client);
    }

    render::run_interval(&mut client, interval_ms(&args))
}

/// True if `flag` appears anywhere in `args`.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Parse `--interval N` (seconds) into milliseconds, falling back to the default
/// for a missing, non-numeric, or non-positive value.
fn interval_ms(args: &[String]) -> u64 {
    args.iter()
        .position(|a| a == "--interval")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|&n| n > 0.0)
        .map(|n| (n * 1000.0) as u64)
        .unwrap_or(DEFAULT_INTERVAL_MS)
}
