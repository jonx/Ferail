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

## Current resume point

- Branch: `main`
- Current commit: `3cc8b7e` (`release: Ferail 0.6.8 Windows drag
  responsiveness`)
- Current Windows release: `0.6.8`
- Published artifacts: unsigned portable Windows x64 ZIP plus its matching
  x64 symbols ZIP
- Next campaign: implement the remaining shared contracts and UI on macOS,
  then finish and accept their Windows mechanisms on a real Windows machine;
  see the
  [Mac-first continuation plan](../features/WINDOWS_COMPATIBILITY_PLAN.md#continuation-plan-mac-first-windows-final).
- Added scope: restore WSL distributions as a cached dynamic **Linux**
  location, adapted from the pinned Ferail-Win32 reference. Implement its
  neutral stopped/starting/ready contract on macOS first; perform registry,
  `wsl.exe`, UNC and symlink work only on Windows (WIN-017/WTEST-130–139).

User acceptance reported on 2026-08-25 against the current Windows work:

- 10,000-image preview browsing works;
- multi-selection works;
- Flat View works with roughly four million files;
- default Open and Reveal in Explorer work;
- the native Windows context menu works;
- the portable package launches and works in a clean Windows Sandbox.

This is valid feature-level Windows acceptance and should not be discarded.
It is not evidence that every adversarial or measured subcase in
`WINDOWS_RELIABILITY_TEST_PLAN.md` ran: provider crash/hang injection,
100-cycle leak soaks, exact latency/memory thresholds, every unusual path,
every third-party Shell extension, and multi-DPI passes remain separately
tracked until their evidence is recorded.

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

### 2026-08-25 — macOS preparation for WIN-013 Shell namespace

```text
Date / machine: 2026-08-25, macOS development machine
Start commit: 6622f0d (WIN-017 WSL locations)
Preparation commits: ced5588 (pure platform namespace contract)
Reference source:
  - /Users/jkn/Source/ShellBat at
    7b8268d60648b74f5a874be8edde3b4fc17f7cdc
  - upstream: https://github.com/smourier/ShellBat
Implemented in shared code:
  - LocationTarget distinguishes an ordinary FileSystem(PathBuf) handoff from
    a pathless PlatformLocation;
  - provider and item identities have redacted Debug output; a row contains a
    compact non-zero integer id, never a parsing name, PIDL, COM pointer or
    Windows type;
  - the provider instance is owned by exactly one tab session, so its Windows
    identity arena will drop with the tab and cannot be mixed with another
    provider's ids;
  - streamed batches are capped at 512 rows, generation checked, cancelled on
    navigation/drop, and have explicit loading/ready/unavailable states;
  - the worker/UI channel holds at most four batches (2,048 rows); a full queue
    backpressures the provider, a dropped receiver stops it, and a provider
    returning without a final batch fails visibly instead of loading forever;
  - the specialized GPUI surface is virtualized and renders only provider DTOs;
    cached breadcrumbs, Back/Forward, Refresh, double-click/Enter and arrow-key
    selection route through the provider while filesystem targets immediately
    return to NativeFs;
  - batches for a background tab update its bounded model but do not notify and
    redraw the unrelated active surface;
  - platform selection is complement-based, so Select All is O(1) even for a
    provider exposing millions of items;
  - ordinary filesystem navigation drops the specialized session before
    loading through NativeFs. FileEntry, Flat View and NodeStore are unchanged.
ShellBat semantics deliberately adopted for the Windows provider:
  - retain the desktop-absolute Shell identity independently from an optional
    SIGDN_FILESYSPATH and hand real filesystem directories to NativeFs;
  - obtain parent/breadcrumb identity from IShellItem::GetParent;
  - enumerate pathless children through BHID_EnumItems / IEnumShellItems;
  - map actions from SFGAO capabilities rather than guessing from item kind.
ShellBat mechanics deliberately not copied:
  - no full-list materialization or sorting before display;
  - no one-row UI apply loop; Ferail streams bounded batches;
  - no Shell/extension call on the GPUI thread and no personal parsing identity
    in a shared row, report, persistent store or cache key.
Native context-menu requirement:
  - preserve the instant Ferail menu as default;
  - request the real Windows menu only through More options / Shift+right-click
    / Shift+F10, for both files and directories;
  - for pathless provider files and containers, pass the selected owned Shell
    identities to the isolated broker only when NATIVE_MENU is advertised;
  - never prefetch QueryContextMenu on selection, navigation, hover or render.
Host tests passed:
  - cargo test -p ferail-core platform_namespace (8 passed)
  - cargo check -p ferail-gpui
  - cargo test -p ferail-gpui platform_namespace (9 passed)
Windows cases claimed: none. WTEST-100–106 and the expanded WTEST-072 remain
  real-Windows gates.
Next exact shared work on macOS:
  1. Add seeded/screenshot access to the in-memory provider surface and finish
     visual polish without adapting rows into FileEntry or fake PathBuf values.
  2. Add capability-gated platform action plumbing for properties, transfers,
     icons and the explicit native menu request; unsupported actions stay
     absent or explain why they cannot run.
  3. Keep the pathless status bar/filter semantics distinct from the stale
     filesystem tab state before exposing the first Windows location.
Next exact Windows work:
  1. Add a ferail-shell-win32 provider whose tab-owned arena stores copied
     absolute PIDL bytes plus an optional desktop-absolute parsing name. Never
     retain borrowed ITEMIDLIST pointers or COM objects across apartments.
  2. Construct/reconstruct IShellItem on its worker STA, enumerate with
     BHID_EnumItems, retrieve SIGDN_NORMALDISPLAY / SIGDN_FILESYSPATH /
     SIGDN_DESKTOPABSOLUTEPARSING as needed, and map SFGAO capability flags.
  3. Emit no more than 512 DTOs per batch and stop promptly on cancellation;
     a disconnected item becomes NotFound/Unavailable, never a stale-pointer
     dereference.
  4. Extend the isolated context-menu broker input from same-parent paths to
     owned namespace identities. Validate separate file and directory menus,
     then provider file/container menus, without restoring prefetch.
  5. Execute WTEST-100–106, WTEST-072 and WTEST-121, then re-run local NTFS and
     Flat 1M/4M performance baselines.
Working-tree files intentionally excluded from this slice:
  - crates/ferail-gpui/src/shell/file_ops.rs
  - crates/ferail-gpui/src/shell/lock_info.rs
  They contain concurrent formatting/work and must not be staged with WIN-013.
```

At handover creation, two unrelated user changes intentionally remain outside
the Windows commits: `CHANGELOG.md` and
`crates/ferail-gpui/src/file_list.rs` (Flat row-buffer release work). Preserve
any equivalent parallel changes that are still present after transferring the
worktree; do not stage them into a Windows feature commit.

## Historical implementation entering the first Windows acceptance pass

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

### D. Shell namespace and Windows metadata — WIN-010 onward

Add platform capability interfaces behind the shared application, not a
Windows fork. Ordinary filesystem folders continue through `NativeFs` with no
per-row COM/PIDL state. Shell namespace locations, Recycle Bin, MTP and virtual
OneDrive roots use Windows-owned location tokens only when no useful direct
path exists.

## Current remaining work

The original crash report's principal P0 user flows now pass in real Windows
use. Resume implementation in this order:

1. Close the bounded-work contracts shared by previews, thumbnails, file
   details and selection: process-wide I/O/cache budgets, per-frame apply
   limits, affected-row invalidation, stable row geometry, and stale-selection
   regression tests.
2. Complete path-based interoperability: actionable Open/Reveal failures,
   targeted refresh after a native Shell verb, `.lnk` resolution, and the
   Explorer clipboard/drag format matrix.
3. Qualify the source-complete WSL path-backed location slice through
   WTEST-130–139: cached installed distributions, no implicit start, explicit
   stopped→starting→path activation, then a `NativeFs` handoff. Only after that
   gate, follow with PIDL-backed This PC, Recycle Bin, provider roots and MTP.
   Never put WSL provider state, PIDLs or COM state on ordinary filesystem or
   Flat View rows.
4. Complete metadata and Properties: shared cached metadata DTOs first,
   Windows `IPropertyStore` and the native Properties action second.
5. Qualify the exact release artifacts: clean/upgraded profiles, constrained
   machine and DPI passes, helper/handle soaks, privacy audit, Authenticode,
   then macOS/Linux regression.

The detailed sequence, ownership boundaries and exit gates live in the
implementation ledger's continuation plan. Do not restart the removed native
menu prefetch while doing any of this.

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
  Final repackage at a4c8353 (all of the above): target/package/
  Ferail-0.6.5-win-x64.zip + -symbols.zip; gate passed with 35 system DLLs
  (dbghelp.dll now imported by the minidump filter). Unsigned.
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

### 2026-08-24 (continued) — bounded assets, deterministic teardown, packaging guard

```text
Date / machine: 2026-08-24, same Windows 11 dev machine
Start commit: b74359e, with shared uncommitted Windows thumbnail/icon/PDF and
  diagnostics work already present in the tree.
End commit(s): none in this pass; the shared working tree remains intentionally
  uncommitted so unrelated concurrent work is not captured accidentally.
Cases passed:
  - Exact WIN-001 InputState repro:
      cargo run -p ferail-gpui --bin ferail-gpui -- --screenshot
        target/windows-audit/get-info-leak-final.png
        --navigate C:\Source\Ferail --properties
    exits 0. An intermediate build drained 76 next-frame callbacks but retained
    one Root-held InputState and exited 101; removing screenshot windows before
    App::quit closes that final ownership edge.
  - ferail-shell-win32: 19 tests passed, including corrupt/zero-length PDF
    deadline coverage and verbatim UTF-16 path handling.
  - Full workspace: 671 unit/integration tests passed, 0 failed (plus expected
    ignored network/doc examples); workspace all-target Clippy with -D warnings,
    cargo fmt and git diff --check passed.
    Final targeted rerun after the UTF-16 edge-case test: shell-win32 20/20,
    ferail-gpui 281 passed + 1 expected network ignore, strict Clippy green.
  - The packaged feature set compiles with
    `cargo check -p ferail-gpui --no-default-features`.
  - Preview requests are latest-wins with active cancellation; Windows kills
    and waits for a superseded broker. Thumbnail warming is bounded to one
    active batch plus the latest pending viewport per visible surface.
  - PDF WinRT open/load/render/read stages share one five-second deadline and
    cancel the outstanding WinRT operation on expiry.
  - Crash breadcrumbs now cover navigation generation, selection, preview and
    thumbnail lifecycle without recording full paths.
  - Packaging now rejects a dirty tree unless -AllowDirty is explicit and asks
    Cargo for release line-table debug information for the symbols archive.
    The dirty-tree rejection was exercised on this 37-path shared tree.
Cases failed + evidence paths:
  - First teardown attempt: %APPDATA%\Ferail\reports\ferail-crash-11480.txt
    (one InputState after callback drain); fixed by the final window-removal
    step above.
Not run / still manual:
  - Real hostile pdfprevhndlr.dll matrix, interactive 10k-media scroll and
    rapid-selection stress, full open/reveal acceptance, WENV-C clean sandbox,
    multi-DPI visual corpus, and an actual clean-tree release package.
Known limitations:
  - The gpui-component strong InputState captures remain upstream; production
    packages exclude the developer leak assertion, while dev screenshot
    teardown now proves clean. The real app-level Quit action is wired through
    the same cleanup; the full human-driven quit matrix is still pending.
  - Thumbnail concurrency is bounded per visible FileList surface, not yet by
    one process-wide cross-window asset semaphore.
  - Reveal failure falls back to the nearest existing parent and logs a precise
    error, but there is not yet an in-app actionable toast.
Next exact work:
  1. Commit or otherwise isolate the shared tree, then run package-win.ps1 from
     a clean checkout and record portable/symbol PDB identities.
  2. Run the interactive acceptance slice and WCORPUS-MEDIA-10K at multiple DPI.
  3. Exercise a genuinely hanging third-party preview handler during rapid
     selection and verify immediate broker death/no descendants.
  4. Run WTEST-004 in a pristine offline Windows Sandbox.
Working-tree files intentionally left unstaged: all changes listed by git
  status; the tree is shared with another active session.
```

### 2026-08-25 — v0.6.6–0.6.8 consolidation and user acceptance

```text
Date / machine: 2026-08-25, real Windows machine; user-reported interactive
  acceptance. Exact Windows build/hardware and evidence paths were not copied
  into this handover.
Start commit: work following 580d193 / the 0.6.5 investigation
End commit(s): 4f7c322 (0.6.6), 1d65395 (0.6.7), 3cc8b7e (0.6.8)
Cases passed at feature level:
  - WCORPUS-MEDIA-10K / 10,000-image preview use;
  - multi-selection;
  - approximately four-million-row Flat View;
  - default Open and Reveal in Explorer;
  - explicit native Windows context menu;
  - clean Windows Sandbox launch/use of the portable package.
Implemented during this interval:
  - bounded/cancellable preview and thumbnail work, native PDF rendering,
    minidumps, line-table symbols and deterministic packaged teardown;
  - Windows font thumbnails/icons, shortcut overlay and portable photo
    metadata;
  - isolated native Shell menu, Restart Manager lock/eject diagnostics and
    native outbound OLE drag;
  - large-selection, marquee, drag-over and hot-cache responsiveness work.
Cases not claimed by this acceptance record:
  - every numbered adversarial/measurement subcase, including hostile
    providers/extensions, 100-cycle leak/handle soaks, every difficult path,
    exact 4M memory/latency thresholds, multi-DPI and privacy inspection.
New dumps/PDB identity: matching v0.6.8 symbols are published as
  Ferail-0.6.8-x64-symbols.zip; copy manifest GUID/age into the next failure
  record rather than assuming symbols by version alone.
Measurements before/after: no durable measurement transcript added here.
Known regressions: none reported in the accepted flows.
Next exact work:
  1. Start the Mac-first continuation plan in
     docs/features/WINDOWS_COMPATIBILITY_PLAN.md.
  2. Preserve the shared UI and million-row fast path; land neutral DTOs,
     schedulers and contract tests before Windows COM mechanisms.
  3. In Phase E, implement the fake path-backed Linux-root provider before
     returning to Windows for WIN-017 registry/wsl.exe/UNC work. Do not start a
     distro during discovery or sidebar rendering and do not copy registry
     BasePath into shared state or logs.
  4. Return to Windows for each phase's mechanism and acceptance matrix; append
     evidence here before marking its exact WTEST cases complete.
Working-tree files intentionally left unstaged: none at 3cc8b7e.
```

### 2026-08-25 — macOS preparation for WIN-017 WSL locations

```text
Date / machine: 2026-08-25, macOS development machine
Start commit: 3cc8b7e, with the Windows continuation-plan documentation
  already modified but uncommitted
End commit(s): none yet; implementation and documentation remain in the
  working tree for review before an isolated commit
Implemented:
  - ferail-core::platform_locations: opaque root ids, privacy-safe errors,
    stopped/starting/ready/unavailable states and generation-safe discovery /
    concurrent activation store;
  - process-cached GPUI Linux section, absent when no provider roots exist;
    cached-state-only render, explicit click activation, Cmd/Ctrl-click new-tab
    behavior, tab/load-generation guard, explicit Refresh integration and
    localized failure states;
  - platform mirrors on macOS/Linux (empty capability) and a Win32 WSL
    provider using HKCU Lxss discovery with bounded wsl.exe fallback;
  - no MSI InstallLocation dependency and no BasePath read, DTO or log;
  - preferred \\wsl.localhost roots while accepting \\wsl$, extended UNC,
    Unicode/spaces and UTF-8/UTF-16 wsl.exe list output;
  - explicit activation through argv-only `wsl.exe -d <name> --exec
    /bin/true`, no console window, 20 s deadline, cancellation and child wait;
    stdout is drained concurrently, retained only up to 1 MiB and rejected on
    overflow so a full pipe cannot defeat the deadline;
  - direct NativeFs enumeration first; explicit activation of a WSL symlink
    and an empty failed WSL directory load both use a five-second
    `readlink -f --` resolver with generation/cancel guard and checked
    /mnt/<drive> conversion;
  - previews, thumbnails and viewport details remain enabled. The existing
    eager recursive folder-size pass is skipped for WSL until it has a
    viewport/on-demand mode;
  - Move to Recycle Bin fails closed for WSL with a localized explanation;
    permanent deletion remains the existing separately confirmed command.
Host tests passed:
  - cargo check -p ferail-gpui
  - cargo test -p ferail-core platform_locations (8 passed)
  - cargo test -p ferail-gpui platform_locations (4 passed)
  - cargo test -p ferail-shell-win32 wsl::tests (4 passed)
  - cargo test -p ferail-core (125 passed)
  - cargo test -p ferail-gpui --lib (291 passed, 1 network test ignored)
  - cargo clippy -p ferail-gpui --all-targets (passes with three pre-existing /
    concurrent-work warnings outside WIN-017)
  - cargo clippy -p ferail-shell-win32 --target x86_64-pc-windows-msvc
    -- -D warnings
  - cargo test -p ferail-core i18n::extract and i18n::pack
  - git diff --check
Cross-check boundary:
  - cargo check -p ferail-shell-win32 --target x86_64-pc-windows-msvc passes.
  - cargo check -p ferail-gpui --target x86_64-pc-windows-msvc cannot finish
    on macOS: ring/aws-lc C builds require the Windows SDK/MSVC headers
    (`assert.h`, `windows.h`). This is the known toolchain boundary, not an
    application or WSL compile result.
Windows cases claimed: none. WTEST-130–139 still require the real machine.
Known review points for Windows:
  - confirm `wsl.exe --list --running --quiet` starts no distribution and the
    3 s discovery deadline is sufficient on cold WSL service startup;
  - confirm killing a timed-out/cancelled wsl.exe leaves no host child and no
    misleading starting state; inspect distro state separately because WSL may
    already have accepted the start request;
  - verify I/O errors for broken/looping symlinks reach the five-second
    fallback and `/mnt/c` hands off to the local Windows path exactly once;
  - verify Explorer Open/Reveal, clipboard/drag and the explicit native menu
    against WSL paths; no behavior is marked supported solely from UNC theory;
  - inspect logs/report/metadata DB for BasePath, distro names, Linux paths or
    captured command output.
Next exact work on Windows:
  1. Build/test the unchanged tree with the MSVC commands in "First Windows
     session" and resolve Windows-only compiler errors without changing the
     shared DTO contract.
  2. Materialize WCORPUS-WSL in a disposable distro; record `wsl -l -v` before
     launch, after sidebar display, and after explicit row activation.
  3. Execute WTEST-130–139 in order, retaining logs, process/handle counts and
     before/after Flat 1M/4M measurements.
  4. Update WIN-017 checkboxes and this block only from recorded Windows
     evidence; keep unverified items `[~]`.
Working-tree files intentionally left unstaged: all WIN-017 implementation,
  locale and planning files listed by `git status --short`; preserve unrelated
  concurrent user work when isolating the commit.
```
