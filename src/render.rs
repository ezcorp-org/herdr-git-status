//! Human and JSON rendering plus the `--once` / `--json` / `--interval` run modes.
//!
//! [`render`] builds the coloured multi-line terminal report; [`render_json`]
//! builds the machine-readable payload. The `run_*` helpers drive a
//! [`collect::snapshot`](crate::collect::snapshot) and print the result, with
//! `run_interval` clearing and redrawing each frame.

use std::io::{self, IsTerminal, Write};

use serde::Serialize;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::collect;
use crate::git;
use crate::herdr::Herdr;
use crate::model::{GitStatus, Space};

// ---- ANSI styling -----------------------------------------------------------

/// ANSI paint gate: colours only when stdout is a TTY and `NO_COLOR` is unset.
struct Style {
    color: bool,
}

impl Style {
    fn detect() -> Self {
        Style {
            color: io::stdout().is_terminal() && crate::config::non_empty_env("NO_COLOR").is_none(),
        }
    }

    /// Wrap `s` in the SGR `code` when colour is enabled, else return it plain.
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }

    /// Colour a status cell by severity: conflicts red, other changes yellow,
    /// clean green.
    fn status(&self, g: &GitStatus, s: &str) -> String {
        if g.conflicts > 0 {
            self.red(s)
        } else if !g.is_clean() {
            self.yellow(s)
        } else {
            self.green(s)
        }
    }
}

/// Branch display: `"<branch> ↑a ↓b"` (arrows only when non-zero), or
/// `"(no branch)"` for a non-repo / unknown branch.
fn branch_display(g: &GitStatus) -> String {
    if !g.is_repo || g.branch.is_empty() {
        return "(no branch)".to_string();
    }
    let mut out = g.branch.clone();
    if g.ahead > 0 {
        out.push_str(&format!(" ↑{}", g.ahead));
    }
    if g.behind > 0 {
        out.push_str(&format!(" ↓{}", g.behind));
    }
    out
}

/// Status cell text: [`git::status_token`] (compact token, or `"✓"` for a fully
/// clean repo), spelled out as `"clean"` for a clean-but-diverged tree — where
/// the updater pushes nothing — and `"—"` when the space isn't a git repo.
fn status_cell(g: &GitStatus) -> String {
    if !g.is_repo {
        return "—".to_string();
    }
    let token = git::status_token(g);
    if token.is_empty() {
        "clean".to_string()
    } else {
        token
    }
}

// ---- human render -----------------------------------------------------------

/// Format the per-space git-status report as a coloured, multi-line string.
pub fn render(spaces: &[Space]) -> String {
    render_styled(spaces, &Style::detect())
}

/// Colour-parametrised body of [`render`] (split out so tests can force a
/// deterministic no-colour rendering).
fn render_styled(spaces: &[Space], style: &Style) -> String {
    let mut lines: Vec<String> = vec![style.bold("  Git status per space"), String::new()];
    if spaces.is_empty() {
        lines.push(style.dim("  No spaces open."));
        return lines.join("\n");
    }

    let mut dirty = 0usize;
    for sp in spaces {
        if !sp.git.is_clean() {
            dirty += 1;
        }
        let marker = if sp.focused {
            style.green("●")
        } else {
            style.dim("○")
        };
        let notes = format!(
            "· {} pane{}",
            sp.pane_count,
            if sp.pane_count == 1 { "" } else { "s" }
        );

        lines.push(format!("  {} {}", marker, style.bold(&sp.label)));
        lines.push(format!("      {}", style.dim(&branch_display(&sp.git))));
        lines.push(format!(
            "      {}   {}",
            style.status(&sp.git, &status_cell(&sp.git)),
            style.dim(&notes),
        ));
        lines.push(String::new());
    }

    lines.push(style.dim(&format!(
        "  ── {} space{} · {} dirty",
        spaces.len(),
        if spaces.len() == 1 { "" } else { "s" },
        dirty,
    )));

    lines.join("\n")
}

// ---- JSON payload -----------------------------------------------------------

