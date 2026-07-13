# herdr-git-status

A [herdr](https://herdr.dev) plugin that shows each space's **git working-tree
status** in the sidebar — so when you're running a herd of agents you can see
at a glance which repos have uncommitted work, and a `✓` when everything is
committed and pushed.

```
⚡ web-app
    git · +2 ~1 ?3    ← agents panel (default mode, stock herdr)

⚡ api
    git · ✓           ← fully clean: nothing to commit, push, or pull

● web-app
    main ↑1
    +2 ~1 ?3          ← spaces card (sidebar mode, patched herdr)
```

The status token is compact and only shows what's non-zero:

| token | meaning                         |
|-------|---------------------------------|
| `+N`  | N files with **staged** changes |
| `~N`  | N files **modified** in the work tree |
| `?N`  | N **untracked** files           |
| `!N`  | N **conflicted** (unmerged) paths |
| `✓`   | **fully clean** — nothing to commit, push, or pull |

A fully clean repo (clean tree, nothing ahead/behind) shows `✓` as positive
confirmation the space is settled; set `clean_checkmark = false` (below) for the
old show-nothing behaviour. A clean tree that still has commits to push or pull
shows no row — branch and ahead/behind (`↑`/`↓`) are rendered by herdr itself on
the branch row, so the status row stays focused on the work tree.

- Per-space `git status --porcelain` summary, refreshed every 5s
- **Worktree-aware**: worktree-child workspaces (rendered as single-row indented
  sidebar entries) are skipped rather than mislabelled
- A live dashboard pane and one-shot report/JSON actions
- A small static Rust binary that talks to herdr over its unix socket — one
  `git` invocation per space per refresh, no Node runtime
- Read-only: uses `git --no-optional-locks status`, so it never contends with
  your own git commands for the index lock

## Install

```sh
herdr plugin install ezcorp-org/herdr-git-status
```

Requirements: `git` and the **Rust toolchain** (`cargo`) on the box hosting the
herdr server — herdr compiles the plugin at install time via
`cargo build --release`. Plugins run on the machine hosting the herdr server, so
remote setups need these on the server box only.

That's it — the default mode works on stock herdr, no patches or extra setup.

## Usage

Toggle the background updater (statuses appear in the agents panel within ~5s):

```sh
herdr plugin action invoke status-toggle --plugin ez-corp.git-status
```

Other entrypoints:

```sh
herdr plugin pane open --plugin ez-corp.git-status --entrypoint dashboard  # live dashboard
herdr plugin action invoke report --plugin ez-corp.git-status              # one-shot snapshot
./target/release/git-status --json                                         # machine-readable
```

Statuses carry a TTL and self-clear if the updater dies; disabling clears
everything immediately. A space that becomes fully clean flips to `✓` on the
next refresh; one whose status empties (e.g. clean but with commits left to
push) has its row actively cleared.

## Modes

Configure in `$HERDR_PLUGIN_CONFIG_DIR/config.toml`
(herdr prints the config dir via `herdr plugin config-dir ez-corp.git-status`):

```toml
mode = "agents-panel"       # default — works on stock herdr
# mode = "sidebar"          # for herdr builds with the sidebar patch (below)

# Refresh cadence in seconds (>= 1). Default 5.
interval_seconds = 5

# Show "✓" for fully clean repos (nothing to commit, push, or pull). Default
# true; false shows nothing for clean repos, so only dirty spaces claim a row.
clean_checkmark = true
```

- **agents-panel** (default): each space gets a `git` pseudo-agent entry in the
  sidebar agents panel, carrying the status token. Works on stock herdr.
- **sidebar**: renders the status inside each spaces card, under the branch
  name. Requires a herdr build patched to add a spaces-card status row — a
  native plugin sidebar surface is an open upstream request (discussion #713).
  That patch modifies herdr itself (AGPL-3.0), so it is **not** distributed
  with this MIT plugin; agents-panel mode works on stock herdr with no patch.

Switching modes cleans up after the other mode automatically.

Note the status is a plain string styled by herdr — it renders in herdr's own
row colours, not per-token colours (those are only possible where herdr itself
draws the token).

Only one sidebar-mode daemon can own a space's status row at a time. If you also
run the [space-usage](https://github.com/ezcorp-org/herdr-pc-ram-and-cpu-usage-overlay)
plugin in sidebar mode, the two will contend for that single row — run one in
sidebar mode and the other in agents-panel mode (or disable one).

## How it works

For each open workspace, the plugin reads the panes over the herdr socket, takes
the first pane's cwd, and runs `git status --porcelain=v1 --branch` there. The
result is reduced to the token above and pushed onto a pane as a `custom_status`
(TTL'd metadata in sidebar mode, or a pseudo-agent in agents-panel mode). It
computes nothing from `/proc` and spawns no shells — just one `git` per space.

## Development

```sh
git clone <this repo>
cd herdr-git-status
cargo build --release
herdr plugin link .
```

`herdr plugin link` references the directory in place and does **not** run the
build step, so run `cargo build --release` first — the linked commands invoke
`./target/release/git-status`. (`herdr plugin install` builds automatically.)

## License

MIT — see [LICENSE](LICENSE).
