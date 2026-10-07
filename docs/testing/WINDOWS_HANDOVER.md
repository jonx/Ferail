# Windows Reliability Handover

← [Implementation ledger](../features/WINDOWS_COMPATIBILITY_PLAN.md) ·
[Acceptance plan](WINDOWS_RELIABILITY_TEST_PLAN.md) ·
[Windows port notes](../features/windows-port.md) · [Open work](../../TODO.md)

<!-- toc depth=2 -->

- [Purpose](#purpose)
- [Current resume point](#current-resume-point)
- [First Windows session](#first-windows-session)
- [Windows-only implementation queue](#windows-only-implementation-queue)
- [Performance invariants to check after every Windows feature](#performance-invariants-to-check-after-every-windows-feature)
- [Collecting dumps](#collecting-dumps)
- [End-of-session handback template](#end-of-session-handback-template)

<!-- /toc -->

## Purpose

This is the short operational handover for resuming the Windows reliability
campaign on a real Windows development machine. The implementation ledger
remains the source of truth for design and issue status; the acceptance plan
contains the complete test matrix. This file answers the practical questions:
which revision to start from, what has already changed, what must be done on
Windows, what evidence to retain, and in what order to proceed.

Update this handover at the end of every Windows work session. Never mark a
Windows-only exit gate complete from macOS or cross-compilation alone.

## Current resume point

Release, artifacts and what is still open are in
[docs/STATUS.md](../STATUS.md#windows-campaign). This file adds only what a
Windows machine needs:

- Branch: `main`. Record the actual starting revision with
  `git rev-parse HEAD` after pulling; never trust an abbreviated hash written
  into a document.
- What each past session did, and what it left behind, is in the
  [session log](../memos/windows-sessions-2026-08.md).
- The acceptance matrix that gates every exit is the
  [reliability test plan](WINDOWS_RELIABILITY_TEST_PLAN.md).

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

Use the exact case definitions in [WINDOWS_RELIABILITY_TEST_PLAN.md](WINDOWS_RELIABILITY_TEST_PLAN.md) and run,
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

### A. Crash containment first - WIN-001 through WIN-004

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

### B. Packaging - WIN-015

1. Inspect the exact Release dependency set (`dumpbin /dependents` or
   equivalent).
2. Decide and document app-local runtime versus installer prerequisite.
3. Test the portable ZIP with networking disabled in WENV-C, where no VC++
   runtime or developer tools are installed.
4. Publish PDBs/symbol identity in a form that makes tester dumps actionable.

### C. Native compatibility on explicit demand - WIN-007

Keep ordinary right-click entirely inside Ferail. Implement the native Shell
menu only through **More options from Windows…**, Shift+right-click, and
Shift+F10. Start from the predecessor's `shell_pump.rs` only as a behavioral
reference; do not port `menu_preload.rs` or any selection-change prefetch.

The production boundary is an isolated broker process which receives a small
explicit target snapshot, binds same-parent child PIDLs, owns the
`IContextMenu2/3` message loop and `TrackPopupMenuEx`, invokes the chosen verb,
then exits. Ferail must remain alive if a third-party handler crashes or hangs.

Implementation landed in the working tree on 2026-08-25. The normal row menu
now ends in **More options from Windows…**; Shift+right-click and Shift+F10
dispatch the same action directly. The executable's early
`--windows-context-menu-broker` role owns all Shell extension code and uses a
readiness pipe: preparation is killed after eight seconds, while a visible menu
is deliberately not timed out. Same-parent filesystem multi-selection is
supported; mixed-parent selections receive an explicit notification. Command
invocation is Unicode and requests synchronous completion. The canonical
Properties verb bypasses its prematurely-returning context-menu handler and
uses `SHObjectProperties` / `SHMultiFileProperties` directly. The owner proxy
is positioned at the click point, and the GPUI live-menu listener explicitly
ignores Shift+right-click so only the native extended menu opens.

Manual gate still required: exercise list and grid views with a selected and
unselected row; compare plain right-click, More, Shift+right-click and
Shift+F10; open dynamic/owner-drawn submenus from 7-Zip, Defender and a Git
client; invoke one mutating verb and confirm Ferail remains responsive. Mixed
parents and namespace-only items must fail clearly rather than flattening or
silently changing the target set.

### D. Shell namespace and Windows metadata - WIN-010 onward

Add platform capability interfaces behind the shared application, not a
Windows fork. Ordinary filesystem folders continue through `NativeFs` with no
per-row COM/PIDL state. Shell namespace locations, Recycle Bin, MTP and virtual
OneDrive roots use Windows-owned location tokens only when no useful direct
path exists.


### E. Fast whole-volume search over the MFT

Search, Flat and search-as-you-type use the directory walker on Windows; only
Disk Usage reads the MFT. This step gives search the same speed. Design
context: [Search § Rely on the OS index](../features/SEARCH.md#rely-on-the-os-index-where-it-exists)
and [Fast NTFS § Architecture boundary](../features/WINDOWS_FAST_NTFS.md#architecture-boundary).
Open item in [TODO.md § Search](../../TODO.md#search).

Build it in this order, verifying each step on Windows before the next:

1. **Scope choice in the filter field**, platform-neutral: *This folder* /
   *Subfolders* / *Whole volume (C:)*, with Shift+Enter for the whole volume.
   It runs on the existing walker (or Spotlight on macOS) before any MFT work,
   so it can land and be tested on every platform.
2. **A search request to the elevated helper.** A separate read-only adapter
   that queries the in-memory MFT index by name and token predicates, returning
   only matching rows. It must not share the Disk Usage request's helper
   lifetime or index implicitly. First use explains the UAC prompt ("Ferail
   uses the same helper as Disk Usage to index C: in a few seconds") before
   asking. Declined elevation, a non-NTFS volume or a helper failure falls back
   to the walker, labelled as such.
3. **Session index.** Keep the index in memory for the session so later
   queries, including Search Subfolders While Typing, answer from memory with
   no walk. Never persist it to disk. Release it after an idle period, behind
   a "Keep the volume index in memory" setting. An open Disk Usage scan of the
   same volume serves as the index directly.
4. **USN journal refresh.** Tail the journal to keep the session index current
   instead of rebuilding it; on a journal gap or wrap, rebuild.
5. **Engine label** in the results header: "NTFS index · 2.300.000 files ·
   3 min ago", "Walker (slow)", so the user always knows which engine
   answered. Hide the "Spotlight" engine choice in Settings off macOS.

Acceptance on Windows: index C: with a few million records within the Fast
NTFS budget; the first query after indexing and every keystroke afterwards
stay under 100 ms with the UI responsive throughout; a file created or renamed
in Explorer appears in results after the USN refresh; declining UAC returns
walker results with the fallback label; killing the helper mid-query degrades
to the walker without freezing the window.

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
- Native action availability comes only from advertised capability, for both
  files and containers. It is never inferred from row kind.

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
  `Ferail-<version>-x64-symbols.zip` (named `…-win-x64-symbols.zip` through
  0.6.6) whose `manifest.json` commit and
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