/// One entry of the `--json` payload.
#[derive(Serialize)]
struct JsonSpace {
    workspace_id: String,
    label: String,
    branch: String,
    focused: bool,
    panes: usize,
    is_repo: bool,
    ahead: u32,
    behind: u32,
    staged: u32,
    modified: u32,
    untracked: u32,
    conflicts: u32,
    clean: bool,
    /// Clean working tree AND nothing to push/pull — the `✓` state.
    fully_clean: bool,
}

/// Serialize spaces to the `--json` payload (array of per-space objects), 2-space
/// indented. No trailing newline.
pub fn render_json(spaces: &[Space]) -> String {
    let payload: Vec<JsonSpace> = spaces
        .iter()
        .map(|s| JsonSpace {
            workspace_id: s.id.clone(),
            label: s.label.clone(),
            branch: s.git.branch.clone(),
            focused: s.focused,
            panes: s.pane_count,
            is_repo: s.git.is_repo,
            ahead: s.git.ahead,
            behind: s.git.behind,
            staged: s.git.staged,
            modified: s.git.modified,
            untracked: s.git.untracked,
            conflicts: s.git.conflicts,
            clean: s.git.is_clean(),
            fully_clean: s.git.is_fully_clean(),
        })
        .collect();
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string())
}

// ---- run modes --------------------------------------------------------------

/// `--once`: print a single rendered snapshot and return.
pub fn run_once(client: &mut Herdr) -> crate::Result<()> {
    let spaces = collect::snapshot(client)?;
    println!("{}", render(&spaces));
    Ok(())
}

/// `--json`: print one JSON snapshot and return.
pub fn run_json(client: &mut Herdr) -> crate::Result<()> {
    let spaces = collect::snapshot(client)?;
    println!("{}", render_json(&spaces));
    Ok(())
}

/// `--interval`: live watch, redrawing every `interval_ms`.
///
/// A background thread restores the cursor and exits on SIGINT/SIGTERM; the main
/// loop hides the cursor, then clears + redraws each frame.
pub fn run_interval(client: &mut Herdr, interval_ms: u64) -> crate::Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    std::thread::spawn(move || {
        if signals.forever().next().is_some() {
            print!("\x1b[?25h"); // show cursor
            let _ = io::stdout().flush();
            std::process::exit(0);
        }
    });

    let mut out = io::stdout();
    write!(out, "\x1b[?25l")?; // hide cursor
    out.flush()?;

    loop {
        let body = match collect::snapshot(client) {
            Ok(spaces) => render(&spaces),
            Err(err) => format!("{} {err}", Style::detect().red("  herdr unavailable:")),
        };
        let footer = Style::detect().dim(&format!(
            "  refreshing every {}s · {} · ctrl-c to quit",
            interval_ms as f64 / 1000.0,
            local_time_string(),
        ));
        write!(out, "\x1b[2J\x1b[H{body}\n\n{footer}\n")?;
        out.flush()?;
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}

