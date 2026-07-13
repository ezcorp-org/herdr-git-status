//! Snapshot → spaces, worktree grouping, and git status.
//!
//! [`collect_spaces`] turns `workspace.list` + per-workspace `pane.list` into
//! [`Space`]s (recording each space's cwd and classifying panes into agent /
//! spare / pseudo buckets). [`group_worktree_families`] tags worktree-child
//! workspaces with their parent so [`snapshot`] can drop them — worktree children
//! render as single-row indented entries in the sidebar with no room for a status
//! row, so surfacing status on them is wasted. Unlike the cpu/ram plugin, nothing
//! is summed across the family: each work tree has its own status, and the child's
//! own top-level card (if any) is where it would show.

use std::collections::{HashMap, HashSet};

use crate::git;
use crate::herdr::Herdr;
use crate::model::Space;

/// Pseudo-agent label marking our agents-panel entries (agents-panel mode) and
/// used to recognise / clean them up in sidebar mode.
pub const PSEUDO_AGENT: &str = "git";

/// Enumerate spaces and classify each space's panes into agent / spare / pseudo
/// buckets, recording the first pane's cwd (where git will run).
///
/// One `workspace.list`, then one `pane.list` per workspace whose panes arrive
/// already ordered — so the "first pane's cwd" matches herdr's own branch
/// derivation. No `pane.process_info` is needed (git status comes from the cwd,
/// not any shell PID).
pub fn collect_spaces(client: &mut Herdr) -> crate::Result<Vec<Space>> {
    let workspaces = client.workspace_list()?;

    let mut spaces = Vec::with_capacity(workspaces.len());
    for ws in workspaces {
        let panes = client.pane_list(&ws.workspace_id)?;

        let mut agent_panes = Vec::new(); // panes with a real agent
        let mut spare_panes = Vec::new(); // plain shell panes — pseudo-agent hosts
        let mut pseudo_panes = Vec::new(); // panes already carrying our "git" agent
        let mut cwd: Option<String> = None;

        for pane in &panes {
            // First pane with a non-empty cwd wins.
            if cwd.is_none() {
                if let Some(c) = pane.cwd.as_deref().filter(|c| !c.is_empty()) {
                    cwd = Some(c.to_string());
                }
            }
            match pane.agent.as_deref() {
                Some(PSEUDO_AGENT) => pseudo_panes.push(pane.pane_id.clone()),
                Some(agent) if !agent.is_empty() => agent_panes.push(pane.pane_id.clone()),
                _ => spare_panes.push(pane.pane_id.clone()),
            }
        }

        let label = if ws.label.is_empty() {
            ws.workspace_id.clone()
        } else {
            ws.label.clone()
        };

        spaces.push(Space {
            id: ws.workspace_id,
            label,
            focused: ws.focused,
            pane_count: panes.len(),
            cwd,
            agent_panes,
            spare_panes,
            pseudo_panes,
            ..Default::default()
        });
    }
    Ok(spaces)
}

/// Tag worktree-child spaces with their group parent, one `worktree.list` per
/// unique repo. Children whose repo's main checkout is open get `family_parent`.
///
/// `worktree.list` errors for non-repo workspaces; that error is folded into
/// "leave it standalone". Parent/child resolution is done against an id→index
/// map and applied after the query loop so we never hold a `&mut` into `spaces`
/// while borrowing `client`.
pub fn group_worktree_families(client: &mut Herdr, spaces: &mut [Space]) {
    let index_of: HashMap<String, usize> = spaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let ids: Vec<String> = spaces.iter().map(|s| s.id.clone()).collect();

    let mut seen_repos: HashSet<String> = HashSet::new();
    let mut assignments: Vec<(usize, String)> = Vec::new(); // (child index, parent id)

    for ws_id in &ids {
        let res = match client.worktree_list(ws_id) {
            Ok(res) => res,
            Err(_) => continue, // workspace isn't in a git repo
        };
        let repo_key = res.source.repo_key;
        if repo_key.is_empty() || seen_repos.contains(&repo_key) {
            continue;
        }
        seen_repos.insert(repo_key);

        // The family only forms when the repo's main checkout is itself open.
        let parent_id = match res.source.source_workspace_id {
            Some(id) if index_of.contains_key(&id) => id,
            _ => continue, // main checkout isn't open — children stay standalone
        };
        for wt in res.worktrees {
            if let Some(child_id) = wt.open_workspace_id {
                if let Some(&child_idx) = index_of.get(&child_id) {
                    if child_id != parent_id {
                        assignments.push((child_idx, parent_id.clone()));
                    }
                }
            }
        }
    }

    for (child_idx, parent_id) in assignments {
        spaces[child_idx].family_parent = Some(parent_id);
    }
}

/// Full pipeline: collect → group worktrees → drop children → git status.
///
/// Worktree children are dropped (they render as single-row indented sidebar
/// entries — no status row), then `git status` is run once per surviving space.
pub fn snapshot(client: &mut Herdr) -> crate::Result<Vec<Space>> {
    let mut spaces = collect_spaces(client)?;
    group_worktree_families(client, &mut spaces);
    spaces.retain(|s| s.family_parent.is_none());
    for sp in &mut spaces {
        sp.git = git::status(sp.cwd.as_deref());
    }
    Ok(spaces)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child_of(id: &str, parent: Option<&str>) -> Space {
        Space {
            id: id.to_string(),
            label: id.to_string(),
            family_parent: parent.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_would_drop_tagged_worktree_children() {
        // Mirror the retain step snapshot() applies after grouping.
        let mut spaces = vec![
            child_of("parent", None),
            child_of("wt-child", Some("parent")),
            child_of("standalone", None),
        ];
        spaces.retain(|s| s.family_parent.is_none());
        let ids: Vec<&str> = spaces.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["parent", "standalone"]);
    }
}
