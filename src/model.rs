//! Data types shared across the plugin.
//!
//! [`Space`] is the internal per-workspace aggregate; [`GitStatus`] is the
//! working-tree summary [`crate::git`] fills in for each space. The remaining
//! types are `serde` views over the `result` payloads of the herdr socket read
//! methods we call (`workspace.list`, `pane.list`, `worktree.list`). Each only
//! declares the fields we consume; serde ignores the rest of herdr's payload.

use serde::Deserialize;

/// Git working-tree summary for one space, computed from
/// `git status --porcelain=v1 --branch`.
///
/// All counts are file counts (a file staged *and* modified in the worktree
/// counts once in each of `staged` and `modified`, matching common git prompts).
/// `is_repo` is `false` when the cwd is missing or not a git repository, in which
/// case every count is zero and no status is surfaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// Whether the space's cwd is inside a git work tree.
    pub is_repo: bool,
    /// Current branch name (or `HEAD` when detached, empty when unknown).
    pub branch: String,
    /// Commits ahead of the upstream, when tracking one.
    pub ahead: u32,
    /// Commits behind the upstream, when tracking one.
    pub behind: u32,
    /// Files with staged (index) changes.
    pub staged: u32,
    /// Files with unstaged worktree changes.
    pub modified: u32,
    /// Untracked files.
    pub untracked: u32,
    /// Unmerged (conflicted) paths.
    pub conflicts: u32,
}

impl GitStatus {
    /// A non-repo or fully clean tree has nothing to surface.
    pub fn is_clean(&self) -> bool {
        !self.is_repo
            || (self.staged == 0
                && self.modified == 0
                && self.untracked == 0
                && self.conflicts == 0)
    }

    /// Fully clean: a repo whose working tree is clean AND that has nothing to
    /// push (ahead=0) AND nothing to pull (behind=0). Unlike [`is_clean`], a
    /// non-repo is NOT fully clean — there is no repo to vouch for.
    ///
    /// [`is_clean`]: GitStatus::is_clean
    pub fn is_fully_clean(&self) -> bool {
        self.is_repo && self.is_clean() && self.ahead == 0 && self.behind == 0
    }
}

/// Per-space aggregate for one herdr space (workspace).
#[derive(Debug, Clone, Default)]
pub struct Space {
    /// herdr workspace id.
    pub id: String,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    /// cwd of the first pane with a non-empty cwd (where git runs).
    pub cwd: Option<String>,
    /// panes with a real agent.
    pub agent_panes: Vec<String>,
    /// plain shell panes.
    pub spare_panes: Vec<String>,
    /// panes carrying our "git" pseudo-agent.
    pub pseudo_panes: Vec<String>,
    /// git working-tree summary, filled by [`crate::git::status`].
    pub git: GitStatus,
    /// workspace id of the worktree-group parent (set → this is a child).
    pub family_parent: Option<String>,
}

// ---- workspace.list ---------------------------------------------------------
//
// result = { "type": "workspace_list", "workspaces": [ { workspace_id, label,
//            focused, .. } ] }

/// `result` payload of `workspace.list`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceListResult {
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
}

/// One entry of `workspaces`; only the fields we consume are modelled.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
}

// ---- pane.list --------------------------------------------------------------
//
// result = { "type": "pane_list", "panes": [ { pane_id, cwd?, agent?, .. } ] }

/// `result` payload of `pane.list`.
#[derive(Debug, Clone, Deserialize)]
pub struct PaneListResult {
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
}

/// One entry of `panes`; only the fields we consume are modelled.
#[derive(Debug, Clone, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

// ---- worktree.list ----------------------------------------------------------
//
// result = { "type": "worktree_list", "source": { .. }, "worktrees": [ .. ] }
// (this method ERRORS when the workspace is not a git repo)

/// `result` payload of `worktree.list`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeListResult {
    pub source: WorktreeSource,
    #[serde(default)]
    pub worktrees: Vec<WorktreeEntry>,
}

/// The `source` object identifying the repo and its main checkout's workspace.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeSource {
    pub repo_key: String,
    #[serde(default)]
    pub source_workspace_id: Option<String>,
}

/// One entry of `worktrees`; only the open workspace id matters for grouping.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeEntry {
    #[serde(default)]
    pub open_workspace_id: Option<String>,
}
