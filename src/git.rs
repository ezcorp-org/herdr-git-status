//! Git working-tree status via `git status --porcelain=v1 --branch`.
//!
//! [`status`] shells out to `git` in a space's cwd once per refresh and parses
//! the stable porcelain-v1 format into a [`GitStatus`]. [`token`] reduces that to
//! the compact sidebar string — `+staged ~modified ?untracked !conflicts`, with
//! zero-count parts omitted and a clean tree yielding the empty string.
//! [`status_token`] is what the updater pushes: [`token`], plus [`CLEAN_MARK`]
//! for a fully clean repo (nothing to commit, push, or pull). Branch and
//! ahead/behind are parsed for the dashboard/JSON but deliberately kept OUT of
//! [`token`]: the patched herdr already renders the branch (and its ahead/
//! behind arrows) on its own row, so the status row shows only the work-tree.

use std::process::{Command, Stdio};

use crate::model::GitStatus;

/// Run `git status` in `cwd` and summarise it. A missing/empty cwd, a non-repo
/// dir, or any git failure yields the default (`is_repo = false`) — the caller
/// then surfaces nothing for that space.
pub fn status(cwd: Option<&str>) -> GitStatus {
    let cwd = match cwd {
        Some(c) if !c.is_empty() => c,
        _ => return GitStatus::default(),
    };
    // `--no-optional-locks` keeps this read-only probe from taking the index
    // lock (so it never contends with the user's own git commands).
    let output = Command::new("git")
        .args([
            "-C",
            cwd,
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "--branch",
            "--untracked-files=normal",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => parse_porcelain(&String::from_utf8_lossy(&out.stdout)),
        _ => GitStatus::default(),
    }
}

/// The mark for a fully clean repo — nothing to commit, push, or pull.
pub const CLEAN_MARK: &str = "✓";

/// The status string the updater surfaces for a space: [`token`] for a dirty
/// tree, [`CLEAN_MARK`] for a fully clean repo, and `""` otherwise (non-repo, or
/// clean-but-diverged — the ahead/behind arrows are herdr's branch row's job).
pub fn status_token(g: &GitStatus) -> String {
    if g.is_fully_clean() {
        return CLEAN_MARK.to_string();
    }
    token(g)
}

/// The compact sidebar token: `"+<s> ~<m> ?<u> !<c>"` with zero parts omitted.
/// Returns `""` for a clean (or non-repo) tree so the sidebar shows no row.
pub fn token(g: &GitStatus) -> String {
    if g.is_clean() {
        return String::new();
    }
    let mut parts = Vec::with_capacity(4);
    if g.staged > 0 {
        parts.push(format!("+{}", g.staged));
    }
    if g.modified > 0 {
        parts.push(format!("~{}", g.modified));
    }
    if g.untracked > 0 {
        parts.push(format!("?{}", g.untracked));
    }
    if g.conflicts > 0 {
        parts.push(format!("!{}", g.conflicts));
    }
    parts.join(" ")
}

/// Parse `git status --porcelain=v1 --branch` output into a [`GitStatus`].
///
/// Line 1 is the `## <branch-info>` header; every other line is an `XY <path>`
/// entry where `X` is the index (staged) state and `Y` the worktree state.
/// Classification, in order: `??` untracked, unmerged (`U` in either column, or
/// `AA`/`DD`), then any non-space `X` counts as staged and any non-space `Y` as
/// modified (a file can be both).
fn parse_porcelain(text: &str) -> GitStatus {
    let mut g = GitStatus {
        is_repo: true,
        ..GitStatus::default()
    };
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            parse_branch_header(header, &mut g);
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let (x, y) = (bytes[0], bytes[1]);
        if x == b'?' && y == b'?' {
            g.untracked += 1;
            continue;
        }
        if x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D') {
            g.conflicts += 1;
            continue;
        }
        if x != b' ' {
            g.staged += 1;
        }
        if y != b' ' {
            g.modified += 1;
        }
    }
    g
}

