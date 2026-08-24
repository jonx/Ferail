# Windows Reliability Test Plan

← [Windows compatibility campaign](../features/WINDOWS_COMPATIBILITY_PLAN.md) ·
[Windows handover](WINDOWS_HANDOVER.md) ·
[Windows port notes](../features/windows-port.md) · [Open work](../../TODO.md)

## Purpose

This is the reproducible acceptance procedure for the Windows corrections
tracked in `WINDOWS_COMPATIBILITY_PLAN.md`. It is run on Windows, against the
actual packaged artifact as well as a diagnostic build. It is not satisfied by
CI compilation, unit tests, a macOS run, or “it worked once.”

The most important regression constraint is scale: Ferail must continue to
browse and interact with several million files using the same compact,
virtualized filesystem path it has today. Windows Shell compatibility must not
put a COM object, PIDL, `PathBuf`, thumbnail, or provider state on every normal
row.

Status notation for a test record:

- `[ ]` not run
- `[x]` pass
- `[F]` fail — attach evidence and issue id
- `[N/A]` capability unavailable on this machine, with reason

## 1. What constitutes a valid run

Record the following before testing:

| Field | Value to record |
| --- | --- |
| Ferail version and full commit | Packaged `cli\ferail.exe --version` plus `git rev-parse HEAD` |
| Artifact | diagnostic build / portable ZIP / installer |
| Signature | signer and `Get-AuthenticodeSignature` status |
| Windows | edition, exact build, update level, language |
| Machine | CPU model, logical processors, RAM, GPU/driver |
| Displays | resolution, DPI scaling, monitor count |
| Storage | filesystem, local/removable/network, free space |
| Providers | OneDrive status, preview handlers, 7-Zip/Git/AV extensions |
| Settings | previews, folder sizes, file details, hidden files, language |
| Profile | clean profile or upgraded profile and source version |

Write evidence under `target/test-reports/windows/<commit>/<run-id>/` so it is
kept out of source control. Each failed case retains:

- the case id and exact step
- screenshot or short screen recording
- Ferail redacted log/report
- matching `.dmp` and PDB identity for a crash/hang
- before/peak/after CPU, working set, handle count, thread count, and redraws
  when performance is involved

Do not use a personal photo library for a shareable report. Reproduce with the
synthetic fixtures below. If a personal library is necessary, inspect and
redact the report locally before it leaves the machine.

## 2. Required Windows environments

### WENV-A — primary development machine

- Fully patched supported Windows 11, physical machine preferred.
- Visual Studio Build Tools/Windows SDK, Rust MSVC toolchain, WinDbg.
- PDB-producing Debug and Release-with-diagnostics builds.
- At least one common third-party context-menu extension.
- At least two preview providers, including PDF if available.
- OneDrive Files On-Demand and, when available, an MTP phone.
- A local NTFS volume and a UNC share.

This environment runs every case.

### WENV-B — constrained machine

- The oldest Windows version Ferail promises to support.
- Four logical processors, 8 GB RAM, integrated GPU or a VM with equivalent
  limits.
- 100%, 150%, and 200% display-scale passes for UI-sensitive cases.

This environment runs smoke, 10k images, ordinary large-folder work, open /
reveal, clipboard, context menu, and packaging. The four-million-row fixture
may run on WENV-A only if disk capacity makes it impractical here, but WENV-B
must open and scroll the one-million fixture.

### WENV-C — clean release machine

- Fresh Windows Sandbox or clean VM.
- No Rust, Visual Studio, VC++ redistributable installed deliberately, Office,
  custom codecs, or Ferail user profile.
- Network disabled for the first portable-ZIP launch.

This environment proves the package is self-contained and fallbacks work when
optional Shell providers do not exist.

## 3. Test corpora

Create one deterministic developer tool or script for these fixtures and
record its revision and manifest checksum. Generation is done once outside the
timed run. The generator must be able to verify counts without loading every
path into memory.

