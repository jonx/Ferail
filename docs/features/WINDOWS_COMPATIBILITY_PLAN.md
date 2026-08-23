# Windows Reliability and Compatibility Plan

← [Windows port notes](windows-port.md) ·
[Windows test plan](../testing/WINDOWS_RELIABILITY_TEST_PLAN.md) ·
[Feature index](README.md) · [Open work](../../TODO.md)

## Status and purpose

This is the execution ledger for the Windows issues reported against Ferail
0.6.5 on 2026-08-23. It turns the tester's observations into bounded work with
an identified failure boundary, a proposed implementation, and a Windows exit
test. It is deliberately separate from [windows-port.md](windows-port.md): that
document explains the port; this one tracks the reliability and compatibility
campaign.

No item is considered shipped because it compiles on macOS or in Windows CI.
The platform work in this plan is implemented and signed off on a real Windows
development machine, then the shared behavior is regression-tested on macOS
and Linux.

Status notation:

- `[ ]` not started
- `[~]` in progress
- `[x]` verified on Windows
- `[!]` blocked, with the blocker written beside the item

Confirmed product decisions:

- Ferail remains one cross-platform application. There will not be a separate
  Windows edition or a forked Windows UI.
- The current Ferail context menu remains the default, instant right-click
  menu on every platform.
- The real Windows Shell context menu is an explicit compatibility action:
  **More options from Windows…**, `Shift`+right-click, and `Shift+F10`.
- Native Shell-menu enumeration is strictly on demand. The old
  selection-change prefetch system will not return.
- Third-party Windows preview and context-menu extensions do not load into the
  Ferail process.
- The direct filesystem path remains the normal fast path. Windows Shell
  namespace enumeration is used only for locations which have no useful
  filesystem path.
- Multi-million-row browsing is a release gate, not an optional benchmark.
  The Windows work may not attach Shell/PIDL/provider state to ordinary rows or
  regress the current four-million-file Flat View behavior.
- Responsiveness and crash containment outrank native integration breadth.
  A missing optional Shell feature may fall back; it may not hang or terminate
  Ferail.

## Evidence retained for the work

The original tester bundle contains:

- `ferail-crash-48428.txt`, `ferail-crash-1584.txt`,
  `ferail-crash-10020.txt`, and `ferail-crash-8856.txt`
- `ferail-hang-10020-0.txt`
- screenshots covering CPU/memory reporting, double-click, Reveal in Explorer,
  `.lnk`, the Windows Fonts folder, and the missing VC++ runtime
- a WinDbg first-chance access violation in an unloaded
  `pdfprevhndlr.dll`
- a 10,821-row run where prefetch completed after roughly 16 seconds, followed
  by a UI heartbeat stall

Reference implementations are pinned so later work is compared with a known
source rather than a moving branch:

- Ferail-Win32: `4fcb2ffb2c622c49b4c333b115588626e4f74245`
- Filociraptor: `c3adf308ead1b9e2badd54e93d754791af8fc18d`
  ([smourier/Filociraptor](https://github.com/smourier/Filociraptor))

The useful Filociraptor lesson is its split path: `ShellLocation` holds a
normal path when one exists and a PIDL only for virtual Shell locations;
`NamespaceScanner` says explicitly that Shell enumeration is about five times
slower and therefore never replaces direct directory enumeration. Its native
context menu is a useful reference for PIDL binding, `IContextMenu`, message
forwarding, `TrackPopupMenu`, invocation, and post-menu refresh. We will not
copy its in-process extension boundary.

Ferail-Win32 confirms why its old prefetch is not coming back:
`menu_preload.rs` started `QueryContextMenu` shortly after every selection
change, and `shell_pump.rs` had to interleave STA work with the UI message pump.
That machinery existed to conceal slow extensions, not to make ordinary file
browsing faster.

## Non-negotiable architecture

```text
Shared Ferail UI and domain model
        |
        +-- filesystem location ------------------------------+
        |   current NativeFs streaming/virtualized fast path  |
        |   no COM, PIDL, or Shell work per row                |
        |                                                     |
        +-- platform capability interfaces                    |
            |                                                 |
            +-- macOS implementation                          |
            +-- Linux implementation                          |
            +-- Windows implementation                        |
                 |                                            |
                 +-- filesystem Shell actions                 |
                 +-- namespace provider for virtual places    |
                 +-- isolated helper process(es)              |
                      preview handlers / native context menu  |
```

### Shared types, platform-owned mechanics

The shared layer gains a location value with two forms in intent (exact naming
is left to implementation):

```rust
enum LocationTarget {
    FileSystem(PathBuf),
    Platform(PlatformLocationKey),
}
```

`PlatformLocationKey` is an opaque, serializable key. On Windows it can carry
a parsing name and arena-owned PIDL bytes. COM interfaces, raw PIDL pointers,
`HWND`, and Windows crate types never enter `ferail-core` or the row renderer.
PIDLs are stored only for namespace surfaces which require them—not on every
ordinary filesystem row and never in the millions-of-rows Flat View payload.

Use small capability-oriented seams rather than one giant operating-system
interface:

- location enumeration and parent/child identity
- open, reveal, properties, and default invocation
- clipboard and drag data exchange
- icons/thumbnails and preview
- optional native context menu

The existing `platform_shell` façade remains suitable for stateless calls.
Stateful namespace and broker services live behind Windows-only
implementations. Shared UI asks for capabilities and renders or hides an
action; `cfg(windows)` must not spread through view code.

All platform calls exchange owned, `Send`-safe request/result DTOs. Rendering
reads cached results only. Shell work never occurs while painting, hit-testing,
scrolling, selecting, or building the normal Ferail context menu.

### Isolation boundary

Windows Shell extensions are third-party native code. The PDF stack already
demonstrates why a worker thread is not a crash boundary: an access violation
in an in-process DLL still terminates Ferail.

Ship a Windows-only helper executable with role-specific invocations (or one
binary with separate modes):

- preview role: hosts `IPreviewHandler`, returns bounded RGBA through IPC, and
  can be killed on a deadline
- context-menu role: owns the hidden/native window, `IContextMenu2/3`, `HMENU`,
  modal message forwarding, and command invocation

The roles must not share a process lifetime: a wedged PDF handler must not
prevent the Windows menu from appearing. Helpers run at the user's normal
integrity level, never elevate implicitly, restart after failure, and idle-exit.
No helper starts with Ferail or during ordinary navigation.

### Performance and privacy gates

Every phase preserves these invariants:

- no whole-folder content sniff, thumbnail generation, tag/property lookup, or
  Shell enumeration merely because a folder became visible
- bounded viewport work, bounded concurrency, cancellation/generation checks,
  and bounded result application per frame
- completion of an off-screen batch does not refresh rows which cannot be seen
- ordinary right-click performs no filesystem or Windows Shell query
- no personal path, thumbnail, preview pixel, or file property is uploaded;
  helper IPC is local and preview pixels are memory-only
- default reports redact personal paths; a full diagnostic report is an
  explicit tester opt-in
- navigation, scroll, selection, and the normal context-menu latency may not
  regress by more than 5% from the Windows baseline captured in Phase 0
- the deterministic 4,194,304-file Flat View test remains uncapped,
  responsive, symbolically selectable, and within 10% of its pre-change
  Windows time/memory baselines

## Issue ledger

### WIN-001 — reproducible Windows evidence and useful crash reports (P0)

**Observation.** The hang report has no thread stacks, breadcrumbs, activity,
or registered task despite a ten-second heartbeat stall. The four “crash”
reports are shutdown assertions for leaked GPUI entity handles, not stacks for
the preceding user-visible failure. Public release artifacts do not give the
tester usable symbols.

**Assessment.** We cannot safely group the preview and multi-selection crashes
under one cause. The PDF access violation is strong evidence; the other reports
are evidence of cleanup defects and missing diagnostics, not proof of the
trigger.

**Work.**

- [ ] Build Debug and Release-with-diagnostics on Windows; retain matching PDBs
  as CI/release-workflow artifacts even if they are not shipped publicly.
- [ ] Enable minidump capture for unhandled native exceptions and document the
  Task Manager full-dump fallback.
- [ ] Add activity breadcrumbs around navigation, selection transitions,
  thumbnail/preview request generations, Shell calls, and table refreshes.
- [ ] Make task snapshots include viewport preview and enrichment schedulers.
- [ ] Reproduce shutdown after zero, one, and multiple windows; use
  `LEAK_BACKTRACE=1` to locate every leaked `InputState` and `TableState`
  handle, then fix ownership rather than suppressing GPUI's assertion.
- [ ] Keep redacted reports useful: row counts, extensions, provider CLSIDs,
  durations, generations, and HRESULTs are allowed; full paths are opt-in.

**Exit gate.** A forced UI stall produces a symbolized UI stack and recent
activity; a forced broker crash identifies the provider and leaves Ferail
alive; 100 open/close cycles end without an entity-handle assertion.

### WIN-002 — PDF/third-party preview-handler crash containment (P0)

**Observation.** WinDbg reports `c0000005` in an unloaded
`pdfprevhndlr.dll`. Current `preview_handler.rs` loads in-process handlers on a
fresh STA thread. Its six-second receive timeout drops the `JoinHandle` but
cannot stop a hung thread or unload unsafe native code from Ferail's address
space.

**Root cause.** A thread provides scheduling isolation, not memory-safety or
termination isolation. A handler may keep callbacks or worker state past
`Unload`, race DLL unload, hang forever, or corrupt the host.

**Work.**

- [ ] Move `IPreviewHandler` activation, hosting, message pumping, capture, and
  unload into the preview broker.
- [ ] Use a small bounded broker pool or one disposable process per provider;
  never one unbounded thread per row/request.
- [ ] Cancel superseded generations. Deadline expiry kills and replaces the
  broker; it does not leave a detached thread behind.
- [ ] Maintain a session quarantine for a CLSID that crashes or times out
  repeatedly, falling back to `IShellItemImageFactory`, the built-in decoder,
  or a generic icon.
- [ ] Validate RGBA dimensions and byte length before accepting broker IPC.

**Exit gate.** Injected crash, access violation, malformed bitmap, and hung
handler fixtures cannot terminate or freeze Ferail; the selected row receives
a fallback within the declared deadline.

### WIN-003 — 10,000-image preview stability and scrollbar integrity (P0)

**Observation.** In a roughly 10,000-image folder, previews reset while
scrolling, the scrollbar can disappear, and the app may crash. A changing
thumbnail must never change the row count or reset table geometry.

**Likely failure class.** Too much warming, stale completions crossing
navigation/scroll generations, large all-at-once apply/refresh passes, or an
asset completion invalidating the whole table. The exact trigger remains to be
captured on Windows before changing code.

**Work.**

- [ ] Record request, decode, upload, apply, and cancellation counters by
  generation while reproducing the supplied video scenario.
- [ ] Limit requests to the viewport plus a small directional overscan.
- [ ] Use fixed worker concurrency and a bounded queue; newest visible work
  wins and old off-screen work is discarded.
- [ ] Bound GPU uploads and table updates per frame.
- [ ] Refresh only affected visible rows; never rebuild/sort the row source for
  a thumbnail completion.
- [ ] Keep scrollbar extent derived solely from stable row geometry.
- [ ] Put process cache limits on pixel bytes and Windows handles, then verify
  eviction while repeatedly scrolling end-to-end.

**Exit gate.** A 10,000-image local fixture and a slower network/OneDrive
fixture survive ten end-to-end scroll passes with stable scrollbar geometry,
bounded memory/handles, no stale-image flashes, no >250 ms UI heartbeat gap,
and no crash.

### WIN-004 — multi-selection crash and large-selection behavior (P0)

**Observation.** The tester reports crashes around multi-selection. The
available files do not contain the triggering stack; some captured reports are
only shutdown leak assertions.

**Work.**

- [ ] Reproduce Shift ranges, Ctrl toggles, right-click on selected and
  unselected rows, Select All, navigation with a live selection, and actions on
  1/100/10,000 entries.
- [ ] Assert that menu construction and painting use symbolic or bounded
  selection summaries rather than cloning every selected path.
- [ ] Audit all selection snapshots that cross worker or Shell boundaries for
  stale row indices; use stable ids/generations.
- [ ] Add regression tests for selected-row removal and refresh while a menu,
  preview, drag, or clipboard operation is in flight.

**Exit gate.** The matrix is clean under Debug assertions and Release, and
Select All does not allocate or resolve one path per selected row until an
explicit operation truly needs those paths.

### WIN-005 — background file-detail scan consumes the machine (P0)

**Observation.** `Detect file types and tags` defaults on. A normal folder load
currently snapshots every row and starts whole-list prefetch; the tester's log
shows 10,821 rows taking about 16 seconds. Applying the returned batch is
followed by a heartbeat stall. Turning the setting off hides useful Format and
Description data, so default-off alone trades away functionality rather than
fixing the architecture.

**Work.**

- [ ] Replace ordinary-folder whole-list prefetch with the viewport-scoped
  model already used by Flat View: visible rows plus bounded overscan, cache
  first, cancel stale work.
- [ ] Apply results by stable id only to affected rows and coalesce repaint
  notifications.
- [ ] Separate **show file details** from an optional **index the entire
  folder while idle** policy. Details remain available while scrolling; eager
  full-folder indexing defaults off.
- [ ] Give thumbnails, descriptions, tags, magic sniffing, and folder sizes a
  shared background-I/O budget so they do not all saturate a cold disk.
- [ ] Pause speculative/idle work during active scrolling, input, battery
  saver, or sustained foreground load.

**Exit gate.** Opening a 10,000-entry directory schedules work proportional to
the viewport, not 10,000 files; Format and Description populate as rows enter
view; idle CPU and I/O settle without disabling the feature.

### WIN-006 — CPU and `rps` figures are alarming or ambiguous (P0)

**Observation.** Ferail showed CPU 37% while Task Manager showed 2.8% in the
same screenshot, and users have seen values around 700%. Current CPU is
expressed in “percent of one logical core”, while Task Manager normalizes a
process across the whole machine. `rps` means redraws per second, but is not
self-explanatory.

**Work.**

- [ ] Normalize the user-facing CPU number by logical processor count so it
  matches Task Manager semantics; retain core-equivalent CPU in diagnostics.
- [ ] Rename or explain redraw rate in a tooltip/localized label; do not imply
  filesystem requests or network traffic.
- [ ] Verify the sampler itself does not cause redraws and that an idle window
  does not repaint merely to keep the number animated.
- [ ] Add a diagnostic detail view for raw process time, core-equivalent CPU,
  normalized CPU, redraw rate, and active task counts.

**Exit gate.** A controlled single-core load and idle test agree with Task
Manager within a documented sampling tolerance; values above 100% cannot
appear in the normal user-facing CPU field.

### WIN-007 — native Windows context menu without the old prefetch (P1, design confirmed)

**Observation.** Ferail's fast menu omits third-party Shell verbs. The old
Ferail-Win32 implementation made those verbs available but paid for
`QueryContextMenu` around selection time and carried a large STA pump/preload
system.

**Work.**

- [ ] Keep the existing GPUI/Ferail menu unchanged for ordinary right-click.
- [ ] Add **More options from Windows…** at its end when the platform
  capability is available.
- [ ] Route `Shift`+right-click and `Shift+F10` directly to the native menu.
  Do not use `Ctrl`+right-click because Ctrl already changes multi-selection.
- [ ] Resolve the target snapshot only after explicit invocation; do not
  prewarm on selection, hover, navigation, or menu build.
- [ ] Have the context-menu broker bind parent/child PIDLs, obtain
  `IContextMenu`, forward `IContextMenu2/3` owner-draw/submenu messages, call
  `TrackPopupMenuEx`, and invoke the selected verb.
- [ ] Let the native menu render itself. Do not flatten it into GPUI entries;
  that breaks owner-draw handlers and dynamic submenus.
- [ ] Support same-parent multi-selection. For selections the Shell cannot
  represent (mixed parents or namespace providers), keep the Ferail menu and
  explain why the Windows action is unavailable.
- [ ] Once the native popup is visible it is user-modal, not timed out. Before
  display, a wedged provider can be abandoned by terminating the broker.
- [ ] After a verb, refresh only possibly affected locations while preserving
  selection/scroll when their targets still exist.

**Exit gate.** Normal right-click latency and navigation benchmarks are
unchanged; 7-Zip/Defender/Git-style test extensions appear only on explicit
request; a crashing or blocked handler cannot stop Ferail.

### WIN-008 — default open/double-click uses the wrong verb or path (P0)

**Observation.** Double-clicking `ski.jpg` sometimes opened Print Pictures and
also produced an association error for `\\?\C:\files\ski.jpg`. The shared
Windows launcher currently uses `cmd /C start` on the path handed to it.

**Root cause.** `cmd start` adds command-line parsing and quoting semantics,
and the Windows Shell rejects or misinterprets verbatim `\\?\` paths at many
boundaries. Default invocation should be a Shell operation, not a command
interpreter operation.

**Work.**

- [~] Move Windows default open to `ShellExecuteExW`/Shell Item invocation with
  the explicit `open`/default verb and a shell-safe path.
- [~] Centralize the lossless conversion of drive and UNC verbatim paths at
  every outward Shell boundary; keep verbatim paths internally for filesystem
  correctness.
- [ ] Treat directories, files, shortcuts, URLs, and namespace items as
  distinct invocation targets.
- [ ] Return actionable HRESULT/association errors to Ferail rather than
  reporting only that a launcher process started.

**Exit gate.** Default associations open correctly for spaces, `#`, `%`, `!`,
Unicode, drive, UNC, and long paths; double-click never selects a non-default
verb such as Print.

### WIN-009 — Reveal in Explorer fails on unusual paths (P0)

**Observation.** Revealing a folder named `paths#é #!` opened Documents rather
than the actual parent. The current implementation launches
`explorer /select,<string>` after stripping the verbatim prefix.

**Root cause.** Explorer command-line parsing is not an identity-preserving
API and is fragile around quoting, namespace items, and some path forms.

**Work.**

- [~] Replace the command line with `SHOpenFolderAndSelectItems` using Shell
  Items/PIDLs, following the working Filociraptor pattern.
- [~] Reveal directories by opening/selecting according to Explorer semantics;
  reveal files through parent + child identity.
- [~] Support drive, UNC, special-character, long, and namespace targets.
- [ ] Fall back to opening the closest valid parent and explain when an item no
  longer exists.

**Exit gate.** The supplied path plus a path matrix opens the correct Explorer
window with the intended item selected.

### WIN-010 — `.lnk` behavior, target metadata, and thumbnail identity (P1)

**Observation.** A shortcut is shown as a generic file in Ferail while Explorer
shows the target's image with a shortcut overlay. Open and preview behavior is
inconsistent.

**Work.**

- [ ] Resolve `IShellLink` off-thread into a cached DTO: target kind/path or
  Shell identity, arguments, working directory, icon location, and broken
  state.
- [ ] On Open, navigate inside Ferail only when the resolved target is a real
  folder; otherwise invoke the shortcut through the Shell so arguments and
  provider behavior are preserved.
- [ ] Copy, move, rename, and trash continue to act on the `.lnk` itself, not
  its target.
- [ ] Request the Shell-provided icon/thumbnail and preserve the shortcut
  overlay; fall back to the target type then a generic shortcut glyph.
- [ ] Show shortcut target/broken status in Get Info without resolving it from
  render or context-menu code.

**Exit gate.** File, folder, app, argument-bearing, relative, UNC, and broken
shortcuts match Explorer's open semantics and remain responsive.

### WIN-011 — icons and thumbnails disagree with Explorer (P1)

**Observation.** `C:\Windows\Fonts` showed several red/error or generic icons
where Explorer displayed Windows-managed font representations. Special Shell
folders and providers may not behave like ordinary extension-based files.

**Work.**

- [ ] Separate icon requests (type/identity) from content-thumbnail requests;
  do not treat “no thumbnail” as a broken icon.
- [ ] Audit `IShellItemImageFactory` flags and the `SHGetFileInfo` fallback for
  `.fon`, `.ttf`, special folders, offline files, and shortcuts.
- [ ] Cache type icons by stable type/provider key and content thumbnails by
  file identity/mtime/size; never cache a transient provider failure forever.
- [ ] Run all provider calls through the bounded Windows asset scheduler and
  isolate unsafe provider fallbacks.
- [ ] Compare the Fonts fixture and a representative type corpus visually
  against Explorer at multiple DPI scales.

**Exit gate.** No error glyph is used merely because a thumbnail provider
declines; visible icons remain stable while scrolling and match the correct
Windows type/provider where available.

### WIN-012 — Explorer clipboard and drag/drop interop is incomplete (P0/P1)

**Observation.** Copy/paste from Explorer to Ferail does not work in every
case. `CF_HDROP` support already ships, so the remaining failures need to be
classified rather than reimplementing the happy path.

**Work.**

- [ ] Build a matrix for clipboard copy/cut and drag/drop in both directions:
  local paths, UNC, Unicode/special characters, multiple parents, OneDrive
  placeholders, `.lnk`, and Shell-only items.
- [ ] Verify `CF_HDROP`, `Preferred DropEffect`, and lifetime/ownership rules
  for normal files; distinguish clipboard from OLE drag failures in logs.
- [ ] Accept Shell ID-list/data-object formats for virtual or delayed-rendered
  items where a real path is unavailable; stream/materialize only after an
  explicit drop/paste.
- [ ] Keep all COM data extraction off paint/input callbacks and expose progress
  and cancellation for delayed transfers.
- [ ] Reject unsupported virtual transfers with a precise message rather than
  silently doing nothing.

**Exit gate.** The matrix passes on Explorer and at least one third-party file
manager; cut state, multi-file order, and collision handling remain correct.

### WIN-013 — Desktop namespace, This PC, devices, OneDrive, and Recycle Bin (P1)

**Observation.** Ferail's Desktop is the filesystem folder, not the Shell
namespace root. Therefore This PC, portable phones, provider roots, and the
Recycle Bin cannot be browsed naturally. OneDrive is reachable only when its
filesystem path is known.

**Work.**

- [ ] Introduce `LocationTarget`/platform location identity without changing
  ordinary `PathBuf` tabs or Flat View rows.
- [ ] Implement a Windows namespace provider with streaming batches,
  cancellation, generation checks, and PIDL arena ownership scoped to the tab.
- [ ] Add distinctly named Locations: **Desktop folder** for the filesystem
  path and **This PC** (or **Windows Desktop**) for the namespace root.
- [ ] Surface Recycle Bin, OneDrive/provider roots, and connected portable
  devices only when enumerated by the Shell.
- [ ] Route open, parent, breadcrumb, refresh, icon, properties, context menu,
  clipboard, and drag through capabilities when no path exists.
- [ ] Keep direct filesystem enumeration for every namespace item which
  resolves to a normal directory. Do not recursively enumerate a disk through
  COM.
- [ ] Treat removable/disconnected provider identity as ephemeral and show a
  recoverable unavailable state rather than retaining stale raw pointers.

**Exit gate.** Local folders benchmark identically to the baseline; This PC,
Recycle Bin, OneDrive, an MTP phone, and a disconnected-device case navigate
without pretending they are filesystem paths.

### WIN-014 — metadata and Windows Properties (P1)

**Observation.** Get Info lacks useful image metadata such as EXIF, while a
Windows user also expects access to the native Properties surface.

**Work.**

- [ ] Add portable metadata parsers in `ferail-fs-native` for fields with
  cross-platform meaning (dimensions, camera/lens, exposure, capture time,
  orientation, GPS-presence with an explicit privacy treatment). Parse on
  demand off-thread and cache by file identity/revision.
- [ ] Add a Windows property provider for useful `IPropertyStore` fields that
  are not available from portable parsing, returning neutral key/value DTOs.
- [ ] Add **Windows Properties…** through the native Shell action capability;
  do not try to recreate every installed property page inside Ferail.
- [ ] Never log metadata values or paths by default. GPS coordinates require a
  deliberate reveal and are not persisted unless the metadata cache policy is
  explicitly approved.

**Exit gate.** JPEG/TIFF/HEIC fixtures show correct portable metadata, malformed
files fail safely, and Windows Properties opens for filesystem and supported
namespace items without blocking the UI.

### WIN-015 — VC++ runtime and genuinely portable release artifacts (P0)

**Observation.** The downloadable executable can fail before Ferail starts
because `VCRUNTIME140.dll` is absent. An application that cannot launch cannot
offer to install its own prerequisite.

**Work.**

- [ ] Test Rust/MSVC static CRT linking for every native dependency in the
  release build. Prefer a self-contained portable ZIP if compatibility and
  security updates remain acceptable.
- [ ] If static CRT is not viable, ship an installer/bootstrapper that checks
  and installs the official Microsoft redistributable before launching Ferail;
  do not download it from inside the main executable.
- [ ] Test the exact ZIP/installer in a fresh Windows Sandbox with no developer
  tools or preinstalled redistributable.
- [ ] Add dependency inspection to packaging CI and fail if an undeclared DLL
  appears.
- [ ] Keep architecture-specific artifacts explicit (`x86_64`, later ARM64)
  and include matching helper binaries and symbols.

**Exit gate.** A pristine supported Windows VM launches, previews a safe image,
opens a file, and exits cleanly without installing Visual Studio tooling.

### WIN-016 — stale command/keymap and artifact consistency (P0)

**Observation.** The 0.6.5 log says `view.toggle_flat` is an unknown command and
skips `secondary-shift-l`, although the current source catalogue contains the
command. This suggests a release/configuration mismatch or initialization
ordering problem.

**Work.**

- [ ] Reproduce with a clean and an upgraded profile; identify whether the
  command catalogue, keymap, or packaged resource versions disagree.
- [ ] Add a startup test that every bundled binding resolves on every target.
- [ ] Version user keymap migrations and distinguish an obsolete user binding
  from a broken built-in binding in diagnostics.
- [ ] Stamp the executable, helper, resources, and report header from one build
  version/commit.

**Exit gate.** A clean 0.6.5→next-version upgrade has no unknown bundled
commands, and mismatched packaged resources fail CI.

## Execution order

### Phase 0 — Windows baseline and decision capture

- [ ] Set up the real Windows development/test machine with Rust, debugger,
  symbol retention, Windows SDK tools, clean test profile, and release build.
- [ ] Preserve the tester fixtures and create synthetic equivalents containing
  no personal data.
- [ ] Capture baseline timings: directory first paint, scroll frame time,
  normal context-menu open, idle CPU/redraws, memory/handles, 10k-image
  scrolling, and 10k-row detail enrichment.
- [ ] Reproduce each report item and attach it to its `WIN-xxx` entry.
- [ ] Confirm the now/later cut with the user before implementation expands
  beyond P0 containment and correctness.

### Phase 1 — make failures diagnosable and non-fatal

- [ ] WIN-001 diagnostics, PDBs, dumps, breadcrumbs, and leaked-handle cleanup.
- [ ] WIN-002 preview broker and provider quarantine.
- [ ] WIN-003 bounded preview scheduler and stable table updates.
- [ ] WIN-004 multi-selection reproduction and ownership fixes.
- [ ] WIN-015 portable-runtime packaging gate.

### Phase 2 — restore the Prime Directive under Windows load

- [ ] WIN-005 viewport-only details and shared I/O budget.
- [ ] WIN-006 Task Manager-compatible metrics and redraw audit.
- [ ] Re-run the baseline and reject any navigation/scroll regression.

### Phase 3 — filesystem interoperability

- [ ] WIN-008 Shell Item/default open.
- [ ] WIN-009 PIDL-based Reveal in Explorer.
- [ ] WIN-012 Explorer clipboard/drag matrix and missing formats.
- [ ] WIN-016 keymap/artifact consistency.

### Phase 4 — native compatibility on demand

- [ ] WIN-007 isolated native Windows context menu, with no prefetch.
- [ ] WIN-010 shortcut semantics.
- [ ] WIN-011 icon/thumbnail correctness.
- [ ] WIN-014 portable metadata plus Windows Properties.

### Phase 5 — Shell namespace locations

- [ ] WIN-013 platform location model and namespace provider.
- [ ] Ship This PC, Recycle Bin, OneDrive/provider roots, and devices
  incrementally behind capability checks.
- [ ] Prove again that ordinary filesystem browsing never enters the namespace
  scanner.

### Phase 6 — release qualification

- [ ] Execute every required case in the
  [Windows Reliability Test Plan](../testing/WINDOWS_RELIABILITY_TEST_PLAN.md),
  including the one-million and 4,194,304-row scale gates.
- [ ] Run unit/contract tests for shared platform interfaces on all targets.
- [ ] Run the complete interactive Windows fixture matrix at 100%, 150%, and
  200% DPI, with local SSD, slow/removable disk, UNC, OneDrive, and no-network
  cases.
- [ ] Soak preview, navigation, selection, and native-menu broker restart loops;
  verify stable memory, GDI/USER handles, thread count, and helper cleanup.
- [ ] Run macOS and Linux regressions for normal menus, open/reveal, previews,
  clipboard, and large Flat View.
- [ ] Update localized strings, screenshots/tour, README, user documentation,
  and `CHANGELOG.md` only alongside the implementation that users can run.
- [ ] Package the exact tested binaries, preserve matching symbols, and record
  the tested Windows builds in the release notes.

## Definition of done

The campaign is complete only when all P0/P1 entries above are `[x]` on a real
Windows machine and:

1. no third-party Shell extension can crash or indefinitely block Ferail;
2. local filesystem browsing retains today's direct, virtualized fast path;
3. useful Format/Description/preview behavior remains available without
   whole-folder eager work;
4. Windows-native operations are optional capabilities, not forks in the UI;
5. the normal context menu remains instant and performs no Shell prefetch;
6. Explorer interoperability covers real files and reports unsupported virtual
   transfers honestly;
7. Shell namespace locations do not contaminate the compact row model; and
8. the public artifact launches on a clean Windows installation and produces
   actionable, privacy-conscious diagnostics if anything still fails.
