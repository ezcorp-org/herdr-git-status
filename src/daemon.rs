//! Sidebar status updater daemon and its enable/disable/toggle controls.
//!
//! The daemon refreshes each space's git status on a cadence, surfacing it either
//! as a "git" pseudo-agent (agents-panel mode) or as TTL'd display-only metadata
//! (sidebar mode). A pid file under the state dir enforces a single instance;
//! statuses self-clear via their TTL if the daemon dies. A fully clean repo
//! (nothing to commit, push, or pull) reports `✓` unless `clean_checkmark =
//! false`; a space with an empty status (non-repo, or clean-but-diverged) has
//! any previous status actively cleared so no stale row lingers. `enable` /
//! `disable` / `toggle` spawn or signal that daemon and sweep leftover statuses.

use std::collections::{HashMap, HashSet};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::collect::{self, PSEUDO_AGENT};
use crate::config::{self, Config, Mode};
use crate::git;
use crate::herdr::{self, Herdr};
use crate::model::Space;

/// Named metadata tokens the updater pushes, one per [`git::Severity`], so herdr
/// can colour each independently via its per-token sidebar style — green clean,
/// yellow dirty, red conflict. herdr renders a custom token's value as flat text
/// (no ANSI, no content-based colour), so severity has to live in the token
/// *name*: exactly one is set per space at a time and the other two are cleared.
/// Reference them in herdr's `config.toml` as `$git_clean` / `$git_dirty` /
/// `$git_conflict`.
pub const TOKEN_CLEAN: &str = "git_clean";
pub const TOKEN_DIRTY: &str = "git_dirty";
pub const TOKEN_CONFLICT: &str = "git_conflict";
/// Every status token name, for clearing the inactive ones (and sweeping on
/// shutdown/disable without needing to know which was active).
pub const STATUS_TOKENS: [&str; 3] = [TOKEN_CLEAN, TOKEN_DIRTY, TOKEN_CONFLICT];

/// The status token a non-empty status for `g` is pushed onto, chosen by
/// [`git::severity`] so the colour matches the terminal dashboard's cell.
fn status_token_name(g: &crate::model::GitStatus) -> &'static str {
    match git::severity(g) {
        git::Severity::Conflict => TOKEN_CONFLICT,
        git::Severity::Dirty => TOKEN_DIRTY,
        git::Severity::Clean => TOKEN_CLEAN,
    }
}

/// Panes/workspaces we have pushed status onto this run, so shutdown can clear
/// them. The stored value is the currently-set status token, so a severity flip
/// (e.g. dirty → conflict) clears the previous token before setting the new one.
#[derive(Debug, Default)]
pub struct Tracked {
    /// Panes carrying our pseudo-agent (released, not TTL'd).
    pub pseudo: HashSet<String>,
    /// Pane id → its active TTL'd status token (agents-panel mode).
    pub metadata: HashMap<String, &'static str>,
    /// Workspace id → its active TTL'd status token (sidebar mode → the spaces
    /// card, which renders workspace tokens rather than pane tokens).
    pub workspaces: HashMap<String, &'static str>,
}

/// PID of a live updater daemon, or `None` (missing pid file / dead process).
///
/// Reads `<state_dir>/updater.pid` and probes the process with `kill(pid, 0)`
/// (signal 0 checks existence only). Any failure — no file, unparsable content,
/// a non-positive pid, or a dead/unsignalable process — reads as `None`.
pub fn daemon_pid() -> Option<u32> {
    let text = std::fs::read_to_string(config::pid_file()).ok()?;
    let pid: i32 = text.trim().parse().ok()?;
    // SAFETY: `kill` with signal 0 performs no delivery, only a liveness probe.
    if pid > 0 && unsafe { libc::kill(pid, 0) } == 0 {
        Some(pid as u32)
    } else {
        None
    }
}