| Corpus | Required shape and purpose |
| --- | --- |
| `WCORPUS-SMALL` | 500 items: folders/files, empty and non-empty, Unicode, spaces, `#`, `%`, `!`, very long names/paths, hidden/read-only files, symlink/junction, local and broken `.lnk`, and mixed supported/unknown extensions. |
| `WCORPUS-OPEN` | Associated JPEG, PNG, PDF, TXT, MP4, archive, executable, no-association file, folder, UNC item, and special-character/long-path copies. |
| `WCORPUS-MEDIA-10K` | Exactly 10,000 visible rows: 8,500 small JPEG/PNG images, resized/oriented examples, 250 videos, 250 PDFs, 500 unsupported files, 250 corrupt/truncated media files, and 250 folders/shortcuts. Contents may be repeated but identities/mtimes vary. |
| `WCORPUS-WIDE-100K` | 100,000 files in one directory. Exercises ordinary listing, selection, file details, sort/filter, and avoids confusing Flat View with a wide directory. |
| `WCORPUS-FLAT-1M` | Exactly 1,048,576 zero-byte files beneath 1,024 directories, with deterministic names and an average relative path near 80–100 UTF-16 code units. |
| `WCORPUS-FLAT-4M` | Exactly 4,194,304 zero-byte files beneath 4,096 directories. This is the non-negotiable current-scale release gate. |
| `WCORPUS-NAMESPACE` | This PC, Recycle Bin with recoverable test items, OneDrive hydrated and placeholder files, an MTP device if present, and a disconnected-device case. |
| `WCORPUS-CLIPBOARD` | Local/UNC/multiple-parent/special-name sets, a OneDrive placeholder, `.lnk`, and Shell-only items for copy, cut, paste, and drag tests. |
| `WCORPUS-METADATA` | Known-answer JPEG/TIFF/HEIC images with EXIF orientation, camera, date, exposure and GPS/no-GPS variants; malformed files; a Windows property-provider document. |

The million-file corpora contain no thumbnails or content. That is deliberate:
they measure the row/path architecture. The 10k-media corpus separately tests
expensive providers. Never enable an eager operation that opens four million
files just to make the scale test “more realistic.”

Before every timed run:

1. verify the expected file count from the corpus manifest;
2. record whether the filesystem cache is cold or warm;
3. close unrelated applications and pause deliberate background indexing;
4. start Ferail with a clean diagnostic log;
5. wait for baseline process statistics to settle for 30 seconds.

## 4. Baseline and performance measurements

Capture a pre-change baseline on the same machine, commit, settings, and
corpus. Later results compare to that record; never compare a Debug build with
a Release build.

Required measurements:

| Metric | How it is interpreted |
| --- | --- |
| Time to first visible batch | Navigation request to usable rows and input. |
| Time to scan complete | Final count/progress completion; UI may remain usable throughout. |
| Input latency | Visible response to wheel, keyboard navigation, selection, and menu command. |
| UI heartbeat gaps | Any gap over 250 ms is investigated; 1 s is an automatic failure. |
| Scroll frame time | p95 and worst observed during a fixed end-to-end pass. |
| CPU | Task Manager-normalized process CPU; core-equivalent retained separately. |
| Redraw rate | Actual window redraws per second, not I/O requests. |
| Memory | Pre-scan steady, peak, scan-complete, after leaving Flat View, and after two minutes. |
| Resources | Process handles, GDI objects, USER objects, threads, helper processes. |
| Work volume | queued/running/cancelled/completed preview and detail requests. |

Global acceptance:

- Normal directory first paint, scrolling, keyboard input, selection, and the
  ordinary Ferail context menu do not regress more than 5% from baseline.
- No action which should be constant/bounded at scale grows linearly with the
  selected or total row count.
- No completion for an off-screen preview/detail batch causes a whole-table
  refresh or visible scroll-position change.
- At idle, worker queues drain, unnecessary redraws stop, and process CPU
  converges near Task Manager's idle noise floor.
- A Shell provider may be slow only inside an isolated helper after an explicit
  user request; it may not stall Ferail's heartbeat.

## 5. Build, automated, and package gate

Run from a Windows Developer PowerShell:

```powershell
rustc -Vv
cargo -V
cargo test --workspace
cargo clippy -p ferail-gpui --all-targets -- -D warnings
cargo build --release --bin ferail-gpui --bin ferail --features mpv
./scripts/package-win.ps1
```

When helper binaries land, packaging tests must assert that every required
helper is present, version-matched, architecture-matched, and signed with the
main executable.

- [x] `WTEST-001` All commands above pass on WENV-A. *2026-08-24 at
  `580d193` plus the WIN-015 packaging changes: fmt, 653 workspace tests,
  workspace-wide strict clippy, release build, and a full `package-win.ps1`
  run all pass; logs under
  `target/test-reports/windows/580d193/2026-08-24-session1/`.*
- [ ] `WTEST-002` Bundled keybindings resolve; startup has no unknown built-in
  command, especially `view.toggle_flat`.
