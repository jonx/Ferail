# Windows Reliability and Compatibility Plan

← [Windows port notes](windows-port.md) ·
[Windows handover](../testing/WINDOWS_HANDOVER.md) ·
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
- Windows exposes installed WSL distributions as a dynamic **Linux** location.
  Merely displaying that location never starts a distribution; selecting a
  stopped distribution may start it explicitly, then hands its UNC root back
  to the normal filesystem fast path.
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
- ShellBat: `7b8268d60648b74f5a874be8edde3b4fc17f7cdc`
  ([smourier/ShellBat](https://github.com/smourier/ShellBat))

The useful Filociraptor lesson is its split path: `ShellLocation` holds a
normal path when one exists and a PIDL only for virtual Shell locations;
`NamespaceScanner` says explicitly that Shell enumeration is about five times
slower and therefore never replaces direct directory enumeration. Its native
context menu is a useful reference for PIDL binding, `IContextMenu`, message
forwarding, `TrackPopupMenu`, invocation, and post-menu refresh. We will not
copy its in-process extension boundary.

ShellBat is the stronger semantic reference for pathless Shell items. It keeps
the desktop-absolute parsing identity/PIDL separate from the optional
`SIGDN_FILESYSPATH`, obtains parents from `IShellItem::GetParent`, enumerates
with `BHID_EnumItems`, and maps supported operations from `SFGAO_*` attributes.
Ferail adopts those semantics, but not ShellBat's list materialization or
one-item-at-a-time UI scheduling: enumeration remains cancellable, streamed
and bounded. Ferail rows carry only compact session-local ids; the tab-owned
provider arena owns PIDL bytes and any parsing names and releases them together.

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

`PlatformLocationKey` is an opaque, compact id scoped to one provider session.
On Windows it indexes a tab-owned arena containing owned PIDL bytes and, only
where needed, a desktop-absolute parsing name. The parsing name is not copied
into rows, persisted or emitted in default diagnostics. COM interfaces, raw
PIDL pointers, `HWND`, and Windows crate types never enter `ferail-core` or the
row renderer. PIDLs are stored only for namespace surfaces which require
them—not on every ordinary filesystem row and never in the millions-of-rows
Flat View payload.

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

- [x] Build Debug and Release-with-diagnostics on Windows; retain matching PDBs
  as CI/release-workflow artifacts even if they are not shipped publicly.
  *2026-08-24: `win.yml` uploads and releases
  `Ferail-<version>-x64-symbols.zip` (PDBs + CodeView/commit manifest)
  alongside the app ZIP. Renamed from `…-win-x64-symbols.zip` because
  updaters up to 0.6.6 matched it as the Windows download.*
- [x] Enable minidump capture for unhandled native exceptions and document the
  Task Manager full-dump fallback. *2026-08-24:
  `ferail_shell_win32::install_crash_dump_handler` — a top-level exception
  filter writes `reports/ferail-<role>-<pid>.dmp` (threads, modules,
  unloaded modules, handles, exception) and appends the exception code and
  address to `ferail-crash-<pid>.txt`; installed in the GUI (OS handling
  continues) and the preview broker (quiet). Verified with a real
  `0xC0000005` via `FERAIL_PREVIEW_BROKER_TEST=av`. Fallback documented in
  the handover (§ Collecting dumps).*
- [x] Add activity breadcrumbs around navigation, selection transitions,
  thumbnail/preview request generations, Shell calls, and table refreshes.
  *2026-08-24: the crash/watchdog ring now records path-free navigation
  generations, selection counts/modes, preview enqueue/cancel/complete state,
  and thumbnail batch start/supersede/complete state. The real leaked-handle
  report that previously said `breadcrumbs: <none>` now names the last
  navigation and shutdown cleanup.*
- [~] Make task snapshots include viewport preview and enrichment schedulers.
  Selection-driven native previews now register as ambient thumbnail tasks;
  confirm the resulting watchdog snapshot on Windows.
- [~] Reproduce shutdown after zero, one, and multiple windows; use
  `LEAK_BACKTRACE=1` to locate every leaked `InputState` and `TableState`
  handle, then fix ownership rather than suppressing GPUI's assertion.
  *2026-08-24: reproduced on Windows 11 (right-click + Esc + quit → leaked
  `PopupMenu`; a user session also leaked an `InputState` twice). Root
  cause was one bug class: subscription closures capturing a strong handle
  to the entity they subscribe to (context-menu `Rc<SharedState>`, the
  filter/breadcrumb/shortcuts-help inputs) — an App→listener→entity cycle.
  Fixed by weak/parameter access; the scripted repro now exits 0. Second
  find, deterministic (`--screenshot --properties`): gpui-component's
  `Input` paint captures strong `InputState` handles per frame, which
  accumulate whenever a second window (Get Info) is open — upstream, see
  GPUI-UPSTREAM.md §12. The assert itself no longer ships: packaged builds
  strip gpui's `test-support`/`leak-detection` (dev + tests keep it), so
  users cannot hit the exit-101 report at all. A dev-only teardown guard now
  drains gpui-component's retained next-frame callbacks while each Root is
  alive and removes screenshot windows before `App::quit`: the deterministic
  `--screenshot --properties` repro went from 76 retained callbacks + one
  final `InputState` (exit 101) to exit 0. The app-level Quit action now uses
  the same bounded cleanup and event-loop turn instead of bypassing native
  window-close callbacks. Still open: the full interactive multi-window
  matrix and a `TableState` leak that has not been reproduced.*
- [~] Keep redacted reports useful: row counts, extensions, provider CLSIDs,
  durations, generations, and HRESULTs are allowed; full paths are opt-in.
  *Provider CLSID + failure kind (crash/timeout/malformed frame) are logged
  on quarantine; the minidump sidecar carries exception code and address,
  never a path. Navigation/selection/preview/thumbnail generations are now
  breadcrumbed; aggregate timing counters remain open.*

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

- [x] Move `IPreviewHandler` activation, hosting, message pumping, capture, and
  unload into the preview broker. *2026-08-24: `--preview-broker` worker mode
  of the Ferail binary itself (no extra packaged helper, version-matched by
  construction); the parent never activates a handler in-process.*
- [x] Use a small bounded broker pool or one disposable process per provider;
  never one unbounded thread per row/request. *One disposable process per
  request, bounded by the preview scheduler's one-active + latest-wins
  waiting slots.*
- [x] Stop reaching for preview handlers at all where Explorer doesn't.
  *2026-08-24: grid/list/viewer thumbnails no longer fall back to the
  `IPreviewHandler` capture (it screenshots the handler's live viewer,
  scrollbars and all); only the preview pane's `fetch_preview_image` does.
  PDFs — the crashing case — now render page 1 through `Windows.Data.Pdf`
  (`pdf_render.rs`): no window, no third-party DLL, no broker. Inside the
  broker the handler is activated in-proc first, deliberately: the broker is
  already disposable, so the parent owns and can terminate the provider.
  `CLSCTX_LOCAL_SERVER` remains a compatibility fallback only; making an
  SCM-owned `prevhost.exe` the primary boundary would make its lifetime
  impossible for Ferail to enforce.*
- [x] Cancel superseded generations. Deadline expiry kills and replaces the
  broker; it does not leave a detached thread behind. *2026-08-24: the active
  selection owns an atomic cancellation token; queuing a newer path flips it,
  the broker wait kills/waits the child, and cancellation does not poison the
  old path's negative cache. Unit-tested queue transition; deadline kill was
  already verified on Windows.*
- [x] Maintain a session quarantine for a CLSID that crashes or times out
  repeatedly, falling back to `IShellItemImageFactory`, the built-in decoder,
  or a generic icon. *One fatal crash/timeout/malformed frame quarantines the
  CLSID for the session, a success clears the count, and the transition is logged once with the CLSID
  (redaction-safe). Verified with the injected-crash hook: icon fallback,
  main process unaffected.*
- [x] Validate RGBA dimensions and byte length before accepting broker IPC.
  *`broker_proto::parse_frame`: magic, dimension ceiling, exact byte count,
  and exact requested size; unit-tested on every host, and the parent's pipe
  reader is capped just above the exact requested frame (not the global 4096
  ceiling), and broker stdout is made non-inheritable before provider code can
  spawn descendants.*

**Exit gate.** Injected crash, access violation, malformed bitmap, and hung
handler fixtures cannot terminate or freeze Ferail; the selected row receives
a fallback within the declared deadline. *2026-08-24: injected crash
(`FERAIL_PREVIEW_BROKER_TEST=crash`), hang (=`hang`, killed at the 6 s
deadline), and malformed-frame validation all verified on a real Windows 11
machine via the CLI chain; Edge's real `PdfPreviewHandler.dll` renders
through the broker end-to-end. Still pending: the same matrix inside the
interactive GUI session and a reproduction against the tester's
`pdfprevhndlr.dll`.*

### WIN-003 — 10,000-image preview stability and scrollbar integrity (P0)

**Observation.** In a roughly 10,000-image folder, previews reset while
scrolling, the scrollbar can disappear, and the app may crash. A changing
thumbnail must never change the row count or reset table geometry.

**Likely failure class.** Too much warming, stale completions crossing
navigation/scroll generations, large all-at-once apply/refresh passes, or an
asset completion invalidating the whole table. The exact trigger remains to be
captured on Windows before changing code.

**Work.**

- [~] Record request, decode, upload, apply, and cancellation counters by
  generation while reproducing the supplied video scenario. *Breadcrumbs now
  expose batch request counts, size, supersession and completion; persistent
  aggregate counters remain open.*
- [x] Limit requests to the viewport plus a small directional overscan.
  *List/grid warm only the reported viewport plus eight rows.*
- [x] Use fixed worker concurrency and a bounded queue; newest visible work
  wins and old off-screen work is discarded. *Each surface now owns exactly
  one sequential thumbnail batch plus one latest pending viewport. A newer
  viewport cancels between provider calls; unstarted `in_flight` reservations
  are released as retryable rather than negative-cached. The preview pane has
  the same one-active + latest-wins shape and kills its Windows broker on
  supersession. The 10k Windows stress matrix remains an exit-gate test, not
  an implementation gap.*
- [ ] Bound GPU uploads and table updates per frame.
- [ ] Refresh only affected visible rows; never rebuild/sort the row source for
  a thumbnail completion.
- [ ] Keep scrollbar extent derived solely from stable row geometry.
- [ ] Put process cache limits on pixel bytes and Windows handles, then verify
  eviction while repeatedly scrolling end-to-end.

**Exit gate.** A 10,000-image local fixture and a slower network/OneDrive
fixture survive ten end-to-end scroll passes with stable scrollbar geometry,
bounded memory/handles, no stale-image flashes, no >250 ms UI heartbeat gap,
and no crash. *2026-08-25: the user reports that the real 10,000-image flow now
works on Windows. Keep the adversarial provider, repeated-pass, cache/handle
measurement and multi-DPI subcases open until separately recorded.*

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
explicit operation truly needs those paths. *2026-08-25: the user reports that
multi-selection now works on Windows, including the flow that previously
failed. The stale-row mutation matrix and measured 1/100/10,000/symbolic-4M
regression tests remain implementation/qualification work.*

### WIN-005 — background file-detail scan consumes the machine (P0)

**Observation.** `Detect file types and tags` defaults on. A normal folder load
currently snapshots every row and starts whole-list prefetch; the tester's log
shows 10,821 rows taking about 16 seconds. Applying the returned batch is
followed by a heartbeat stall. Turning the setting off hides useful Format and
Description data, so default-off alone trades away functionality rather than
fixing the architecture.

**Work.**

- [x] Replace ordinary-folder whole-list prefetch with the viewport-scoped
  model already used by Flat View: visible rows plus bounded overscan, cache
  first, cancel stale work.
- [x] Apply results by stable id only to affected rows and coalesce repaint
  notifications.
- [~] Separate **show file details** from an optional **index the entire
  folder while idle** policy. Details remain available while scrolling; eager
  full-folder indexing has been removed from ordinary browsing; an explicit
  idle indexing policy does not exist yet.
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

- [x] Normalize the user-facing CPU number by logical processor count so it
  matches Task Manager semantics; retain core-equivalent CPU in diagnostics.
- [x] Rename or explain redraw rate in a tooltip/localized label; do not imply
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

- [~] Keep the existing GPUI/Ferail menu unchanged for ordinary right-click.
  *Implementation is isolated from the normal menu path; real-window latency
  and interaction validation remain.*
- [~] Add **More options from Windows…** at its end when the platform
  capability is available.
- [~] Route `Shift`+right-click and `Shift+F10` directly to the native menu.
  Do not use `Ctrl`+right-click because Ctrl already changes multi-selection.
- [x] Resolve the target snapshot only after explicit invocation; do not
  prewarm on selection, hover, navigation, or menu build.
- [x] Have the context-menu broker bind parent/child PIDLs, obtain
  `IContextMenu`, forward `IContextMenu2/3` owner-draw/submenu messages, call
  `TrackPopupMenuEx`, and invoke the selected verb.
- [x] Let the native menu render itself. Do not flatten it into GPUI entries;
  that breaks owner-draw handlers and dynamic submenus.
- [~] Support same-parent multi-selection. For selections the Shell cannot
  represent (mixed parents or namespace providers), keep the Ferail menu and
  explain why the Windows action is unavailable. *Same-parent filesystem
  selections work; mixed-parent selections return an explicit notification;
  namespace-only items remain future work.*
- [~] Resolve and show the native menu for both filesystem **files and
  directories**, using their actual Shell items. Extend the same explicit,
  capability-gated path to supported namespace files/containers; never infer
  menu support from row kind alone. *Filesystem items share the current broker
  path; the directory and namespace acceptance matrix remains to be recorded.*
- [x] Once the native popup is visible it is user-modal, not timed out. Before
  display, a wedged provider can be abandoned by terminating the broker.
- [ ] After a verb, refresh only possibly affected locations while preserving
  selection/scroll when their targets still exist.

**Implementation (2026-08-25).** `ferail-gpui.exe` has an early, GUI-free
`--windows-context-menu-broker` role. The UI resolves paths only after the
explicit action, launches that role without a console, and waits on a
background executor. The broker owns OLE, PIDLs, `IContextMenu2/3`, its hidden
owner window and native popup, then exits. A private readiness pipe bounds only
pre-popup provider enumeration (eight seconds); after readiness, the wait is
unbounded and user-modal. No COM object or provider code enters the GPUI
process. Unit tests and strict Windows clippy pass; the three UI entry paths and
third-party extension matrix still require the real-window manual gate.

**Exit gate.** Normal right-click latency and navigation benchmarks are
unchanged; 7-Zip/Defender/Git-style test extensions appear only on explicit
request; a crashing or blocked handler cannot stop Ferail. *2026-08-25: the
user reports that the native Windows menu works in normal use. The hostile
extension, 100-open handle soak and post-mutation targeted-refresh cases remain
open.*

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
verb such as Print. *2026-08-25: the user reports that default Open works on
Windows. Keep the exhaustive unusual-path and failure-message matrix open.*

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
window with the intended item selected. *2026-08-25: the user reports that
Reveal in Explorer works on Windows. Keep deleted-target messaging and the
full difficult-path matrix open.*

### WIN-010 — `.lnk` behavior, target metadata, and thumbnail identity (P1)

**Observation.** A shortcut is shown as a generic file in Ferail while Explorer
shows the target's image with a shortcut overlay. Open and preview behavior is
inconsistent.

**Work.**

- [~] Resolve `IShellLink` off-thread into a cached DTO: target kind/path or
  Shell identity, arguments, working directory, icon location, and broken
  state. *The shared `platform_shortcuts` contract now defines a cancellable
  resolver, neutral failure states, privacy-redacted owned DTOs and a bounded
  process-memory cache keyed by `NodeId` plus file revision. The Windows COM
  resolver and GPUI application remain.*
- [ ] On Open, navigate inside Ferail only when the resolved target is a real
  folder; otherwise invoke the shortcut through the Shell so arguments and
  provider behavior are preserved.
- [ ] Copy, move, rename, and trash continue to act on the `.lnk` itself, not
  its target.
- [~] Request the Shell-provided icon/thumbnail and preserve the shortcut
  overlay; fall back to the target type then a generic shortcut glyph.
  *2026-08-24: grid/list/sidebar icons for `.lnk` now composite the
  shell-reported overlay (the shortcut arrow) over the target icon —
  `ferail-fs-native::icons::win_shell::overlay_rgba`
  (`SHGetFileInfoW(SHGFI_OVERLAYINDEX)` → `SHGetImageList(SHIL_JUMBO)` →
  `IImageList::GetOverlayImage`/`GetIcon` → `DrawIconEx`). Overlay
  composition is `.lnk`-only on purpose: the gpui `IconCache` keys by
  extension, so per-file overlays (cloud state) would bleed across files.
  The content-thumbnail identity and broken-shortcut fallback remain
  open.*
- [ ] Show shortcut target/broken status in Get Info without resolving it from
  render or context-menu code.

**ShellBat reference.** Its `ShellN.Extensions/Utilities/Link.cs` correctly
loads the file through `IPersistFile`, then reads `IShellLinkW::GetIDList`,
`GetPath`, `GetArguments`, `GetWorkingDirectory` and `GetIconLocation`. Ferail
will follow those identity semantics on a cancellable worker, but will not
retain COM objects in shared/cache state or expose raw arguments/paths in
diagnostics.

**Exit gate.** File, folder, app, argument-bearing, relative, UNC, and broken
shortcuts match Explorer's open semantics and remain responsive.

### WIN-011 — icons and thumbnails disagree with Explorer (P1)

**Observation.** `C:\Windows\Fonts` showed several red/error or generic icons
where Explorer displayed Windows-managed font representations. Special Shell
folders and providers may not behave like ordinary extension-based files.

**Work.**

- [x] Separate icon requests (type/identity) from content-thumbnail requests;
  do not treat “no thumbnail” as a broken icon. *2026-08-24: the shell
  fetches (`fetch_quick_look_thumbnail` / `fetch_preview_image`) return
  content only; the type icon is a separate `fetch_type_icon` that
  `video_poster::fetch_content` asks for last, after the bundled raster /
  cover-art / poster tiers.*
- [~] Audit `IShellItemImageFactory` flags and the `SHGetFileInfo` fallback for
  `.fon`, `.ttf`, special folders, offline files, and shortcuts.
  *Diagnosed and largely fixed 2026-08-24 on real Windows:*
  - *[x] `C:\Windows\Fonts` is a shell **namespace junction**:
    `SHCreateItemFromParsingName` returns `E_INVALIDARG` for any
    file-system path under it (the same `arial.ttf` copied elsewhere
    parses fine), so both the thumbnail and the icon fetch failed there and
    the grid showed the blank placeholder. Fixed by retrying with a bind
    context carrying `STR_FILE_SYS_BIND_DATA` (`IFileSystemBindData`) to
    force simple file-system parsing — in
    `ferail-shell-win32::shell_item_image_factory` (thumbnails/previews)
    and its twin `ferail-fs-native::icons::win_shell` (icons).*
  - *[x] The font thumbnail provider (“Abg” cards for `.ttf`/`.otf`)
    returns its DIB **bottom-up** while image thumbnails arrive top-down —
    both with positive `biHeight`, so the sign is useless and the
    “THUMBNAILONLY = top-down” rule rendered font cards rotated 180°.
    Fixed with an extension-gated orientation exception
    (`ttf/otf/ttc/fon/pfb/pfm`) in `fetch_shell_image`.*
  - *Still open: offline-file (cloud placeholder) flags and special
    shell folders beyond Fonts.*
- [~] Cache type icons by stable type/provider key and content thumbnails by
  file identity/mtime/size; never cache a transient provider failure forever.
  *The shared asset key now contains only compact file/provider identity,
  revision and asset kind/size. Existing caches still need migration to that
  identity and an expiring transient-negative policy.*
- [~] Run all provider calls through the bounded Windows asset scheduler and
  isolate unsafe provider fallbacks. *A pure `BoundedAssetLane` now enforces
  fixed concurrency and pending capacity, deduplicates requests, prioritizes
  selected/visible work over overscan, favors the newest viewport, cancels old
  generations without releasing an active slot early, and rejects stale
  completion. Process-level GPUI integration and Windows broker/provider
  routing remain.*
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

- [~] Promote normal on-disk `external_drag_payload` files to a native OLE
  drag through `SHCreateDataObject`/`SHDoDragDrop`, with copy, move, link,
  modifiers, cancellation, and GPUI-to-Shell visual handoff. *Implemented
  2026-08-25 for local/real paths; the format/provider matrix below remains.*
- [ ] Build a matrix for clipboard copy/cut and drag/drop in both directions:
  local paths, UNC, Unicode/special characters, multiple parents, OneDrive
  placeholders, `.lnk`, and Shell-only items.
- [ ] Verify `CF_HDROP`, `Preferred DropEffect`, and lifetime/ownership rules
  for normal files; distinguish clipboard from OLE drag failures in logs.
- [~] Accept Shell ID-list/data-object formats for virtual or delayed-rendered
  items where a real path is unavailable; stream/materialize only after an
  explicit drop/paste. *The shared provider action contract now carries
  capability-gated Copy/Move/Link requests and an O(1) symbolic Select All
  snapshot without fabricating paths. The Windows `IDataObject`/OLE bridge,
  delayed rendering and materialization remain.*
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

- [~] Introduce `LocationTarget`/platform location identity without changing
  ordinary `PathBuf` tabs or Flat View rows. *The pure core contract and
  tab-owned GPUI surface exist on macOS with compact ids, capability/flag DTOs,
  bounded worker/UI back-pressure, generation rejection, virtualized rows,
  breadcrumbs, history, recoverable errors and O(1) Select All. The real
  Windows provider remains.*
- [ ] Implement a Windows namespace provider with streaming batches,
  cancellation, generation checks, and PIDL arena ownership scoped to the tab.
- [ ] Add distinctly named Locations: **Desktop folder** for the filesystem
  path and **This PC** (or **Windows Desktop**) for the namespace root.
- [ ] Surface Recycle Bin, OneDrive/provider roots, and connected portable
  devices only when enumerated by the Shell.
- [~] Route open, parent, breadcrumb, refresh, icon, properties, context menu,
  clipboard, and drag through capabilities when no path exists. Native context
  menus must cover both file and container items, and must be requested only
  after the explicit menu action. *A neutral `PlatformAction` maps each action
  to one advertised capability, independent of displayed row kind; providers
  default to explicit `Unsupported`. Action requests retain tab-owned location
  identity and a compact explicit-or-complement selection. The Windows action
  executor and UI affordances remain.*
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

- [~] Add portable metadata parsers in `ferail-fs-native` for fields with
  cross-platform meaning (dimensions, camera/lens, exposure, capture time,
  orientation, GPS-presence with an explicit privacy treatment). Parse on
  demand off-thread and cache by file identity/revision. *2026-08-24:
  `image_meta::read_image_meta` (header dimensions + kamadak-exif subset)
  feeds Get Info's Image section on every platform; GPS is presence-only —
  coordinates are never parsed, shown, logged, or persisted. A reusable
  bounded, path-free identity/revision cache now exists; wiring it into Get
  Info remains (Get Info parses one file per open today). `ImageMeta::Debug`
  exposes presence flags only, never EXIF strings.*
- [~] Add a Windows property provider for useful `IPropertyStore` fields that
  are not available from portable parsing, returning neutral key/value DTOs.
  *The shared grouped DTO, restricted scalar/list value set, cancellable
  provider seam and generic identity/revision cache are complete. All values,
  display names and paths are redacted from diagnostics. The approved Windows
  key mapping, `IPropertyStore` worker and Get Info merge remain.*
- [ ] Add **Windows Properties…** through the native Shell action capability;
  do not try to recreate every installed property page inside Ferail.
- [~] Never log metadata values or paths by default. GPS coordinates require a
  deliberate reveal and are not persisted unless the metadata cache policy is
  explicitly approved. *Shared EXIF/property DTOs and both revision caches are
  process-memory-only and debug-redacted; the Windows canary audit remains.*

**Exit gate.** JPEG/TIFF/HEIC fixtures show correct portable metadata, malformed
files fail safely, and Windows Properties opens for filesystem and supported
namespace items without blocking the UI.

### WIN-015 — VC++ runtime and genuinely portable release artifacts (P0)

**Observation.** The downloadable executable can fail before Ferail starts
because `VCRUNTIME140.dll` is absent. An application that cannot launch cannot
offer to install its own prerequisite.

**Work.**

- [x] Test Rust/MSVC static CRT linking for every native dependency in the
  release build. Prefer a self-contained portable ZIP if compatibility and
  security updates remain acceptable. *2026-08-24: `package-win.ps1` now
  builds the whole graph with `-C target-feature=+crt-static`; dumpbin shows
  no CRT/UCRT imports in either binary, and the packaged exe launches and
  renders headlessly on a real Windows 11 machine.*
- [x] If static CRT is not viable, ship an installer/bootstrapper that checks
  and installs the official Microsoft redistributable before launching Ferail;
  do not download it from inside the main executable. *n/a — static CRT is
  viable, so no bootstrapper is needed.*
- [x] Test the exact ZIP/installer in a fresh Windows Sandbox with no developer
  tools or preinstalled redistributable. *2026-08-25: the user reports that
  the v0.6.8 portable package launches and works in a clean Windows Sandbox.
  The released product is the portable ZIP; installer qualification remains
  separate if an installer becomes a published artifact.*
- [x] Add dependency inspection to packaging CI and fail if an undeclared DLL
  appears. *The gate lives in `package-win.ps1` (dumpbin against a Windows
  system-DLL allowlist; any CRT import fails the run), which `win.yml` CI
  executes for every release ZIP.*
- [~] Keep architecture-specific artifacts explicit (`x86_64`, later ARM64)
  and include matching helper binaries and symbols. *x86_64 ZIPs plus a
  `-symbols.zip` (PDBs + CodeView/commit manifest) ship now; helper binaries
  join it when they exist. Packaging now forces
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, records that policy in the
  symbol manifest, and refuses a dirty tree unless `-AllowDirty` is explicitly
  supplied for a local-only smoke package. The clean-Sandbox portable-ZIP
  launch now passes; Authenticode and any future installer remain open.*

**Exit gate.** A pristine supported Windows VM launches, previews a safe image,
opens a file, and exits cleanly without installing Visual Studio tooling.

### WIN-016 — stale command/keymap and artifact consistency (P0)

**Observation.** The 0.6.5 log says `view.toggle_flat` is an unknown command and
skips `secondary-shift-l`. The catalogue and action registration were present,
but the catalogue-to-GPUI dispatch match omitted that one action; this was a
source consistency bug, not an upgraded-profile or initialization issue.

**Work.**

- [x] Identify whether the command catalogue, keymap, or packaged resource
  versions disagree: the missing GPUI dispatch arm was the root cause.
- [ ] Re-run with both a clean and an upgraded Windows profile.
- [~] Add a startup test that every bundled binding resolves on every target.
  *The test now executes the real GPUI startup dispatcher for every
  translatable shortcut in the compiled platform catalogue; an omitted route
  fails with the command id and binding. It passes on macOS; Windows CI and the
  packaged-profile run remain.*
- [ ] Version user keymap migrations and distinguish an obsolete user binding
  from a broken built-in binding in diagnostics.
- [ ] Stamp the executable, helper, resources, and report header from one build
  version/commit.

**Exit gate.** A clean 0.6.5→next-version upgrade has no unknown bundled
commands, and mismatched packaged resources fail CI.

### WIN-017 — WSL distributions as path-backed Linux locations (P1)

**Observation.** The current application has no entry point for installed WSL
distributions, although Windows exposes their filesystems through
`\\wsl.localhost\<distribution>` and the legacy `\\wsl$\<distribution>`
aliases. The pinned Ferail-Win32 predecessor already solved the useful core:
it enumerated `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`, listed
stopped as well as running distributions, started one only after explicit
navigation, understood both UNC authorities, and delegated Linux symlink
resolution to `wsl.exe`.

The predecessor is a behavioral reference, not code to copy unchanged. Its
MSI-install-location probe misses modern inbox/Store configurations; some
registry/filesystem/process calls sit too near UI handling; `Command::output`
has no deadline or cancellation; and its `/mnt/<drive>` symlink handling is
unfinished. Copying its broad "skip magic on WSL" policy would also remove
useful Ferail functionality instead of scheduling it correctly.

**Product semantics.** Show installed distributions, including stopped ones,
under a cached dynamic **Linux** location. A stopped distribution is visibly
stopped and is never activated by startup, sidebar rendering, hover, tree
expansion, prefetch, or background enrichment. Clicking it is the explicit
permission to start it. Once its root is available, it is a real path-backed
location and uses `NativeFs`, the compact row representation, streaming
listing and the same viewport-bounded enrichment as an ordinary slow/remote
filesystem. It does not become a PIDL-backed Shell-namespace tab.

**Shared work, developed on macOS first.**

- [x] Add a narrow path-backed platform-root capability and neutral owned DTO:
  opaque id, display label, availability (`ready`, `stopped`, `starting`,
  `unavailable`) and optional filesystem target. It carries no registry key,
  distribution `BasePath`, process handle or Windows error type. *Implemented
  in `ferail-core::platform_locations`; the store is O(distributions) and its
  stale-discovery/activation transitions are host-tested.*
- [x] Render the dynamic Linux section from cached state only. Use a fake
  provider on macOS to test zero/many roots, stopped→starting→ready,
  cancellation, failure, refresh, tabs/history and disappearance without an
  OS query in layout or paint. *The section is absent when the provider is
  empty; fake-provider tests cover the primary state transition and refresh
  cancellation. Full tab/history/disappearance UI automation remains in the
  Windows matrix.*
- [x] Keep activation generation-safe and cancellable. Late completion after
  navigation, tab closure or refresh cannot replace the current location.
  *The process state rejects stale provider generations and the originating
  tab id/load generation gates navigation.*
- [~] Define filesystem capabilities for WSL-specific limits: hidden-name
  policy, symlink following, trash/delete support, native menu, Open/Reveal
  and terminal actions. Unsupported behavior must be explained rather than
  silently emulated with destructive semantics. *Dotfiles retain existing
  hidden semantics; symlink fallback and a fail-closed Recycle Bin gate ship
  in the working tree. Open/Reveal/menu/transfer behavior still needs the real
  Windows fixture matrix; WSL-terminal behavior is not part of this slice.*
- [~] Assert that the provider allocates O(distributions), adds nothing to
  ordinary rows, and leaves local browsing and Flat 1M/4M unchanged. *The
  storage boundary is implemented and `FileEntry` is unchanged; measured
  Flat 1M/4M acceptance remains Windows work.*

**Windows mechanism.**

- [~] Discover installed distributions off-thread from the per-user Lxss
  registry, with `wsl.exe --list --quiet` as a bounded fallback/validation for
  current WSL packaging. Do not depend on the old HKLM MSI install key. Cache
  the snapshot and expose explicit refresh; absence of WSL is an ordinary
  unavailable capability, not a startup warning. *Implemented and Windows
  cross-checked in `ferail-shell-win32`; real registry/Store/inbox-WSL
  acceptance remains.*
- [~] Prefer `\\wsl.localhost\<distribution>` as the generated path while
  accepting that form, `\\wsl$\<distribution>`, and their extended-UNC forms.
  Parse roots lexically with tests for Unicode/spaces and preserve path
  identity/case; never fabricate a Linux path by string concatenation outside
  the dedicated converter. *Pure parsing/output-decoding tests run on macOS;
  real UNC acceptance remains.*
- [~] On explicit activation only, run
  `wsl.exe -d <distribution> --exec /bin/true` on the bounded worker lane.
  Apply a deadline, cancellation, generation check and process cleanup, and
  show **Starting <distribution>…** plus an actionable error. Never use
  `cmd.exe`, `sh -c`, or an interpolated command string. *Implemented with a
  20-second deadline, cooperative cancellation and killed/waited child;
  process-tree behavior must be verified on Windows.*
- [~] First try normal UNC filesystem operations. If explicit navigation must
  follow a Linux symlink Windows cannot resolve, invoke
  `wsl.exe -d <distribution> --exec readlink -f -- <linux-path>` off-thread,
  validate the returned absolute path, detect broken/looping links, and map it
  back to the same UNC authority. Convert `/mnt/<drive>/...` to a Windows path
  only through a checked converter; cover a distro stopping mid-request.
  *Direct enumeration remains first. Explicit activation of a WSL symlink and
  failed WSL directory navigation both use the five-second `readlink -f --`
  resolver; the checked `/mnt/<drive>` converter is implemented. Loops, broken
  links and shutdown still require Windows execution.*
- [~] Treat WSL as a slow/remote I/O class rather than disabling features.
  Format/Description, thumbnails, preview and magic detection remain
  viewport/on-demand, budgeted and cancellable. If richer Linux permissions or
  symlink details are added, gather them in a bounded batch—never one
  `wsl.exe` process per row. *Viewport details/thumbnails remain enabled; the
  current all-directory recursive folder-size pass is deliberately skipped
  for WSL until it has a viewport/on-demand mode.*
- [~] Define deletion before enabling it. Do not claim Windows Recycle Bin
  recovery for a WSL item unless the exact operation proves recoverable; until
  a Linux trash provider exists, fail closed or use an explicit permanent
  deletion confirmation appropriate to the existing safety policy. *Move to
  Recycle Bin now fails closed for WSL with a localized explanation;
  Delete Immediately remains the separately confirmed permanent operation.*

**Privacy and diagnostics.** A registry `BasePath` reveals the host user and
VHD location and must never enter shared DTOs, persistence, telemetry, crash
reports or normal logs. Distribution names and browsed Linux paths are also
personal path components: UI may display them, but diagnostics use only a
request id, state, WSL version/capability and redacted path class. No thumbnail,
file content or command output is persisted by the provider.

**Exit gate.** On a real Windows machine, no-WSL, no-distro, WSL1/WSL2,
running/stopped, Unicode-name, symlink (`/bin`, broken, loop, `/mnt/c`) and
distro-shutdown cases all fail or recover responsively. A stopped distro starts
only after its row is activated; browsing then uses the normal filesystem
surface with previews/details intact. Process/handle counts settle, reports
contain no `BasePath` or raw personal path, and local NTFS plus Flat 4M remain
within the saved performance/memory gates.

## Continuation plan: Mac-first, Windows-final

The original P0 flows have now been exercised successfully by the user on a
real Windows machine, including 10k previews, multi-selection, Flat 4M,
Open/Reveal, the native menu and a clean Sandbox package. The next work starts
on macOS because that is the daily development environment, but Windows-only
mechanisms are never declared complete there.

### Delivery rule for every slice

Each remaining capability lands in this order:

1. Define the smallest platform-neutral request/result DTO and ownership
   contract. No COM interface, PIDL pointer, `HWND`, Windows error type or
   platform object crosses into `ferail-core` or a row model.
2. Implement shared scheduling, cancellation, generation checks, caching and
   UI states on macOS. Use the real macOS analogue where one exists and a
   deterministic fake/unsupported capability where it does not.
3. Prove the shared behavior on macOS: unit/contract tests, UI-thread guards,
   ordinary browsing, selection and Flat 1M/4M regression where the slice can
   touch row/model behavior.
4. Commit that coherent shared slice independently. Do not mix it with the
   Windows COM implementation.
5. On Windows, implement the platform mechanism behind the same seam, run the
   focused fixture matrix and record its evidence in the handover. Only then
   move the corresponding ledger and exact WTEST cases to `[x]`.

The existing `platform_shell` facade remains the right boundary for stateless
operations. Stateful services use small capability-oriented interfaces rather
than one giant OS abstraction: asset scheduling, link resolution, transfer
offers, platform locations and property gathering have different lifetimes and
failure modes.

### Scale contract carried by every phase

- Ordinary filesystem and Flat View rows remain platform-neutral and compact:
  no PIDL, COM object, provider state, owned duplicate `PathBuf`, thumbnail or
  metadata map per row.
- Select All remains a complement/range representation. An explicit operation
  may stream the selected paths in bounded chunks; menu construction, painting
  and drag preview never materialize millions of targets.
- Work discovery is O(viewport) during browsing. Process-wide budgets cap
  blocking I/O, decodes, Shell calls, GPU uploads and UI applies independently.
- Stateful Windows namespace identity belongs to the namespace surface/tab and
  drops with it. It never enters the global filesystem `NodeStore` or a Flat
  scan arena.
- After every shared row/scheduler change, rerun ordinary navigation plus Flat
  1M; after a phase, rerun Flat 4M and compare time, working set and scroll
  responsiveness with the saved baseline.

### Scope decisions before coding

- Do **not** reintroduce automatic whole-folder detail indexing. Viewport
  details plus cache-on-demand are the product behavior. A future explicit
  “index this folder while idle” command would be a separate opt-in feature,
  not a prerequisite for Windows reliability; `WTEST-024` is N/A until such a
  feature exists.
- Continue shipping the portable ZIP as the Windows product for now. Do not
  spend implementation time on an installer unless we decide to publish one;
  installer-only cases are then N/A, while clean-ZIP and Authenticode gates
  remain relevant.
- Keep the captured `IPreviewHandler` image as a last-resort preview. Hosting a
  live native preview child is a separate feature with HWND/z-order/focus
  complexity and is not required to close this campaign.
- Do not expose GPS coordinates. Presence-only is the approved privacy model;
  tests must verify that coordinates are never parsed, logged or persisted,
  not ask for a reveal control.
- Implement a curated useful Windows property subset, not Explorer's entire
  property system. The native Properties sheet remains the escape hatch for
  provider-specific pages.

### Phase A — make the current baseline reproducible

This phase changes no user behavior. It prevents us from building the next
features on an ambiguous baseline.

**On macOS**

- Restore `cargo fmt --all -- --check` and workspace strict Clippy without
  weakening lints or UI-thread guards.
- Add deterministic, streaming generators/verifiers for
  `WCORPUS-MEDIA-10K`, `WCORPUS-WIDE-100K`, `WCORPUS-FLAT-1M` and
  `WCORPUS-FLAT-4M`; keep generated data and personal media out of Git.
- Add a machine-readable acceptance-record template carrying commit, artifact
  hash, machine/profile, corpus manifest and before/peak/after measurements.
- Update stale Windows/GPUI TODO references as implementation moves.

**Finish on Windows**

- Regenerate/verify the same corpus manifests and attach a short v0.6.8
  baseline for CPU, memory, handles, first paint and Flat 4M.
- Record the exact Sandbox artifact hash and PDB manifest identity already
  accepted by the user.

**Exit:** a second machine can recreate every required corpus and compare a
future build with v0.6.8 without guessing what was tested.

### Phase B — finish bounded previews, details and selection reliability

This closes WIN-001, WIN-003, WIN-004, WIN-005 and WIN-006 before adding broad
new Windows surfaces.

**On macOS**

- Introduce one process-wide work coordinator with separate bounded lanes for
  filesystem detail reads, thumbnails/provider calls, decode, GPU upload and UI
  apply. Visible/selected work outranks overscan; active input pauses only
  speculative work, never data already needed on screen.
- Give GPU uploads and result application a per-frame budget. Apply by stable
  id/generation and invalidate only affected visible rows; row count and
  scrollbar extent remain derived from the immutable listing geometry.
- Give thumbnail/pixel/provider caches explicit byte and handle budgets.
  Cancellation releases reservations as retryable; transient failures have a
  bounded negative-cache lifetime.
- Audit selection handoffs to preview, menu, drag, clipboard and mutations.
  Snapshots use stable ids/generations or a streaming iterator, never stale row
  indices. Add regression tests for row removal/refresh during each handoff.
- Extend diagnostics with active lane counts, cancellations, raw/core-normalized
  CPU and redraw sampling without making the sampler cause repaint.

**Finish on Windows**

- Connect Windows thumbnail/PDF/preview-provider work to the shared budgets;
  retain the disposable preview broker as the crash boundary.
- Run the 10k repeated-scroll/provider failure matrix, the measured selection
  matrix through symbolic 4M, the idle-redraw check and helper/handle soaks.

**Exit:** visible Format/Description and previews remain available, all queues
and caches settle, scrollbar geometry never changes from asset completion, and
the original preview/selection crashes stay absent under repetition.

### Phase C — complete path-based Shell actions

This finishes WIN-007, WIN-008 and WIN-009 without introducing namespace
browsing yet.

**On macOS**

- Define a neutral `ShellActionOutcome`: cancelled, invoked, failed with an
  actionable category, plus a bounded set of filesystem locations that may
  need refresh. UI notifications and selection/scroll preservation consume
  this result without knowing the platform API.
- Make post-action refresh generation-safe and targeted. A watcher event may
  coalesce with it, but correctness must not depend on a provider emitting one.
- Add contract tests for no-association, vanished target, closest-existing
  parent, cancellation and a mutation completing after navigation changed.

**Finish on Windows**

- Have the native-menu broker report cancel/invoke and the canonical verb when
  available. After a mutating verb, refresh only the selected parent(s) and
  preserve surviving selection/scroll.
- Map `ShellExecuteExW` and PIDL Reveal failures into the neutral actionable
  errors; complete the drive/UNC/Unicode/special/long/deleted path matrix.
- Run third-party menu preparation crash/hang injection and the 100-open
  PIDL/HMENU/USER-handle soak.

**Exit:** ordinary right-click still performs no Shell query, explicit native
actions cannot crash Ferail, and successful mutations become visible without a
whole-window or whole-model reload.

### Phase D — shortcuts, icons and Explorer transfers

This completes WIN-010, WIN-011 and WIN-012 while all items still have real
filesystem paths.

**On macOS**

- Define a cached neutral `LinkInfo` DTO: target kind/path or platform key,
  arguments, working directory, icon identity and broken state. Use it for
  alias UI/contract tests without making render or menu code resolve a link.
- Split asset cache keys into type/provider identity and content identity
  `(node/revision, size, purpose)`. Keep overlays per item when their state is
  not type-wide.
- Define transfer offers as capabilities: real paths, platform item ids and
  delayed streams, with copy/move/link effect, progress, cancellation and an
  explicit unsupported result. Keep extraction off the UI thread.

**Finish on Windows**

- Resolve `.lnk` through `IShellLinkW` on the bounded worker, cache by file
  identity/revision, preserve arguments on Open and continue mutating the link
  file itself.
- Complete cloud/offline and special-folder icon behavior through the bounded
  asset lane; compare the representative corpus with Explorer at 100/150/200%.
- Complete Explorer clipboard and drag/drop in both directions for normal,
  UNC, multi-parent, `.lnk`, OneDrive and Shell ID-list/delayed objects; reject
  unsupported virtual transfers visibly.

**Exit:** link behavior matches Explorer, no provider call occurs during
painting/menu construction, and transfer semantics are correct without
unbounded path or data-object materialization.

### Phase E — Windows platform locations: WSL bridge, then Shell namespace

This implements WIN-017 and WIN-013 only after path-based actions and
transfers have stable capability seams. WSL is the first slice because it
proves the dynamic-location/state contract while still handing navigable
content back to `NativeFs`; opaque PIDL-backed locations follow only after
that smaller boundary is stable.

**On macOS**

- Introduce `LocationTarget::FileSystem` versus opaque
  `LocationTarget::Platform` and a stateful location-provider interface with
  streaming batches, parent/breadcrumb identity, cancellation and explicit
  unavailable state.
- Add the path-backed-platform-root specialization needed by WSL: cached
  roots may be stopped/starting/ready and activation asynchronously yields a
  `FileSystem` target. Exercise it with a deterministic fake Linux provider;
  do not emulate the registry or `wsl.exe` on macOS.
- Build the shared surface against an in-memory fake platform tree and, where
  useful, a macOS virtual-location analogue. Prove history, tabs, refresh,
  selection and disappearance without assuming a `PathBuf`.
- Keep normal `NativeFs` navigation and Flat View byte-for-byte on their
  existing row path; add allocation assertions preventing platform payload on
  ordinary rows.

**Finish on Windows**

- First implement the cached WSL distribution provider and lexical path
  converters from WIN-017. Discovery and explicit start are bounded,
  cancellable worker operations. Once a distro root resolves, route it through
  `NativeFs`; no registry value or WSL process state belongs to a file row.
- Complete WSL symlink, `/mnt/<drive>`, stopped/disconnected and privacy tests
  before adding PIDL-backed browsing. This preserves the useful behavior from
  Ferail-Win32 without its UI-adjacent blocking calls or broad metadata skip.
- Implement a tab-owned identity arena containing PIDL bytes and optional
  parsing names, indexed by compact integer ids in the shared rows;
  enumerate This PC, Recycle Bin, provider roots and MTP in streaming batches.
- Route only pathless items through provider capabilities for open, parent,
  breadcrumbs, properties, native menu, transfers and icons. Immediately hand
  real filesystem directories back to `NativeFs`.
- Test device disconnect/reconnect and stale identity as a recoverable
  unavailable state; never persist or dereference a raw pointer.

**Exit:** Windows can browse WSL distributions, This PC, Recycle Bin,
OneDrive/provider roots and MTP while local NTFS navigation and Flat 4M remain
within their baseline. Merely displaying Linux locations starts no distro and
no path-backed WSL row carries platform-provider state.

### Phase F — metadata and native Properties

This completes WIN-014 without turning file inspection into background
indexing.

**On macOS**

- Cache portable image metadata by file identity/revision with a bounded
  lifetime. Parse only on explicit Get Info/preview demand, never while merely
  listing or painting.
- Define neutral grouped property DTOs and privacy classifications. Values that
  may contain personal information are display-only by default and excluded
  from logs, reports and persistent caches unless explicitly approved.
- Keep the current policy that GPS coordinates are never parsed, displayed or
  persisted; only coordinate presence may be shown.

**Finish on Windows**

- Gather the approved `IPropertyStore` subset off-thread and merge it with the
  portable groups without duplicating fields.
- Add direct **Windows Properties…** through the Shell capability for files,
  folders, shortcuts and supported namespace items, using the isolated/native
  ownership rules already established.
- Run known-answer JPEG/TIFF/HEIC, malformed metadata and privacy persistence
  checks.

**Exit:** Get Info remains on-demand and responsive, native Properties works,
and WTEST-120 finds no retained preview pixels, personal paths or metadata
values outside the documented policy.

### Phase G — release qualification and distribution

**On macOS/Linux**

- Run the shared regression slice after every phase and the complete
  navigation, menu, selection, preview, clipboard and Flat 1M/4M suite before
  the release candidate.

**Finish on Windows**

- Run the exact remaining WTEST matrix on WENV-A, the constrained/DPI subset on
  WENV-B and the exact portable artifact offline on WENV-C.
- Verify clean and upgraded profiles, version/commit/resource stamping, PDB
  identity, helper cleanup and privacy/recovery cases.
- Decide whether the product remains portable-ZIP-only. If so, mark installer
  cases N/A with that product decision; otherwise qualify and sign the
  installer. Obtain Authenticode before calling the Windows distribution
  complete.

**Exit:** the sign-off record names one exact artifact and commit, all P0/P1
entries are either passed or deliberately removed from product scope, and no
cross-platform or four-million-row regression remains.

## Original campaign ledger and completion status

### Phase 0 — Windows baseline and decision capture

- [~] Set up the real Windows development/test machine with Rust, debugger,
  symbol retention, Windows SDK tools, clean test profile, and release build.
- [~] Preserve the tester fixtures and create synthetic equivalents containing
  no personal data. `WCORPUS-OPEN` exists; the scalable corpus generators in
  continuation Phase A remain.
- [ ] Capture baseline timings: directory first paint, scroll frame time,
  normal context-menu open, idle CPU/redraws, memory/handles, 10k-image
  scrolling, and 10k-row detail enrichment.
- [ ] Reproduce each report item and attach it to its `WIN-xxx` entry.
- [ ] Confirm the now/later cut with the user before implementation expands
  beyond P0 containment and correctness.

### Phase 1 — make failures diagnosable and non-fatal

- [~] WIN-001 diagnostics, PDBs, dumps and breadcrumbs ship; measured
  multi-window/helper soaks remain.
- [~] WIN-002 preview broker and provider quarantine ship; the exhaustive
  hostile-provider GUI matrix remains.
- [~] WIN-003 bounded preview scheduling ships and the 10k user flow passes;
  shared apply/cache limits and measured repetition remain.
- [~] WIN-004 user flow passes and 0.6.8 improves large selection/drag;
  stale-row mutation tests and measured matrices remain.
- [~] WIN-015 portable-runtime packaging gate: static CRT, dependency gate,
  symbol bundle and clean-Sandbox portable launch verified; signing and any
  future installer remain.

### Phase 2 — restore the Prime Directive under Windows load

- [~] WIN-005 viewport-only details complete; shared I/O budget remains.
- [~] WIN-006 Task Manager-compatible metrics complete; idle redraw audit and
  expanded diagnostics remain.
- [ ] Re-run the baseline and reject any navigation/scroll regression.

### Phase 3 — filesystem interoperability

- [~] WIN-008 Shell API/default open implemented; Windows matrix remains.
- [~] WIN-009 PIDL-based Reveal implemented; Windows matrix remains.
- [~] WIN-012 normal-path outbound OLE drag implemented; Explorer clipboard,
  provider and virtual-item matrix remains.
- [~] WIN-016 missing keymap dispatch fixed; artifact consistency remains.

### Phase 4 — native compatibility on demand

- [~] WIN-007 isolated native Windows context menu, with no prefetch; UI and
  third-party-extension manual matrix remains.
- [ ] WIN-010 shortcut semantics.
- [~] WIN-011 icon/thumbnail correctness: Fonts and fallback separation ship;
  cloud/special-provider cache and multi-DPI work remains.
- [~] WIN-014 portable photo metadata ships; caching, Windows property values
  and direct native Properties remain.

### Phase 5 — Windows platform locations

- [~] WIN-017 cached WSL discovery, explicit activation and path-backed
  `NativeFs` handoff implemented in source; WTEST-130–139 remain on Windows.
- [~] WIN-013 pure platform-location contract, bounded worker bridge and
  tab-owned virtualized surface complete; Windows namespace provider remains.
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
   actionable, privacy-conscious diagnostics if anything still fails; and
9. WSL distributions are discovered without starting them, activate only on
   explicit navigation, then use the normal compact filesystem path without
   leaking registry base paths or browsed Linux paths into diagnostics.
