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
- Baseline commit: `5f1a8fd` (`fix(preview): coalesce rapid selection requests`)
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
| `5f1a8fd` | Bound selection-driven image and text preview scheduling to one active request plus one latest-wins waiting slot; expose active native preview work in task snapshots. | Hold Up/Down across `WCORPUS-MEDIA-10K`; verify bounded preview/STA concurrency, stable scrollbar, latest selection wins, and no stale-provider crash. |

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
   hold Up/Down continuously through long ranges and confirm preview work never
   grows beyond one active image request, one active text request, and their
   latest waiting selections;
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

## Collecting dumps

- **Automatic.** Any unhandled native exception writes
  `%APPDATA%\Ferail\reports\ferail-crash-<pid>.dmp` (the GUI) or
  `ferail-preview-broker-<pid>.dmp` (a preview helper) and appends the
  exception code/address to `ferail-crash-<pid>.txt` in the same folder.
  Rust panics keep writing that text report as before. Injected repro:
  `FERAIL_PREVIEW_BROKER_TEST=av` (or `crash`, `hang`) in the environment of
  a running Ferail makes the next third-party preview fail that way.
- **Hang or no dump written (Task Manager fallback).** Task Manager →
  Details → right-click `Ferail.exe` → *Create dump file* writes a full dump
  to `%TEMP%`. Use this for UI stalls, where nothing faults.
- **Symbolizing.** Open the `.dmp` in WinDbg with the PDBs from
  `Ferail-<version>-win-x64-symbols.zip` whose `manifest.json` commit and
  CodeView GUID match the binary (`.sympath+ <extracted folder>`; `!analyze
  -v`; `lm` to spot unloaded third-party modules). Never symbolize against
  PDBs from a different build.

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

### 2026-08-24 — first real-Windows session (diagnostic gate + WIN-015)

```text
Date / machine: 2026-08-24, Windows 11 Enterprise dev machine, C:\Source\Ferail
  (VS 2022 Community 14.44 tools, stable Rust 1.95 MSVC; no iscc/signtool,
  WinDbg not yet audited)
Start commit: 580d193, with uncommitted work in the tree (i18n CRLF test fix,
  shell-mac cfg gates, package-win.ps1 WIN-015 changes)
End commit(s): 093ae10 (i18n CRLF test), 94b1926 (shell-mac gating),
  9e7702d (WIN-015 static CRT + dependency gate + symbols)
Cases passed:
  - Diagnostic gate: cargo fmt --check; cargo test --workspace (653 tests,
    32 suites, 0 failures); cargo clippy --workspace --all-targets
    -D warnings; cargo build --release.
  - WTEST-001, WTEST-003: full package-win.ps1 run; static-CRT build; both
    binaries import only 34 Windows system DLLs (no vcruntime/msvcp/ucrt);
    portable + symbols ZIPs produced; packaged Ferail.exe renders a headless
    --screenshot and exits 0; packaged CLI launches.
Cases failed + evidence paths: none run beyond the above. The interactive
  acceptance slice (startup profiles, Ctrl+Shift+L, CPU/redraw comparison,
  open/reveal matrix, WCORPUS runs) has NOT been run — needs a human at the
  GUI. Logs: target/test-reports/windows/580d193/2026-08-24-session1/.
New dumps/PDB identity: no dumps. Packaged identities in
  target/package/Ferail-symbols/manifest.json — GUI ferail_gpui.pdb
  {41A17A9E-F1AB-48B1-8C64-D6F6F397A23A} age 1, CLI ferail.pdb
  {7702FEF6-A1DB-4224-96E0-CAAC216E6E28} age 1, commit 580d193. Note these
  ZIPs predate the three commits above; rebuild before distributing.
Measurements before/after: none (no acceptance timings this session).
Known regressions: none observed.
Next exact command or code location:
  1. Run § "2. Run the fast acceptance slice" interactively on this machine.
  2. WENV-C: pristine Windows Sandbox, network off, launch the portable ZIP
     (WTEST-004).
  3. Then WIN-001..004 crash containment (docs/features/
     WINDOWS_COMPATIBILITY_PLAN.md § A) with the diagnostic build.
  4. Consider CARGO_PROFILE_RELEASE_DEBUG=limited in package-win.ps1 before
     the dump work — current PDBs carry publics only, no line tables.
Working-tree files intentionally left unstaged: none.
```