/// Parse the `## ` header body into branch + ahead/behind.
///
/// Forms: `main...origin/main [ahead 1, behind 2]`, `main...origin/main [gone]`,
/// bare `main` (no upstream), `HEAD (no branch)` (detached), and
/// `No commits yet on main` (fresh repo).
fn parse_branch_header(header: &str, g: &mut GitStatus) {
    if header.starts_with("HEAD (no branch)") {
        g.branch = "HEAD".to_string();
        return;
    }
    if let Some(rest) = header.strip_prefix("No commits yet on ") {
        g.branch = rest.trim().to_string();
        return;
    }
    // Split off the optional " [ahead .., behind ..]" tracking suffix.
    let (names, tracking) = match header.split_once(" [") {
        Some((names, t)) => (names, Some(t.trim_end_matches(']'))),
        None => (header, None),
    };
    // Local branch is the part before "...<upstream>".
    g.branch = names
        .split("...")
        .next()
        .unwrap_or(names)
        .trim()
        .to_string();
    if let Some(tracking) = tracking {
        for part in tracking.split(',') {
            let part = part.trim();
            if let Some(n) = part.strip_prefix("ahead ") {
                g.ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                g.behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- branch header parsing ----------------------------------------------

    #[test]
    fn header_bare_branch_no_upstream() {
        let mut g = GitStatus::default();
        parse_branch_header("main", &mut g);
        assert_eq!(g.branch, "main");
        assert_eq!((g.ahead, g.behind), (0, 0));
    }

    #[test]
    fn header_upstream_without_divergence() {
        let mut g = GitStatus::default();
        parse_branch_header("main...origin/main", &mut g);
        assert_eq!(g.branch, "main");
        assert_eq!((g.ahead, g.behind), (0, 0));
    }

    #[test]
    fn header_ahead_and_behind() {
        let mut g = GitStatus::default();
        parse_branch_header("feat/x...origin/feat/x [ahead 3, behind 2]", &mut g);
        assert_eq!(g.branch, "feat/x");
        assert_eq!((g.ahead, g.behind), (3, 2));
    }

    #[test]
    fn header_ahead_only_and_gone_upstream() {
        let mut ahead = GitStatus::default();
        parse_branch_header("main...origin/main [ahead 5]", &mut ahead);
        assert_eq!((ahead.ahead, ahead.behind), (5, 0));

        let mut gone = GitStatus::default();
        parse_branch_header("wip...origin/wip [gone]", &mut gone);
        assert_eq!(gone.branch, "wip");
        assert_eq!((gone.ahead, gone.behind), (0, 0));
    }

    #[test]
    fn header_detached_and_fresh_repo() {
        let mut detached = GitStatus::default();
        parse_branch_header("HEAD (no branch)", &mut detached);
        assert_eq!(detached.branch, "HEAD");

        let mut fresh = GitStatus::default();
        parse_branch_header("No commits yet on main", &mut fresh);
        assert_eq!(fresh.branch, "main");
    }

    // ---- porcelain body: counts by type -------------------------------------

    #[test]
    fn porcelain_counts_each_type() {
        // Built as a slice + join so leading-space lines (" M", " D") keep their
        // column-X space — a `\`-newline string literal would strip it.
        // "M " = staged modify, " M" = worktree modify, "MM" = both, "??"
        // untracked, "A " = staged add, " D" = worktree delete.
        let text = [
            "## main...origin/main [ahead 1]",
            "M  staged.rs",
            " M worktree.rs",
            "MM both.rs",
            "A  added.rs",
            " D gone.rs",
            "?? new.rs",
            "?? new2.rs",
        ]
        .join("\n");
        let g = parse_porcelain(&text);
        assert!(g.is_repo);
        assert_eq!(g.branch, "main");
        assert_eq!(g.ahead, 1);
        // staged: staged.rs, both.rs, added.rs = 3
        assert_eq!(g.staged, 3);
        // modified (worktree): worktree.rs, both.rs, gone.rs = 3
        assert_eq!(g.modified, 3);
        assert_eq!(g.untracked, 2);
        assert_eq!(g.conflicts, 0);
        assert!(!g.is_clean());
    }

    #[test]
    fn porcelain_counts_conflicts_separately() {
        // Unmerged forms: UU (both modified), AA (both added), DD (both deleted),
        // and U in either column. Slice + join keeps columns intact.
        let text = [
            "## main",
            "UU conflict.rs",
            "AA added-both.rs",
            "DD deleted-both.rs",
            "DU us-deleted.rs",
            "M  clean-stage.rs",
        ]
        .join("\n");
        let g = parse_porcelain(&text);
        assert_eq!(g.conflicts, 4);
        // The conflicted lines are NOT double-counted as staged/modified.
        assert_eq!(g.staged, 1); // only clean-stage.rs
        assert_eq!(g.modified, 0);
    }

    #[test]
    fn porcelain_clean_tree_is_clean() {
        let g = parse_porcelain("## main...origin/main\n");
        assert!(g.is_repo);
        assert!(g.is_clean());
        assert_eq!(token(&g), "");
    }

    #[test]
    fn porcelain_renamed_counts_as_staged() {
        // Rename: X='R', Y=' '. Counts once as staged.
        let g = parse_porcelain("## main\nR  old.rs -> new.rs\n");
        assert_eq!(g.staged, 1);
        assert_eq!(g.modified, 0);
    }

    // ---- token formatting ----------------------------------------------------

    #[test]
    fn token_omits_zero_parts_in_fixed_order() {
        let g = GitStatus {
            is_repo: true,
            staged: 2,
            modified: 1,
            untracked: 3,
            ..GitStatus::default()
        };
        assert_eq!(token(&g), "+2 ~1 ?3");
    }

    #[test]
    fn token_single_part() {
        let g = GitStatus {
            is_repo: true,
            modified: 4,
            ..GitStatus::default()
        };
        assert_eq!(token(&g), "~4");
    }

    #[test]
    fn token_includes_conflicts_last() {
        let g = GitStatus {
            is_repo: true,
            staged: 1,
            conflicts: 2,
            ..GitStatus::default()
        };
        assert_eq!(token(&g), "+1 !2");
    }

    #[test]
    fn token_empty_for_non_repo_and_clean() {
        assert_eq!(token(&GitStatus::default()), ""); // non-repo
        assert_eq!(
            token(&GitStatus {
                is_repo: true,
                ..GitStatus::default()
            }),
            "",
        );
    }

    // ---- status_token (what the updater pushes) -------------------------------

    #[test]
    fn status_token_fully_clean_repo_is_check_mark() {
        let g = GitStatus {
            is_repo: true,
            ..GitStatus::default()
        };
        assert!(g.is_fully_clean());
        assert_eq!(status_token(&g), CLEAN_MARK);
    }

    #[test]
    fn status_token_non_repo_is_empty_not_check_mark() {
        // A non-repo has zero counts and zero ahead/behind, but there is no repo
        // to vouch for — it must NOT read as "fully clean".
        let g = GitStatus::default();
        assert!(!g.is_fully_clean());
        assert_eq!(status_token(&g), "");
    }

    #[test]
    fn status_token_clean_but_diverged_is_empty() {
        // Clean tree with commits to push/pull: the branch row's ↑/↓ arrows carry
        // that story, so the status stays empty rather than claiming ✓.
        for (ahead, behind) in [(1, 0), (0, 2), (3, 4)] {
            let g = GitStatus {
                is_repo: true,
                ahead,
                behind,
                ..GitStatus::default()
            };
            assert_eq!(status_token(&g), "", "ahead={ahead} behind={behind}");
        }
    }

    #[test]
    fn status_token_dirty_tree_passes_through_token() {
        let g = GitStatus {
            is_repo: true,
            staged: 2,
            untracked: 1,
            ..GitStatus::default()
        };
        assert_eq!(status_token(&g), token(&g));
        assert_eq!(status_token(&g), "+2 ?1");
    }

    #[test]
    fn status_non_repo_cwd_is_not_a_repo() {
        // /tmp is (almost certainly) not a git repo — status stays default.
        let g = status(Some("/"));
        assert!(!g.is_repo);
        assert!(g.is_clean());
    }

    #[test]
    fn status_empty_or_missing_cwd() {
        assert!(!status(None).is_repo);
        assert!(!status(Some("")).is_repo);
    }
}
