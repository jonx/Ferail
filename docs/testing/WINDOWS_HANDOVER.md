# Windows Reliability Handover

← [Implementation ledger](../features/WINDOWS_COMPATIBILITY_PLAN.md) ·
[Acceptance plan](WINDOWS_RELIABILITY_TEST_PLAN.md) ·
[Windows port notes](../features/windows-port.md) · [Open work](../../TODO.md)

## Purpose

This is the short operational handover for resuming the Windows reliability
campaign on a real Windows development machine. The implementation ledger
remains the source of truth for design and issue status; the acceptance plan
contains the complete test matrix. This file answers the practical questions:
which revision to start from, what has already changed, what must be done on
Windows, what evidence to retain, and in what order to proceed.

Update this handover at the end of every Windows work session. Never mark a
Windows-only exit gate complete from macOS or cross-compilation alone.

## Repository state at handover creation

- Branch: `main`
- Baseline commit: `b67754a` (`docs(windows): record implementation progress`)
- Release under investigation: `0.6.5`
- Tester bundle:
  `/Users/jkn/Downloads/reunabletofindprojectdjohnknipperpersonalshellba.zip`
- Reference Ferail-Win32 checkout: `/Users/jkn/Source/Ferail-win32`
  at `4fcb2ffb2c622c49b4c333b115588626e4f74245`
- Reference Filociraptor checkout: `/Users/jkn/Source/Filociraptor`
  at `c3adf308ead1b9e2badd54e93d754791af8fc18d`

Always record the actual starting revision on Windows:

```powershell
git status --short
git rev-parse HEAD
git log -8 --oneline
```

At handover creation, two unrelated user changes intentionally remain outside
the Windows commits: `CHANGELOG.md` and
`crates/ferail-gpui/src/file_list.rs` (Flat row-buffer release work). Preserve
any equivalent parallel changes that are still present after transferring the
worktree; do not stage them into a Windows feature commit.

## Completed implementation awaiting Windows acceptance

| Commit | Change | Windows status |
| --- | --- | --- |
| `774332c` | Normalize the displayed Windows CPU percentage like Task Manager; rename `rps` to localized `redraws/s`. | Run WTEST-CPU and idle-redraw cases. |
| `a213e4d` | Replace `cmd /C start` with `ShellExecuteExW`; replace Explorer `/select,` strings with PIDL-based `SHOpenFolderAndSelectItems`. | Run the complete open/reveal path matrix. |
| `c4c1baa` | Make ordinary Format/Description/quarantine enrichment viewport-only and O(viewport) on apply. | Run 10k-media, 100k-wide and 4M Flat regressions. |
| `eaa39a4` | Connect `view.toggle_flat` to `ToggleFlatView` in the GPUI keymap. | Confirm a clean and upgraded profile emits no unknown-command warning and Ctrl+Shift+L works. |

These changes passed native macOS tests and strict Clippy. macOS-to-MSVC
cross-checking cannot validate the full application because C dependencies
need the Windows SDK and MSVC tools (`stdlib.h`, `windows.h`, `ml64.exe`). This
is an environment boundary, not a Windows acceptance result.

## First Windows session

### 1. Prepare a diagnostic build

Install the current stable Rust MSVC toolchain, Visual Studio Build Tools with
the matching Windows SDK, WinDbg, and Git. Then run:

```powershell
rustup show
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

Record failures under
`target/test-reports/windows/<commit>/<run-id>/`; do not repair unrelated
warnings by weakening lints or disabling Windows code.

### 2. Run the fast acceptance slice before new Windows code

Use the exact case definitions in `WINDOWS_RELIABILITY_TEST_PLAN.md` and run,
in order:

1. startup on a clean profile and an upgraded 0.6.5 profile;
2. Ctrl+Shift+L and startup-log inspection for `view.toggle_flat`;
3. single-core and idle CPU/redraw comparison with Task Manager;
4. double-click/default-open matrix for JPEG, PDF, TXT, video, directories,
   Unicode, spaces, `#`, `%`, `!`, UNC and long paths;