Historical note: before this session, macOS preparation through `5f1a8fd`
passed all 281 `ferail-gpui` library tests and strict all-target Clippy; that
was preparation, not Windows acceptance.

### 2026-08-24 (continued) — user-driven fixes, WIN-002 broker, WIN-001 dumps

```text
Date / machine: 2026-08-24, same Windows 11 dev machine
Start commit: 1c0783d
End commit(s): e3ae2fc (status-bar strip beside its label), ae210bb +
  e74d352 (WCORPUS-OPEN generator), 4da59ce (WIN-002 preview broker),
  00aefe9 (WIN-001 leaked-handle cycles), e56fde3 (WIN-001 minidumps)
Cases passed:
  - WIN-002 containment via the CLI chain: Edge PdfPreviewHandler.dll renders
    through --preview-broker; injected crash → strike + icon fallback, app
    alive; injected hang → child killed at 6 s, zero orphans; malformed
    frame rejected (unit tests).
  - WIN-001 leaked-handle assertion: reproduced with a scripted right-click +
    Esc + close (exit 101, PopupMenu 52v3); after the cycle fixes the same
    script plus click-into-filter + typing exits 0. The user's interactive
    session had also leaked InputState 8v1 twice — same bug class in the
    filter/breadcrumb/shortcuts-help subscriptions, fixed; needs the user's
    own flow re-run to confirm.
  - WIN-001 minidumps: FERAIL_PREVIEW_BROKER_TEST=av → real 0xC0000005 →
    ferail-preview-broker-<pid>.dmp (MDMP, Exception/ThreadList/ModuleList/
    HandleData streams) + sidecar line in ferail-crash-<pid>.txt.
  - Status bar: progress strip now beside the task label (screenshot
    screenshots/status-bar-progress-left.png).
  - Corpus: WCORPUS-OPEN materialized (52 files, deepest path 405 chars,
    forced CON.txt/NUL.png/trailing-dot names); JPEG/PNG/WAV/PDF/ZIP fixtures
    validated to decode.
Cases failed + evidence paths: none failed. Not run: interactive acceptance
  slice; WTEST-004 (Windows Sandbox is not installed on this machine —
  enabling it needs elevation + reboot); the real pdfprevhndlr.dll repro.
New dumps/PDB identity: test dump ferail-preview-broker-1448.dmp under
  %APPDATA%\Ferail\reports (debug build, no packaged PDB pairing).
  Repackaged with the broker commit: target/package/Ferail-0.6.5-win-x64*.zip
  (predates 00aefe9 and the minidump commit — rebuild before distributing).
Measurements before/after: none.
Known regressions: none observed. Note: Edge's PDF preview handler captures
  as an all-white frame through the broker (its async paint outlasts the
  3.5 s probe budget) — same as the old in-process path; capture quality is
  WTEST-045 territory, containment is unaffected.
Incident: during the user's manual testing, Cargo.toml was moved into
  target/ by a Ferail drag/move; restored from the index (content identical).
Next exact command or code location:
  1. User: re-run the interactive session that leaked InputState 8v1 with
     LEAK_BACKTRACE=1 and quit; expect exit 0.
  2. Interactive acceptance slice (§2), now with test-data/open-reveal/ for
     the open/reveal matrix (WTEST-060…065).
  3. WIN-002 leftovers: kill-on-supersede for an in-flight broker;
     IInitializeWithStream fallback for handlers refusing the file init.
  4. WIN-004 selection audit + regression tests; WIN-003 needs the
     WCORPUS-MEDIA-10K repro first.
  5. Enable Windows Sandbox and run WTEST-004 on a freshly packaged ZIP.
Working-tree files intentionally left unstaged: none.
```