- [x] `WTEST-003` Package dependency inspection finds no undeclared runtime
  DLL. *2026-08-24: both packaged binaries import 34 Windows system DLLs and
  nothing else — no `vcruntime*`/`msvcp*`/`ucrtbase`/`api-ms-win-crt-*`; the
  gate now fails packaging if one appears.*
- [ ] `WTEST-004` Portable ZIP launches offline on WENV-C.
- [ ] `WTEST-005` Installer install, launch, upgrade, uninstall, and retained
  user-settings behavior pass on WENV-C.
- [ ] `WTEST-006` `Get-AuthenticodeSignature` is valid for the executable,
  helpers, and installer intended for public release.

Evidence: command transcript, package file hashes, dependency report,
signature output, and clean-machine recording.

## 6. Exact interactive cases

### A. Startup, shutdown, and diagnostics — WIN-001/WIN-015/WIN-016

- [ ] `WTEST-010` Launch/quit the diagnostic build 100 times with no leaked
  `InputState`/`TableState` assertion, orphan helper, or rising handle count.
- [ ] `WTEST-011` Open/close 20 windows and 100 tabs in varied order; final
  shutdown is clean.
- [ ] `WTEST-012` Force a diagnostic UI-thread stall. The report contains a
  symbolized stack, last activities, active task/provider ids, and no raw path
  in default redacted mode.
- [ ] `WTEST-013` Force a helper crash. Ferail stays interactive, reports the
  helper role/provider, offers/falls back safely, and can service the next
  request with a restarted helper.
- [ ] `WTEST-014` Start with a profile upgraded from 0.6.5 and with a clean
  profile. Built-in commands and settings resolve identically.

Release blocker: any in-process crash, shutdown assertion, unsymbolizable dump,
or orphan helper.

### B. Ordinary browsing and background work — WIN-005/WIN-006

Run with file details enabled and eager full-folder indexing disabled.

- [ ] `WTEST-020` Open `WCORPUS-WIDE-100K`. First detail work is limited to
  visible rows plus documented overscan; queued work is not near 100,000.
- [ ] `WTEST-021` Scroll one page at a time. Format and Description populate
  for newly visible rows; already cached rows return without content reads.
- [ ] `WTEST-022` Rapidly scroll top→bottom→top, then navigate away while work
  is pending. Stale generations do not update the new folder.
- [ ] `WTEST-023` Wait idle. Queues drain, disk reads cease, and off-screen
  completions do not keep redraw rate elevated.
- [ ] `WTEST-024` Enable optional idle whole-folder indexing, interact during
  the pass, then disable it. Foreground input preempts it; cancellation is
  prompt and partial cache results remain valid.
- [ ] `WTEST-025` Compare Ferail CPU with Task Manager at idle, under a
  controlled one-thread load, and under multi-worker load. The status CPU uses
  Task Manager semantics and cannot show 700%; diagnostics may show core
  equivalents.
- [ ] `WTEST-026` Verify the redraw label/tooltip clearly says redraws and the
  stats sampler does not itself force continuous repaint.

Release blocker: whole-folder work on ordinary navigation, >1 s heartbeat gap,
or loss of Format/Description while the feature is enabled.

### C. Million-row Flat View — non-negotiable cross-platform scale gate

Run Release on WENV-A with previews/folder sizes/file details set to their
normal user defaults. Flat View itself must automatically keep work
viewport-scoped.

- [ ] `WTEST-030` Scan `WCORPUS-FLAT-1M`; final count is exactly 1,048,576 and
  no cap/truncation message appears.
- [ ] `WTEST-031` Scan `WCORPUS-FLAT-4M`; final count is exactly 4,194,304 and
  progress/cancel remain responsive throughout.
- [ ] `WTEST-032` During both scans, continuously scroll, resize, switch
  list/grid where supported, change columns, and use keyboard navigation. No
  one-second heartbeat gap or lost input is allowed.
- [ ] `WTEST-033` At 4M, jump/scroll near start, middle, and end. Names and Path
  cells are correct, scroll position does not jump when detail results arrive,
  and row geometry remains usable.
- [ ] `WTEST-034` Select All at 4M. Completion is under 100 ms on the reference
  machine and incremental memory is under 16 MB; status count is immediately
  correct. Toggle exclusions and clear selection without materializing four
  million ids.
- [ ] `WTEST-035` Open the normal context menu on a visible selected row after
  Select All. Menu construction stays bounded and does not resolve four
  million paths. Commands which cannot safely target that selection are
  disabled/explained.