5. Reveal in Explorer over the same difficult-path matrix;
6. `WCORPUS-MEDIA-10K` with Format and Description visible while scrolling;
7. `WCORPUS-WIDE-100K`, then `WCORPUS-FLAT-4M`, checking that detail work is
   proportional to the viewport and Flat memory/scroll behavior is unchanged.

Do not proceed by fixing the first symptom in place. Retain the log, report,
Task Manager measurements, screenshot/video, and dump/PDB identity for every
failed case.

### 3. Commit acceptance fixes independently

Keep one concern per commit. Suggested prefixes:

```text
fix(windows-preview): ...
fix(windows-menu): ...
fix(windows-shell): ...
fix(windows-clipboard): ...
build(windows): ...
```

After each commit, update both the implementation ledger checkbox and the
matching acceptance record. `[x]` means verified on Windows; use `[~]` for code
implemented elsewhere but still awaiting the real-machine matrix.

## Windows-only implementation queue

### A. Crash containment first — WIN-001 through WIN-004

1. Reproduce the PDF preview access violation and multi-selection crashes with
   the diagnostic build and matching PDBs.
2. Add PDB identity, provider identity and current preview/selection request to
   the report before changing behavior.
3. Move third-party preview-handler execution out of `ferail.exe`. A crash or
   hang in `pdfprevhndlr.dll` must terminate only the helper and return a
   fallback/failed preview state to the main process.
4. Bound the preview request scheduler to viewport + directional overscan,
   generation-cancel stale work, and preserve scrollbar/model identity.

Do not paper over access violations with Rust panic handling: an in-process
COM provider can corrupt native state before Rust observes anything.

### B. Packaging — WIN-015

1. Inspect the exact Release dependency set (`dumpbin /dependents` or
   equivalent).
2. Decide and document app-local runtime versus installer prerequisite.
3. Test the portable ZIP with networking disabled in WENV-C, where no VC++
   runtime or developer tools are installed.
4. Publish PDBs/symbol identity in a form that makes tester dumps actionable.

### C. Native compatibility on explicit demand — WIN-007

Keep ordinary right-click entirely inside Ferail. Implement the native Shell
menu only through **More options from Windows…**, Shift+right-click, and
Shift+F10. Start from the predecessor's `shell_pump.rs` only as a behavioral
reference; do not port `menu_preload.rs` or any selection-change prefetch.

The production boundary is an isolated broker process which receives a small
explicit target snapshot, binds same-parent child PIDLs, owns the
`IContextMenu2/3` message loop and `TrackPopupMenuEx`, invokes the chosen verb,
then exits. Ferail must remain alive if a third-party handler crashes or hangs.

### D. Shell namespace and Windows metadata — WIN-010 onward

Add platform capability interfaces behind the shared application, not a
Windows fork. Ordinary filesystem folders continue through `NativeFs` with no
per-row COM/PIDL state. Shell namespace locations, Recycle Bin, MTP and virtual
OneDrive roots use Windows-owned location tokens only when no useful direct
path exists.

## Performance invariants to check after every Windows feature

- Normal navigation and right-click perform no Shell extension enumeration.
- No normal row gains a COM object, PIDL, owned duplicate path, thumbnail, or
  provider state.
- Select All remains complement-based and does not materialize millions of
  selected ids or paths.
- Background completion never scans the entire table to apply a viewport
  result.
- `WCORPUS-FLAT-4M` remains scrollable and memory stays proportional to the
  compact row/path representation.
- Optional Windows integration may fail closed with an explanation; it may
  not freeze or terminate the main process.

## End-of-session handback template

Append a dated block below before leaving Windows:

```text
Date / machine:
Start commit:
End commit(s):
Cases passed:
Cases failed + evidence paths:
New dumps/PDB identity:
Measurements before/after:
Known regressions:
Next exact command or code location:
Working-tree files intentionally left unstaged:
```

## Session log

No real-Windows acceptance session has been recorded yet for the post-0.6.5
commits listed above.
