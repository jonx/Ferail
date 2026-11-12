# Git Status

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Ferail shows read-only Git awareness for folders inside a work tree: a Git
column in the file list and the current branch in the status bar. It never
stages, commits, checks out or writes anything.

## What the user sees

- **Git column.** Offered in the header's show/hide menu like any column,
  placed beside Name by default, and empty outside a work tree. A path's own
  state is one letter: `M` modified (not staged), `S` staged, `R` renamed,
  `D` deleted, `C` conflicted, `?` untracked, `I` ignored. A folder whose
  contents changed shows a dot in the colour of the loudest state inside it:
  conflicted, then modified, deleted, renamed, staged, untracked. Ignored
  content never marks its parent. Clean rows show nothing. Every mark has a
  tooltip naming the state. The letters are Git's own vocabulary and are not
  translated.
- **Branch label.** The status bar shows "On branch main", or "Detached at
  1a2b3c4", followed by `↑n` / `↓n` commits ahead of and behind the upstream.
  Its tooltip spells out the upstream. Private Mode presents the branch and
  upstream names like other personal labels. On a narrow window the label
  drops to the bare name, and it is the second segment to go, after the app
  statistics.

## How it works

| Piece | Location |
| --- | --- |
| Porcelain v2 parser, folder aggregation | `ferail_core::git_status` |
| Running `git` | `ferail_fs_native::git` |
| Cache, scheduling, apply to tabs | `crates/ferail-gpui/src/git_status.rs` |
| Row marks | `FileListDelegate::git_marks`, rendered by the `git` column |
| Branch label | `Tab::git_branch`, `status_bar::git_label` |

Ferail links no Git library. It runs the user's `git` binary on the
background executor, at most two commands:

- `git -C <folder> rev-parse --is-inside-work-tree --show-cdup`, once per
  folder that no cached repository already covers, to find the work-tree
  root. Porcelain status output does not name the root.
- `git -C <root> status --porcelain=v2 -z --branch --untracked-files=normal
  --ignored=matching --find-renames`, which yields the branch, ahead/behind
  and every changed, untracked or ignored path in one run. Untracked and
  ignored folders are reported once rather than file by file, so the output
  stays small beside `target/` or `node_modules/`.

Every call runs with `--no-pager -c core.fsmonitor=false`,
`GIT_OPTIONAL_LOCKS=0` (Git never writes `.git/index` on Ferail's behalf),
`GIT_TERMINAL_PROMPT=0`, `GIT_PAGER=cat`, `PAGER=cat`, a null stdin, and
without any inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
`GIT_COMMON_DIR` or `GIT_PREFIX`. A child still running after 10 seconds
(`GIT_TIMEOUT`) is killed and the column stays empty for that repository
until the next Refresh. On macOS `/usr/bin/git` is used only when developer
tools stand behind it, so the app never triggers the tools installer dialog.
Without a usable `git`, nothing is spawned for the rest of the session.

The cache in `ProcessState::git` holds one result per repository root and
answers every tab and window whose folder lies in that work tree, so moving
between subfolders costs no process while the result is younger than
`FRESH_FOR` (30 s). A folder inside a dirty submodule or inside an
untracked or ignored folder is asked about separately, since another
repository may start there. Folders found not to be in a work tree are
remembered until Refresh.

Requests come from semantic events only: a directory load completing, the
watcher's reload fan-out (`Shell::broadcast_reload_for_process`, which also
covers in-app file operations), Refresh, and the window returning to the
front (skipped when the result is under 2 s old). There is no polling. Each
repository runs one `git` child at a time after a 150 ms debounce; requests
arriving meanwhile fold into a single rerun, and a result overtaken by an
invalidation is dropped by generation unless nothing was shown yet.

When a result lands, `Shell::apply_git_to_tab` folds it into each tab's
`git_marks`, one hash probe per row by on-disk name, and stores the branch
label on the tab. Sorting carries the marks with their rows. A clean folder
or one outside a work tree keeps the vector empty. Paint reads the vector
and nothing else. Safe Mode, search and other tool results, and platform
namespaces show no Git state.