/// Local wall-clock `HH:MM:SS` for the live-watch footer stamp.
fn local_time_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as libc::time_t;
    // SAFETY: `localtime_r` fills the caller-owned `tm`; `secs` is a valid time_t.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Style {
        Style { color: false }
    }

    fn space(label: &str, focused: bool, git: GitStatus, panes: usize) -> Space {
        Space {
            id: label.to_string(),
            label: label.to_string(),
            focused,
            pane_count: panes,
            git,
            ..Default::default()
        }
    }

    fn repo(branch: &str) -> GitStatus {
        GitStatus {
            is_repo: true,
            branch: branch.to_string(),
            ..GitStatus::default()
        }
    }

    // ---- branch_display ------------------------------------------------------

    #[test]
    fn branch_display_arrows_only_when_nonzero() {
        assert_eq!(branch_display(&repo("main")), "main");
        let ahead = GitStatus {
            ahead: 2,
            ..repo("main")
        };
        assert_eq!(branch_display(&ahead), "main ↑2");
        let both = GitStatus {
            ahead: 1,
            behind: 3,
            ..repo("dev")
        };
        assert_eq!(branch_display(&both), "dev ↑1 ↓3");
    }

    #[test]
    fn branch_display_no_repo() {
        assert_eq!(branch_display(&GitStatus::default()), "(no branch)");
    }

    // ---- status_cell ---------------------------------------------------------

    #[test]
    fn status_cell_variants() {
        assert_eq!(status_cell(&GitStatus::default()), "—"); // non-repo
        assert_eq!(status_cell(&repo("main")), "✓"); // repo, fully clean
        let dirty = GitStatus {
            staged: 1,
            untracked: 2,
            ..repo("main")
        };
        assert_eq!(status_cell(&dirty), "+1 ?2");
        // clean working tree but ahead/behind != 0 shows "clean"
        let ahead = GitStatus {
            ahead: 1,
            ..repo("main")
        };
        assert_eq!(status_cell(&ahead), "clean");
    }

    // ---- severity colouring --------------------------------------------------

    #[test]
    fn status_colour_by_severity() {
        let c = Style { color: true };
        let clean = repo("main");
        let dirty = GitStatus {
            modified: 1,
            ..repo("main")
        };
        let conflict = GitStatus {
            conflicts: 1,
            ..repo("main")
        };
        assert_eq!(c.status(&clean, "x"), "\x1b[32mx\x1b[0m"); // green
        assert_eq!(c.status(&dirty, "x"), "\x1b[33mx\x1b[0m"); // yellow
        assert_eq!(c.status(&conflict, "x"), "\x1b[31mx\x1b[0m"); // red
    }

    // ---- render layout -------------------------------------------------------

    #[test]
    fn render_empty_spaces() {
        let out = render_styled(&[], &plain());
        assert_eq!(out, "  Git status per space\n\n  No spaces open.");
    }

    #[test]
    fn render_lays_out_marker_branch_and_status() {
        let git = GitStatus {
            staged: 2,
            modified: 1,
            untracked: 3,
            ahead: 1,
            ..repo("feature/x")
        };
        let out = render_styled(&[space("main", true, git, 2)], &plain());
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[0], "  Git status per space");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "  ● main"); // focused marker + bold label
        assert_eq!(lines[3], "      feature/x ↑1"); // branch + ahead
        assert!(lines[4].contains("+2 ~1 ?3"), "status: {}", lines[4]);
        assert!(lines[4].contains("· 2 panes"), "notes: {}", lines[4]);
        assert_eq!(lines[5], "");
        assert!(
            lines[6].starts_with("  ── 1 space · 1 dirty"),
            "{}",
            lines[6]
        );
    }

    #[test]
    fn render_unfocused_clean_singular_pane() {
        let out = render_styled(&[space("s", false, repo("main"), 1)], &plain());
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[2], "  ○ s"); // unfocused marker
        assert_eq!(lines[3], "      main");
        assert!(lines[4].contains("✓"));
        assert!(lines[4].contains("· 1 pane") && !lines[4].contains("panes"));
        assert!(lines[6].starts_with("  ── 1 space · 0 dirty"));
    }

    // ---- json ----------------------------------------------------------------

    #[test]
    fn json_field_shape() {
        let git = GitStatus {
            is_repo: true,
            branch: "main".to_string(),
            ahead: 1,
            modified: 2,
            ..GitStatus::default()
        };
        let out = render_json(&[space("w1", true, git, 2)]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["workspace_id"], "w1");
        assert_eq!(parsed[0]["branch"], "main");
        assert_eq!(parsed[0]["ahead"], 1);
        assert_eq!(parsed[0]["modified"], 2);
        assert_eq!(parsed[0]["clean"], false);
        assert_eq!(parsed[0]["fully_clean"], false);
        assert_eq!(parsed[0]["is_repo"], true);
    }

    #[test]
    fn json_empty_payload_is_bare_brackets() {
        assert_eq!(render_json(&[]), "[]");
    }

    #[test]
    fn json_clean_repo_reports_clean_true() {
        let out = render_json(&[space("w", false, repo("main"), 1)]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["clean"], true);
        assert_eq!(parsed[0]["fully_clean"], true);
    }

    #[test]
    fn json_clean_but_diverged_is_not_fully_clean() {
        let git = GitStatus {
            ahead: 2,
            ..repo("main")
        };
        let out = render_json(&[space("w", false, git, 1)]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed[0]["clean"], true);
        assert_eq!(parsed[0]["fully_clean"], false);
    }
}