/// `--daemon`: run the updater loop until signalled, then clear and exit.
///
/// Single-instance via the pid file; a signal-hook thread performs the SIGINT/
/// SIGTERM shutdown (clear tracked statuses, unlink pid, `exit(0)`) over its own
/// socket connection so it need not wait on the main loop's sleep. The loop
/// shuts down after five consecutive failures (herdr server likely gone).
pub fn run_daemon() -> crate::Result<()> {
    if daemon_pid().is_some() {
        return Ok(()); // another updater is already live
    }
    std::fs::create_dir_all(config::state_dir())?;
    std::fs::write(config::pid_file(), format!("{}\n", std::process::id()))?;

    let config = config::load_config();

    let mut client = match herdr::connect() {
        Ok(client) => client,
        Err(err) => {
            // Nothing to run without a host connection — don't leave a pid file
            // pointing at a process that is about to exit.
            let _ = std::fs::remove_file(config::pid_file());
            return Err(err);
        }
    };

    let stopping = Arc::new(AtomicBool::new(false));
    let tracked = Arc::new(Mutex::new(Tracked::default()));

    // Signal thread: on the first SIGINT/SIGTERM, win the shutdown race and clear
    // everything via a fresh connection, then exit. The main loop must not
    // re-report after this runs, so it parks once it observes `stopping`.
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    {
        let stopping = Arc::clone(&stopping);
        let tracked = Arc::clone(&tracked);
        thread::spawn(move || {
            if signals.forever().next().is_some() && !stopping.swap(true, Ordering::SeqCst) {
                shutdown(herdr::connect().ok().as_mut(), &tracked);
            }
        });
    }

    let interval = Duration::from_secs(config.interval_seconds);
    let mut failures: u32 = 0;
    loop {
        match collect::snapshot(&mut client) {
            Ok(spaces) => {
                if stopping.load(Ordering::SeqCst) {
                    park(); // shutdown ran during the sample — do not re-report
                }
                {
                    let mut guard = tracked.lock().expect("tracked mutex poisoned");
                    push_statuses(&mut client, &spaces, &config, &mut guard);
                }
                failures = 0;
                thread::sleep(interval);
            }
            Err(_) => {
                failures += 1;
                if failures >= 5 && !stopping.swap(true, Ordering::SeqCst) {
                    shutdown(Some(&mut client), &tracked); // herdr server likely gone
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
        if stopping.load(Ordering::SeqCst) {
            park();
        }
    }
}

/// `--enable`: spawn a detached `--daemon` process (no-op if already running).
pub fn enable_updater() -> crate::Result<()> {
    if daemon_pid().is_some() {
        notify("git status updater already enabled");
        return Ok(());
    }

    // Re-exec ourselves as the daemon, fully detached: a new session (setsid) so
    // it survives the controlling terminal, and null stdio.
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and the only action taken in the
    // forked child before exec; it starts a new session, detaching the daemon.
    unsafe {
        cmd.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    cmd.spawn()?; // do not wait — the child outlives us and is reaped by init

    notify("git status updater enabled");
    Ok(())
}

/// `--disable`: signal the daemon and sweep any leftover statuses.
pub fn disable_updater() -> crate::Result<()> {
    if let Some(pid) = daemon_pid() {
        // The daemon clears its own statuses on shutdown; best-effort.
        // SAFETY: `kill` merely posts SIGTERM to the pid; failure is ignored.
        unsafe {
            libc::kill(pid as i32, SIGTERM);
        }
    }

    // Belt and braces: sweep every current pane in case the daemon died — release
    // pseudo-agents (no TTL) and clear metadata statuses. If herdr is
    // unavailable, metadata TTLs expire the statuses anyway.
    if let Ok(mut client) = herdr::connect() {
        if let Ok(spaces) = collect::collect_spaces(&mut client) {
            let mut sweep = Tracked::default();
            for sp in &spaces {
                sweep.pseudo.extend(sp.pseudo_panes.iter().cloned());
                // The stored token is a don't-care here: clear_all sweeps every
                // STATUS_TOKENS name per id, so a dead daemon's stale token of
                // any severity is cleared regardless of which we record.
                for pane in sp.agent_panes.iter().chain(&sp.spare_panes) {
                    sweep.metadata.insert(pane.clone(), TOKEN_DIRTY);
                }
                sweep.workspaces.insert(sp.id.clone(), TOKEN_DIRTY);
            }
            clear_all(&mut client, &sweep);
        }
    }

    notify("git status updater disabled");
    Ok(())
}

/// `--toggle`: disable if a daemon is live, else enable.
pub fn toggle_updater() -> crate::Result<()> {
    if daemon_pid().is_some() {
        disable_updater()
    } else {
        enable_updater()
    }
}

/// The status string pushed for a space: [`git::status_token`] — the dirty
/// token, or `✓` for a fully clean repo — unless the user opted out via
/// `clean_checkmark = false`, which restores the dirty-token-only behaviour.
fn space_status(sp: &Space, config: &Config) -> String {
    if config.clean_checkmark {
        git::status_token(&sp.git)
    } else {
        git::token(&sp.git)
    }
}

/// Push each space's git status onto a pane, mode-dependent, recording the
/// touched panes in `tracked`.
///
/// A space with an empty status — non-repo, clean-but-diverged, or any clean
/// tree when `clean_checkmark = false` — has its row withdrawn: in sidebar mode
/// any status we previously set is cleared; in agents-panel mode the
/// pseudo-agent is released. Everything else reports its status — the dirty
/// token, or `✓` for a fully clean repo.
///
/// The two modes are strictly separate about WHERE they write: agents-panel mode
/// only ever claims a spare pane as a "git" pseudo-agent and never writes
/// spaces-card metadata (a space with no spare pane simply shows nothing, so it
/// can't collide with a status-row plugin like space-usage); sidebar mode writes
/// TTL'd metadata on the first spare (else agent) pane.
pub fn push_statuses(client: &mut Herdr, spaces: &[Space], config: &Config, tracked: &mut Tracked) {
    let source = config::plugin_id();
    let ttl_ms = config.interval_seconds * 1000 * 3;

    for sp in spaces {
        let status = space_status(sp, config);

        if config.mode == Mode::AgentsPanel {
            // Drop stale claims from earlier runs so a space keeps one entry.
            for extra in sp.pseudo_panes.iter().skip(1) {
                release_pseudo(client, extra, &source, tracked);
            }
            // agents-panel mode ONLY claims a spare pane as a "git" pseudo-agent;
            // it must NEVER write spaces-card metadata. A space with no spare
            // (agent-only) pane therefore surfaces nothing here — otherwise the
            // fallback would write onto an agent pane and fight the space-usage
            // plugin for the single status row, and the two would flip-flop every
            // refresh. So this branch always ends the iteration; the metadata
            // block below is sidebar-mode only.
            if let Some(pane) = sp.pseudo_panes.first().or_else(|| sp.spare_panes.first()) {
                if status.is_empty() {
                    release_pseudo(client, pane, &source, tracked); // clean → no entry
                    if let Some(prev) = tracked.metadata.remove(pane) {
                        let _ = client.clear_metadata_status(pane, &source, prev);
                    }
                } else if client
                    .report_agent(pane, &source, PSEUDO_AGENT, "idle")
                    .is_ok()
                {
                    tracked.pseudo.insert(pane.clone());
                    // 0.7.5: report_agent only claims the entry; the status text
                    // rides a severity-named token (`$git_clean` / `$git_dirty` /
                    // `$git_conflict`) so herdr can colour it. Named tokens are
                    // independent, so this never collides with the space-usage
                    // plugin's `usage` token.
                    let token = status_token_name(&sp.git);
                    clear_stale_pane_token(client, pane, &source, token, tracked);
                    if client
                        .report_metadata_status(pane, &source, token, &status, ttl_ms)
                        .is_ok()
                    {
                        tracked.metadata.insert(pane.clone(), token);
                    }
                }
                // A report_agent failure means the pane just closed; do nothing
                // this cycle (the next refresh re-evaluates) rather than falling
                // back to metadata on an agent pane.
            }
            continue;
        }

        // sidebar mode: release pseudo-agents left over from agents-panel mode.
        for pane_id in &sp.pseudo_panes {
            release_pseudo(client, pane_id, &source, tracked);
        }

        // 0.7.5: the spaces card renders WORKSPACE tokens (`[ui.sidebar.spaces]`
        // `$git_clean` / `$git_dirty` / `$git_conflict`), not pane tokens — so
        // report at the workspace level.
        if status.is_empty() {
            // clean → clear a token we previously set (idempotent; skip if we
            // never set one, so always-clean spaces cost no extra calls).
            if let Some(prev) = tracked.workspaces.remove(&sp.id) {
                let _ = client.workspace_clear_metadata(&sp.id, &source, prev);
            }
        } else {
            let token = status_token_name(&sp.git);
            clear_stale_workspace_token(client, &sp.id, &source, token, tracked);
            if client
                .workspace_report_metadata(&sp.id, &source, token, &status, ttl_ms)
                .is_ok()
            {
                tracked.workspaces.insert(sp.id.clone(), token);
            }
        }
    }
}

/// Release every pseudo-agent and clear every status token in `tracked`.
///
/// Each tracked pane/workspace gets ALL [`STATUS_TOKENS`] cleared, not just the
/// one recorded as active — cheap, idempotent, and robust to a severity that
/// flipped right before shutdown or a stale token left by a crashed prior run.
pub fn clear_all(client: &mut Herdr, tracked: &Tracked) {
    let source = config::plugin_id();
    for pane_id in &tracked.pseudo {
        let _ = client.release_agent(pane_id, &source, PSEUDO_AGENT);
    }
    for pane_id in tracked.metadata.keys() {
        for token in STATUS_TOKENS {
            let _ = client.clear_metadata_status(pane_id, &source, token);
        }
    }
    for workspace_id in tracked.workspaces.keys() {
        for token in STATUS_TOKENS {
            let _ = client.workspace_clear_metadata(workspace_id, &source, token);
        }
    }
}

/// Clear a pane's previously-set status token when this cycle's severity picks a
/// different one, so a dirty → conflict flip never leaves two tokens lit.
fn clear_stale_pane_token(
    client: &mut Herdr,
    pane: &str,
    source: &str,
    token: &str,
    tracked: &mut Tracked,
) {
    if let Some(prev) = tracked.metadata.get(pane).copied() {
        if prev != token {
            let _ = client.clear_metadata_status(pane, source, prev);
        }
    }
}

/// Workspace-level counterpart of [`clear_stale_pane_token`] (sidebar mode).
fn clear_stale_workspace_token(
    client: &mut Herdr,
    workspace_id: &str,
    source: &str,
    token: &str,
    tracked: &mut Tracked,
) {
    if let Some(prev) = tracked.workspaces.get(workspace_id).copied() {
        if prev != token {
            let _ = client.workspace_clear_metadata(workspace_id, source, prev);
        }
    }
}

// ---- helpers ----------------------------------------------------------------

/// Clear tracked statuses, unlink the pid file, and `exit(0)`.
///
/// Shared by the signal thread (own connection) and the five-failure path (main
/// connection). `client` is `None` only when no socket could be opened, in which
/// case the pid file is still removed before exiting. Never returns.
fn shutdown(client: Option<&mut Herdr>, tracked: &Mutex<Tracked>) -> ! {
    if let Some(client) = client {
        if let Ok(tracked) = tracked.lock() {
            clear_all(client, &tracked);
        }
    }
    let _ = std::fs::remove_file(config::pid_file());
    std::process::exit(0);
}

/// Idle forever while the signal thread completes its shutdown and `exit(0)`s the
/// whole process; keeps the main loop from re-reporting or racing that exit.
fn park() -> ! {
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// Best-effort release of our pseudo-agent on `pane_id` (a closed pane errors and
/// is ignored — nothing to release); drops it from `tracked` so shutdown won't
/// re-clear it.
fn release_pseudo(client: &mut Herdr, pane_id: &str, source: &str, tracked: &mut Tracked) {
    let _ = client.release_agent(pane_id, source, PSEUDO_AGENT);
    tracked.pseudo.remove(pane_id);
}

/// Best-effort "Git status" toast over a throwaway connection.
fn notify(body: &str) {
    if let Ok(mut client) = herdr::connect() {
        let _ = client.notification_show("Git status", body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GitStatus;

    fn dirty_space(id: &str, spare: &[&str]) -> Space {
        Space {
            id: id.to_string(),
            label: id.to_string(),
            spare_panes: spare.iter().map(|s| s.to_string()).collect(),
            git: GitStatus {
                is_repo: true,
                modified: 2,
                ..GitStatus::default()
            },
            ..Default::default()
        }
    }

    fn fully_clean_space(id: &str) -> Space {
        Space {
            id: id.to_string(),
            label: id.to_string(),
            git: GitStatus {
                is_repo: true,
                ..GitStatus::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn dirty_space_produces_a_token() {
        let sp = dirty_space("repo", &["pane-1"]);
        assert_eq!(space_status(&sp, &Config::default()), "~2");
    }

    #[test]
    fn status_token_name_maps_severity_to_a_coloured_token() {
        // Fully clean (the ✓) → the green clean token.
        let clean = fully_clean_space("repo");
        assert_eq!(status_token_name(&clean.git), TOKEN_CLEAN);
        // Dirty, no conflicts → the yellow dirty token.
        let dirty = dirty_space("repo", &[]);
        assert_eq!(status_token_name(&dirty.git), TOKEN_DIRTY);
        // A conflict → the red conflict token, outranking other changes.
        let conflict = Space {
            git: GitStatus {
                is_repo: true,
                staged: 1,
                conflicts: 2,
                ..GitStatus::default()
            },
            ..Default::default()
        };
        assert_eq!(status_token_name(&conflict.git), TOKEN_CONFLICT);
    }

    #[test]
    fn status_tokens_are_the_three_severity_names() {
        assert_eq!(STATUS_TOKENS, [TOKEN_CLEAN, TOKEN_DIRTY, TOKEN_CONFLICT]);
    }

    #[test]
    fn fully_clean_space_pushes_check_mark_by_default() {
        let sp = fully_clean_space("repo");
        assert_eq!(space_status(&sp, &Config::default()), git::CLEAN_MARK);
    }

    #[test]
    fn clean_checkmark_opt_out_restores_empty_status() {
        let sp = fully_clean_space("repo");
        let config = Config {
            clean_checkmark: false,
            ..Config::default()
        };
        assert!(space_status(&sp, &config).is_empty());
    }

    #[test]
    fn clean_but_diverged_space_status_is_empty() {
        // herdr's branch row owns the ↑/↓ story; the status is withdrawn, not ✓.
        let mut sp = fully_clean_space("repo");
        sp.git.ahead = 2;
        assert!(space_status(&sp, &Config::default()).is_empty());
    }

    #[test]
    fn non_repo_space_status_is_empty() {
        let sp = Space::default(); // is_repo = false
        assert!(space_status(&sp, &Config::default()).is_empty());
    }
}