- [ ] `WTEST-036` Filter at 4M, cancel midway, clear filter, and refresh the
  Flat snapshot. UI remains responsive and the authoritative count is restored.
- [ ] `WTEST-037` Toggle Flat View off after the 4M scan. Surface-owned path
  arenas/ids/rows drop immediately. After two minutes, retained logical bytes
  are within 5% of the pre-scan live allocation baseline and process working
  set is no worse than the pre-change Windows baseline plus 10%.
- [ ] `WTEST-038` Repeat enter→complete→leave three times. Peak memory, handles,
  threads, and completion time do not grow monotonically.
- [ ] `WTEST-039` Start Copy File List at 4M, verify yielding progress and
  cancel promptly. A successful full clipboard copy is measured separately and
  may require large memory, but the UI may not freeze or build a second
  all-paths vector on the UI thread.

Scale acceptance:

- peak working set at 4,194,304 rows must not exceed the pre-change Windows
  baseline by more than 10%; the known cross-platform reference is roughly
  1.1 GB for about 4.1 million rows, but the Windows baseline captured on the
  same machine is authoritative;
- Select All stays symbolic;
- no PIDL, COM object, thumbnail state, full `PathBuf`, or provider-property
  map is allocated per filesystem row;
- scan and apply remain O(entries), while painting/selection/menu preparation
  remain viewport- or summary-bounded.

Release blocker: truncation, input freeze, per-row Shell state, repeat-scan
growth, or >10% memory/performance regression.

### D. 10k previews, scrolling, and hostile providers — WIN-002/WIN-003/WIN-011

- [ ] `WTEST-040` Open `WCORPUS-MEDIA-10K` in list and grid. Initial requests
  cover only the viewport plus overscan; worker and upload queues stay within
  configured bounds.
- [ ] `WTEST-041` Perform ten top↔bottom scroll passes at varied speeds. The
  scrollbar remains visible/stable, rows never reset to the top, and a stale
  thumbnail never lands on a reused row.
- [ ] `WTEST-042` Toggle previews off/on during load; pending work cancels and
  generic icons remain correct.
- [ ] `WTEST-043` Resize icon/thumbnail size repeatedly while scrolling. Old
  size generations are dropped; memory settles after each pass.
- [ ] `WTEST-044` Navigate between two media folders quickly 50 times. No old
  folder preview appears in the current folder.
- [ ] `WTEST-045` Preview safe PDF/image/video files and corrupt variants.
  Every failure becomes a fallback, never a crash.
- [ ] `WTEST-046` Test a deliberately crashing preview provider in the broker.
  Ferail survives, the provider is quarantined for the session, and later safe
  providers still work.
- [ ] `WTEST-047` Test a provider that never returns. Deadline terminates only
  that preview broker and leaves navigation/scroll/menu responsive.
- [ ] `WTEST-048` Inspect Fonts, shortcuts, folders, known types, and unknown
  types at multiple DPI values. Provider refusal produces a correct generic
  icon, not an error badge.

Release blocker: any third-party DLL loaded in Ferail, disappearing scrollbar,
whole-list repaint loop, unbounded queue/cache, or crash.

### E. Multi-selection — WIN-004

- [ ] `WTEST-050` Single, Ctrl-toggle, Shift-range, keyboard range, and
  right-click selection semantics match the documented selection rules.
- [ ] `WTEST-051` Right-click an already-selected row: the selection remains a
  group. Right-click outside it: selection collapses to the clicked row.
- [ ] `WTEST-052` Delete/rename/move rows while selection, preview, and context
  menu work are in flight; stale indices are not used.
- [ ] `WTEST-053` Repeat with 1, 100, 10,000, and symbolic 4M selection sizes.
- [ ] `WTEST-054` Start clipboard/drag/file operation from a large selection,
  cancel where supported, and confirm UI work is yielded/bounded.

Release blocker: crash, wrong target set, or eager path/id materialization for
Select All.

### F. Open and Reveal — WIN-008/WIN-009

For every item in `WCORPUS-OPEN`, repeat from double-click, Enter, context menu,
and command palette where available.

- [ ] `WTEST-060` Files launch the Windows default `open` verb. A JPEG never
  opens Print unless Print is deliberately configured as the default.
- [ ] `WTEST-061` Folders navigate inside Ferail; namespace folders use their
  provider capability.
- [ ] `WTEST-062` No-association and missing-target cases return an actionable
  error without claiming success merely because a launcher started.
- [ ] `WTEST-063` Drive, UNC, spaces, Unicode, `#`, `%`, `!`, and long/verbatim
  internal paths all open correctly.
- [ ] `WTEST-064` Reveal opens Explorer at the exact parent with the intended
  item selected for the entire path matrix, including the original
  `paths#é #!` shape.
- [ ] `WTEST-065` Reveal a deleted item. Ferail opens the closest valid parent
  or reports that it vanished; it must not silently open Documents.

Release blocker: `cmd /C start` remains in the Windows default-open path,
Explorer opens an unrelated default folder, or path characters change.

### G. Native Windows context menu — WIN-007

- [ ] `WTEST-070` Normal right-click opens the Ferail menu at baseline speed
  and creates no helper, COM/PIDL query, or prefetch task.
- [ ] `WTEST-071` **More options from Windows…**, `Shift`+right-click, and
  `Shift+F10` open the real Windows menu only after invocation.
- [ ] `WTEST-072` Test built-in, 7-Zip, Git, Defender/AV, owner-draw item,
  dynamic submenu, disabled item, Properties, and Cancel.
- [ ] `WTEST-073` Invoke rename/delete/archive/provider verbs and verify the
  affected Ferail view refreshes while preserving valid selection/scroll.
- [ ] `WTEST-074` Same-parent multi-selection reaches the native menu. A
  mixed-parent or unsupported virtual selection is disabled/explained without
  collapsing or mis-targeting the selection.
- [ ] `WTEST-075` Crash and hang a context-menu handler before popup display.
  Only the broker fails; Ferail remains interactive and normal right-click
  continues to work.
- [ ] `WTEST-076` Leave the native menu open for two minutes. It is not killed
  as a “timeout”; keyboard/mouse submenu behavior remains native.
- [ ] `WTEST-077` After cancel/invoke, the helper exits on idle policy and no
  PIDL/HMENU/COM/USER handle leaks across 100 opens.

Release blocker: any selection/hover prefetch, Shell extension in Ferail's
process, normal-menu regression, or uncontained handler failure.

### H. Shortcuts — WIN-010

- [ ] `WTEST-080` Open `.lnk` targets for a file, directory, application with
  arguments, UNC path, and Shell item; behavior matches Explorer.
- [ ] `WTEST-081` Broken link shows a recoverable/broken state and does not
  navigate to a fabricated path.
- [ ] `WTEST-082` Rename/copy/move/trash modifies the `.lnk`, not its target.
- [ ] `WTEST-083` Preview/icon uses the Shell identity/target representation
  with a shortcut overlay where available.
- [ ] `WTEST-084` Get Info shows target and arguments from cached off-thread
  resolution; scrolling/rendering performs no link resolution.

### I. Explorer clipboard and drag/drop — WIN-012

For each `WCORPUS-CLIPBOARD` set, test Explorer→Ferail and Ferail→Explorer.

- [ ] `WTEST-090` Copy/paste one and many normal local files.
- [ ] `WTEST-091` Cut/paste preserves move semantics and clears cut state only
  after successful completion.
- [ ] `WTEST-092` Drag/drop copy and move obey modifier/drop-effect semantics.
- [ ] `WTEST-093` Repeat with UNC, Unicode/special names, long paths, multiple
  source parents, and `.lnk`.
- [ ] `WTEST-094` Hydrated and placeholder OneDrive files materialize only
  after explicit transfer and expose progress/cancel when delayed.
- [ ] `WTEST-095` Shell-only/MTP items use supported data-object formats or
  produce an explicit unsupported message; they never silently disappear.
- [ ] `WTEST-096` Repeat one matrix pass with another Windows file manager to
  catch assumptions specific to Explorer.

Release blocker: common `CF_HDROP` regression, wrong copy/move effect, UI-thread
materialization, or silent failure.

### J. Shell namespace and Recycle Bin — WIN-013

- [ ] `WTEST-100` Sidebar names distinguish the filesystem Desktop folder from
  This PC/Windows namespace; each opens the correct location.
- [ ] `WTEST-101` This PC lists drives/devices without routing local drive
  contents through namespace COM enumeration.
- [ ] `WTEST-102` Navigate Recycle Bin, inspect an item, invoke supported native
  actions, and refresh after restore/empty/delete operations.
- [ ] `WTEST-103` Navigate OneDrive root, hydrated folder, placeholder folder,
  and offline/error case.
- [ ] `WTEST-104` Connect, browse, disconnect, and reconnect an MTP device.
  Stale PIDLs become an unavailable state; Ferail does not dereference or
  persist raw pointers.
- [ ] `WTEST-105` Exercise breadcrumb parent, back/forward, new tab, refresh,
  properties, context menu, clipboard, and drag in pathless locations.
- [ ] `WTEST-106` Re-run local NTFS navigation and Flat 1M after namespace work;
  timings/memory stay within the global 5%/10% gates.

Release blocker: path fabrication for virtual items, raw PIDL lifetime bug, or
ordinary directories switching to Shell enumeration.

### K. Metadata and Properties — WIN-014

- [ ] `WTEST-110` Known-answer EXIF fields match fixture manifests for JPEG,
  TIFF, and HEIC where supported; orientation is not double-applied.
- [ ] `WTEST-111` Malformed metadata fails safely and leaves normal file info
  available.
- [ ] `WTEST-112` Metadata loads on demand off-thread and caches by identity /
  revision; repeated repaint/scroll performs no parse.
- [ ] `WTEST-113` GPS is hidden until deliberately revealed, not written into
  normal logs/reports, and follows the approved persistence policy.
- [ ] `WTEST-114` **Windows Properties…** opens the native property surface for
  a file, folder, shortcut, and supported namespace item without blocking
  Ferail.

### L. Privacy and failure recovery

- [ ] `WTEST-120` Search logs, crash reports, helper logs, cache/database, and
  temporary directories after the media/metadata suite. No preview pixels,
  thumbnail files created by Ferail, EXIF values, clipboard contents, or raw
  personal paths are retained contrary to policy.
- [ ] `WTEST-121` Disconnect network/removable/provider storage during open,
  preview, clipboard, and namespace enumeration. Ferail cancels/fails visibly
  and remains usable.
- [ ] `WTEST-122` Suspend/resume and sign-out/shutdown with helpers/workers in
  flight. No corrupt cache, leaked helper, or startup recovery loop follows.
- [ ] `WTEST-123` Low-memory/resource-pressure pass: provider work sheds or
  falls back before the main process crashes.

## 7. Cross-platform regression after the Windows pass

Windows compatibility is not accepted by moving shared behavior backwards.
On current macOS and the supported Linux test environment:

- [ ] `WTEST-X01` ordinary listing and Flat 1M scan remain within their saved
  performance/memory baselines;
- [ ] `WTEST-X02` normal context menus, selection semantics, open/reveal,
  clipboard, preview, Format and Description still work;
- [ ] `WTEST-X03` platform capability absence hides Windows-only entries
  cleanly—no disabled “Windows” placeholders on macOS/Linux;
- [ ] `WTEST-X04` helper packaging and Windows DTOs add no per-row allocation
  or process startup on non-Windows targets;
- [ ] `WTEST-X05` all localization and command-catalogue tests pass.

## 8. Test cadence

### Per relevant commit

Run automated tests, `WCORPUS-SMALL`, the changed issue's cases, normal
right-click latency, and Flat 1M smoke.

### At the end of each implementation phase

Run every case mapped to that phase plus Flat 4M, 10k previews, 100-cycle
shutdown/helper leak checks, and the primary macOS/Linux regressions.

### Release candidate

Run the entire document on WENV-A, the defined subset on WENV-B, and packaging
on WENV-C using the exact signed artifacts intended for publication. A retest
after any release-blocking fix covers the fixed case, its subsystem section,
startup/shutdown, Flat 4M, and 10k preview smoke—not only the single failing
step.

## 9. Sign-off record

Create one copy of this block per release candidate:

```text
Version / commit:
Artifact SHA-256:
Windows environments:
Tester(s):
Date:

Automated gate:           PASS / FAIL
Startup & diagnostics:    PASS / FAIL
Ordinary browsing:        PASS / FAIL
Flat 1M / 4M:             PASS / FAIL
10k previews/providers:   PASS / FAIL
Selection:                PASS / FAIL
Open / Reveal:            PASS / FAIL
Native context menu:      PASS / FAIL
Shortcuts:                PASS / FAIL
Clipboard / drag:         PASS / FAIL
Namespace / Recycle Bin:  PASS / FAIL
Metadata / Properties:    PASS / FAIL
Privacy / recovery:       PASS / FAIL
macOS/Linux regression:   PASS / FAIL

Known failures and issue links:
Release decision and approver:
```

No release is approved with an uncontained crash, UI-thread stall, package that
does not launch cleanly, incorrect destructive-operation target, or regression
of the four-million-row gate.
