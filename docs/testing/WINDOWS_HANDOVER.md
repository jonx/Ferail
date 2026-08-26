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
- Last published Windows baseline: `3cc8b7e` (`release: Ferail 0.6.8 Windows
  drag responsiveness`)
- Mac-first preparation series: `6622f0d` through `77a5d36`, followed by the
  current handover/audit commit. Use `git rev-parse HEAD` after pulling rather
  than assuming the last abbreviated hash in this document.
- Current Windows release: `0.6.8`
- Published artifacts: unsigned portable Windows x64 ZIP plus its matching
  x64 symbols ZIP
- Next campaign: connect the prepared shared contracts to their GPUI owners,
  then implement and accept the Windows Shell mechanisms on a real Windows
  machine;
  see the
  [Mac-first continuation plan](../features/WINDOWS_COMPATIBILITY_PLAN.md#continuation-plan-mac-first-windows-final).
- Added scope: WSL distributions are implemented in source as cached dynamic
  **Linux** locations, adapted from the pinned Ferail-Win32 reference. Registry,
  `wsl.exe`, UNC and symlink behavior still requires real-Windows qualification
  (WIN-017/WTEST-130–139).

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

### 2026-08-26 — Windows continuation from the Mac-first series

```text
Date / machine: 2026-08-26, real Windows development machine
Pulled start commit: ea27263
Windows validation and corrections:
  - cargo check -p ferail-gpui passes;
  - cargo clippy -p ferail-gpui --lib -- -D warnings passes;
  - the Windows WSL contract tests pass (4 tests);
  - the shared namespace, shortcut and property contract tests pass;
  - the GPUI namespace tests pass with the intended --lib target (9 tests);
  - fixed three cfg-specific warnings which macOS could not expose in
    directory_reader.rs/settings.rs and three strict-Clippy failures in the
    pulled shell renderer;
  - fixed a Windows rustc stack overflow in the new Disk Usage path-arena test.
    Its glob import captured GPUI's #[test] macro; explicit imports keep the
    ordinary Rust test macro and remove the recursive expansion. No larger
    recursion/stack setting is required.
Asset coordinator slice completed:
  - ProcessState now owns exactly one AssetWorkCoordinator with separately
    bounded provider, decode, upload and apply lanes;
  - requests carry a process-local surface scope because a generation number
    belongs to one tab/surface. Retiring generation 2 in one tab can no longer
    cancel generation 1 in another tab which happens to use the same number;
  - active reservations are keyed by scope + compact asset key, and stale
    completion still cannot free a newer generation's slot;
  - detailed submit/retirement returns displaced or dropped pending requests
    to the host. This is required to release external path/pixel payloads and
    retryable thumbnail-cache reservations instead of leaving permanent
    "in flight" entries;
  - independent lane caps, cross-surface isolation, eviction cleanup and stale
    completion are covered by 8 ferail-core asset_work tests;
  - ferail-core strict Clippy and ferail-gpui Windows compilation pass.
Thumbnail dispatcher completed in the following Windows slice:
  - list and grid viewports now submit to one process-owned payload dispatcher;
    the old per-FileList active/latest batch state and the grid's independent
    six-worker waves are removed;
  - ordinary NodeId + revision + size requests coalesce across surfaces. Flat
    scan-local NodeIds include their numeric surface scope, so two arenas that
    both minted NodeId(1) cannot share a job or cache result;
  - every requester remains a separate waiter carrying table scope,
    generation, row index and NodeId. Completion checks all four before it
    notifies the table or grid-owning Shell; stale rows do not repaint;
  - native thumbnail provider calls use the process provider lane (4 active,
    128 pending). New selected/visible work can displace overscan and the
    evicted path reservation is explicitly released as retryable;
  - decoded image construction uses the upload lane, at most two per frame;
    cache insertion and affected-surface notification use the apply lane, at
    most eight per frame. Row count and scrollbar geometry are untouched;
  - a viewport generation change removes obsolete pending waiters/jobs. An
    already-running provider is allowed to finish into the shared cache rather
    than being overlapped or throwing away reusable work.
Still open in asset routing:
  - path/type icon warming is now routed through the same provider, upload and
    apply lanes. Type keys and path+size keys coalesce across Shell, grid and
    tree requesters; negative results cache the normal blank fallback while a
    canceled reservation remains retryable;
  - route shortcut resolution and approved property reads through the provider
    lane;
  - expose aggregate lane counts/cancellations in task diagnostics without a
    repaint and run the real 10k/hostile-provider/DPI matrix.
Formatting note:
  - cargo fmt --all -- --check also reports pre-existing formatting drift in
    pulled core/win32 files under rustfmt 1.9.0. Files changed in this Windows
    continuation were formatted directly; do not mix a repository-wide
    mechanical rewrite into the scheduler behavior change without review.
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
Later preparation blocks in this handover supersede the original shared-work
list above: capability-gated actions, symbolic transfer selection, shortcut
and property DTOs/caches, asset scheduling, and command consistency now exist.
Remaining shared/UI work before or alongside the Windows provider:
  1. Add seeded/screenshot access and visual polish without adapting rows into
     FileEntry or fake PathBuf values.
  2. Connect action/property/shortcut/asset contracts to process/tab owners;
     unsupported capabilities stay absent or explain why they cannot run.
  3. Keep pathless status/filter semantics distinct from filesystem tab state.
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

### 2026-08-26 — WIN-013 namespace enumeration on Windows

```text
Implemented:
  - visible Windows sidebar entries for This PC and Recycle Bin, with
    modifier-click opening the namespace in a new tab;
  - a tab-owned Win32 provider arena containing copied desktop-absolute PIDL
    bytes only. Session-local integer ids remain the sole row identity;
  - explicit Shell enumeration in a disposable worker-mode Ferail process,
    with a ten-second deadline, cancellation, kill/wait and a 64 MiB protocol
    ceiling. A crashing or hanging MTP/Shell provider cannot crash or consume
    a GPUI executor worker indefinitely;
  - filesystem children hand their SIGDN_FILESYSPATH straight back to
    NativeFs. Pathless containers retain platform identity and enumerate via
    BHID_EnumItems; no fake PathBuf enters FileEntry or NodeStore;
  - UI batches remain capped at 512 and the existing four-batch GPUI channel
    provides the final apply backpressure. The binary broker protocol also
    streams through a bounded 512-record channel and flushes every 128 rows;
    neither process constructs a duplicate full-list snapshot;
  - repeated refreshes deduplicate identical copied PIDLs inside the tab arena
    rather than growing one identity entry per refresh;
  - capability gating is deliberately conservative: only navigation that is
    implemented is advertised;
  - pathless rows now advertise NATIVE_MENU and pass an explicit selection of
    at most 128 copied PIDLs to the existing disposable context-menu broker
    over stdin. Every PIDL is structurally validated before use; the broker
    still owns QueryContextMenu, invocation and modeless Properties lifetime;
  - the instant Ferail row menu exposes More…, while Shift+right-click skips
    it and requests the official Windows menu directly. Completion refreshes
    only that namespace session;
  - restore/delete, property DTOs and transfers remain absent rather than
    silently misbehaving until their provider actions are implemented.
  - audited the shared Shell action surface: filesystem target resolution is
    empty in namespace mode, and Get Info, paste/move-paste, Copy File List,
    terminal, New Folder, Flat View, Disk Usage and all three empty-pane drop
    paths now fail explicitly. None can reuse the tab's display-only
    `current_dir` snapshot from before the namespace was opened.
Qualification boundary:
  - this commit delivers namespace discovery/browsing, not the remaining
    action slice. WTEST-100–106 stay unchecked until native actions and real
    device/disconnect evidence are complete.
```

### 2026-08-25 — macOS preparation for WIN-010 shortcuts

```text
Shared contract prepared:
  - ferail-core::platform_shortcuts defines an owned, Send-safe resolver
    request/result contract for filesystem, Shell-namespace and URL targets;
  - only a resolved real directory produces Navigate(path); files,
    applications, URLs and provider targets produce InvokeShortcut so Windows
    invokes the .lnk itself and preserves its arguments/working directory;
  - broken/unsupported/cancelled states cannot fabricate a target;
  - the bounded memory-only cache uses NodeId + file revision, never a source
    path key, and immediately evicts a revision mismatch;
  - custom Debug implementations redact source/target/icon/working-directory
    paths, URLs and argument contents.
Reference checked:
  - ShellBat/ShellN.Extensions/Utilities/Link.cs at pinned ShellBat commit
    7b8268d60648b74f5a874be8edde3b4fc17f7cdc.
Host proof:
  - cargo test -p ferail-core platform_shortcuts
  - cargo clippy -p ferail-core --all-targets -- -D warnings
Windows implementation still required:
  1. Implement ShortcutResolver in ferail-shell-win32. On the worker COM
     apartment, CoCreate ShellLink, IPersistFile::Load the .lnk and read owned
     copies from GetIDList/GetPath/GetArguments/GetWorkingDirectory/
     GetIconLocation. No COM interface or borrowed PIDL crosses the worker.
  2. Classify a filesystem target without opening its contents; retain an
     owned absolute PIDL identity when GetPath has no usable filesystem path.
     Use bounded/cancellable resolution and never show Shell resolution UI.
  3. Add the process cache to GPUI and request only on explicit Open/Get Info
     or visible icon/preview work. Apply by NodeId + revision + generation.
  4. Route Open from ShortcutInfo::open_disposition. Keep rename/copy/move/
     trash targeting the original .lnk path unconditionally.
  5. Add the Get Info shortcut section from cached DTO fields; never resolve
     from render or menu construction, and never log the values.
  6. Execute WTEST-080–087 on file/folder/app-with-arguments/relative/UNC/
     Shell-target/broken fixtures. Re-run 10k media and Flat 1M/4M gates.
Windows cases claimed from macOS: none.
```

### 2026-08-26 — WIN-008/009/010 path interoperability

```text
Implemented on Windows:
  - WindowsShortcutResolver loads .lnk files with IShellLinkW/IPersistFile in
    a fresh STA, uses non-interactive/no-search resolution flags, and copies
    path, arguments, working directory and icon location into owned DTOs;
  - explicit shortcut Open is scheduled through the shared process Provider
    lane and capped Apply lane, cached by NodeId + FileRevision (512 entries),
    and accepted only when tab, load generation, row identity and revision
    still match;
  - real directory targets navigate in Ferail; files/applications invoke the
    original .lnk so Windows retains arguments and working-directory rules;
  - normal Open and Reveal surface final failures to the user; Reveal retains
    the closest-existing-parent fallback;
  - after the isolated native context-menu broker closes successfully, only
    tabs showing the selected paths' parent directories reload.
Validation:
  - cargo check -p ferail-gpui --all-targets
Still required for WIN-010:
  - PIDL/Shell-namespace-only shortcut targets and the Get Info shortcut
    section;
  - the manual WTEST-060–065 and WTEST-080–087 matrices.
```

### 2026-08-25 — macOS preparation for WIN-012/013 provider actions

```text
Shared contract prepared:
  - every platform action (including Rename) maps to one advertised
    capability; file/container appearance never implies support;
  - native-menu requests distinguish normal and extended invocation but share
    the same explicit, capability-gated route for provider files and folders;
  - a provider action receives its tab-owned PlatformLocation plus a symbolic
    selection. Select All is `all + exceptions`, so four million rows do not
    become four million ids, PIDLs or paths in GPUI;
  - explicit selections and exceptions are sorted/deduplicated at the request
    boundary, and Debug output exposes only their count;
  - enumeration-only providers safely return Unsupported by default;
  - a provider can hand resolved real paths back to existing NativeFs/file-op
    flows, but those paths remain redacted from Debug output.
Host proof:
  - cargo test -p ferail-core platform_namespace
  - cargo test -p ferail-gpui platform_namespace
  - cargo check -p ferail-gpui
Windows implementation still required:
  1. Override perform_action in the tab-owned Shell provider. Validate the
     advertised capability again on its worker and resolve each compact id
     from that provider's owned PIDL arena; reject ids from another generation.
  2. Expand a complement selection incrementally on the worker only after an
     explicit action. Bound intermediate PIDL/data-object storage and honour
     cancellation; never materialize Select All from render, selection change,
     menu construction or drag hover.
  3. Extend the isolated context-menu broker input to owned namespace
     identities. Request `IContextMenu` for both provider files and provider
     containers only when NATIVE_MENU is present; keep the instant Ferail menu
     unchanged and do not restore QueryContextMenu prefetch.
  4. Implement `IDataObject`/`CFSTR_SHELLIDLIST`, `CF_HDROP` when real paths
     exist, and Preferred DropEffect for clipboard and drag. Delayed provider
     extraction starts only on explicit paste/drop and exposes progress/cancel.
  5. Hand real resolved paths to existing collision/quarantine-aware file-op
     flows. For pathless or unsupported transfers, return a precise message;
     never fabricate a path or silently drop an item.
  6. Execute WTEST-070–077, WTEST-090–096, WTEST-100–106 and WTEST-121. Include
     file and directory native menus, provider file and container menus, plus
     symbolic Select All at four million rows; re-run Flat 1M/4M baselines.
Windows cases claimed from macOS: none.
```

### 2026-08-26 — WIN-012 path-backed clipboard and drag formats

```text
Implemented:
  - Windows clipboard writes CF_HDROP plus Preferred DropEffect=Copy/Move and
    reads the same effect from Explorer/other file managers;
  - Cut state no longer clears when Paste merely starts; it clears only after
    a non-cancelled move with no failed items;
  - outbound OLE Shell data objects expose CF_HDROP and CFSTR_SHELLIDLIST;
  - incoming drop negotiation maps Ctrl=Copy, Shift=Move and Ctrl+Shift or
    Alt=Link, never advertising an effect omitted by the source; Link creates
    .lnk files in the destination and does not mutate source targets;
  - eager clipboard and drag expansion stays capped before symbolic Select All
    can materialize an unbounded path list (20k clipboard, 10k drag).
Still required:
  - delayed FILEDESCRIPTOR/FILECONTENTS extraction for pathless provider/MTP
    items, owned namespace PIDLs and the manual WTEST-090–096 matrix;
  - verify hydrated/placeholder OneDrive behavior with real accounts.
```

### 2026-08-25 — macOS preparation for WIN-014 property data

```text
Shared contract prepared:
  - RevisionCache is a reusable bounded process-memory FIFO keyed by compact
    identity + caller-owned revision. It stores no source path, evicts a stale
    revision immediately, supports explicit clear/drop, and Debug reports only
    capacity and count;
  - the existing shortcut cache now uses that same implementation, preserving
    its public contract and revision invalidation;
  - PlatformProperties is an owned grouped DTO restricted to useful text/list,
    boolean, integer and timestamp values. Native blobs, PROPVARIANTs, COM
    interfaces and borrowed strings cannot enter shared/UI state;
  - property values, localized display names, section names, target paths and
    provider identities are redacted from Debug. ImageMeta now similarly
    reports only EXIF-field presence, never camera/lens/time/exposure values;
  - PlatformPropertiesProvider is a cancellable worker-only seam. Filesystem
    surfaces can cache by NodeId + FileRevision; a tab provider can cache by
    its compact item id + generation/revision without persisting a parsing name.
Host proof:
  - cargo test -p ferail-core revision_cache
  - cargo test -p ferail-core platform_properties
  - cargo test -p ferail-core platform_shortcuts
  - cargo test -p ferail-core image_meta_debug_redacts_exif_values
  - cargo clippy -p ferail-core --all-targets -- -D warnings
Windows implementation still required:
  1. Define an explicit allowlist of useful PROPERTYKEY values not already
     supplied by portable metadata. Read them through IPropertyStore on the
     bounded Windows worker and convert/clear every PROPVARIANT there.
  2. Merge returned DTO sections into Get Info without duplicate portable
     fields. Fetch only on explicit Get Info/visible details demand; render,
     listing, hover and Flat scanning must perform no property I/O.
  3. Add one process/surface-owned bounded cache. Apply results only when item
     identity, file/provider revision and UI generation still match; clear it
     when its owner closes and never write it to metadata.db or disk.
  4. Add Windows Properties through the explicit PROPERTIES capability using
     the native isolated ownership rules. Cover ordinary files, directories,
     .lnk files, provider files and provider containers.
  5. Execute WTEST-110–115 and WTEST-120–123 with unique privacy canaries,
     malformed values, replacement/revision invalidation, disconnect and low
     memory. Re-run ordinary listing and Flat 1M/4M performance gates.
Windows cases claimed from macOS: none.
```

### 2026-08-26 — WIN-014 approved Windows property data

```text
Implemented:
  - WindowsPropertiesProvider opens IPropertyStore in a fresh STA only for an
    explicit Get Info request and copies a fixed allow-list into owned neutral
    property DTOs; arbitrary keys/blobs are never enumerated;
  - the STA now lives in a disposable `--windows-properties-broker` process,
    not a joined in-process thread. The host drains a 1 MiB-bounded protocol,
    cancels or kills/waits after eight seconds, validates section/property/text
    counts and accepts owned UTF-8 scalar data only;
  - GPS latitude/longitude keys are absent by construction and covered by a
    unit test;
  - filesystem results are cached process-wide by NodeId + FileRevision with
    a 256-entry memory-only bound; replacement/mtime changes evict stale data;
  - Get Info validates path/identity/revision before applying and cancels a
    read when its embedded view retargets or window closes;
  - .lnk Get Info reuses/resolves the bounded shortcut DTO and shows target,
    arguments and working directory without persisting or logging values.
Still required:
  - namespace-item property keys once the PIDL provider lands;
  - manual WTEST-110–115 canary/rewrite/privacy verification.
```

### 2026-08-25 — macOS preparation for WIN-016 command consistency

```text
Shared/startup guard prepared:
  - install_binding now returns whether the exact production dispatcher
    recognized a catalogue id; startup behavior remains non-fatal for a truly
    unknown id and keeps its explicit warning;
  - a GPUI test creates a real TestAppContext, translates every shortcut in the
    platform-compiled bundled catalogue, and passes each binding through that
    same production dispatcher;
  - any future catalogue shortcut lacking a GPUI, app-level or deliberately
    deferred route fails the test with its command id and translated binding;
  - the documented `+` alternate remains the only accepted untranslatable key
    because GPUI reserves plus as its chord separator and `=` is also bound.
Host proof:
  - cargo test -p ferail-gpui \
      keymap::tests::every_bundled_shortcut_has_a_recognized_route
  - cargo check -p ferail-gpui
Windows implementation/qualification still required:
  1. Run that exact test under the Windows target/build used for packaging; the
     cfg-compiled catalogue differs (for example preview and reveal labels).
  2. Launch the exact packaged executable once with a clean profile and once
     over a copied 0.6.5 profile. Capture startup logs and verify no bundled
     `unknown command id`; press Ctrl+Shift+L and the other Windows chords.
  3. Add versioned user-keymap migration if/when user bindings are persisted.
     Diagnostics must call an obsolete user id a user migration issue, not a
     broken bundled catalogue.
  4. Finish the packaging consistency gate: executable, helper(s), bundled
     resources, report header and symbol manifest must share one version and
     commit; inject a stale fixture to prove the gate fails.
  5. Execute WTEST-014 and WTEST-015. Do not mark either complete from the
     macOS test alone.
Windows cases claimed from macOS: none.
```

### 2026-08-25 — macOS preparation for WIN-011 asset scheduling

```text
Shared scheduling primitive prepared:
  - AssetKey carries a compact NodeId or tab-local PlatformItemId, a file or
    opaque provider revision, and the requested icon/thumbnail/property/link
    kind. It contains no path, PIDL, pixels, COM object or provider string;
  - BoundedAssetLane has fixed active concurrency and pending capacity.
    Submitting four million speculative requests retains only the configured
    constant-size queue;
  - selected work outranks visible work, visible work outranks overscan, and
    ties favor the newest request so rapid scrolling does not create a train;
  - duplicate work is coalesced/reprioritized. When a generation is retired,
    pending work is dropped and active cancellation tokens are set, but slots
    remain occupied until worker acknowledgement, preserving the hard cap;
  - completion is keyed by asset identity/revision plus UI generation, so a
    stale worker cannot release a newer reservation.
Host proof:
  - cargo test -p ferail-core asset_work
  - cargo clippy -p ferail-core --all-targets -- -D warnings
Windows/shared integration still required:
  1. Own one coordinator in process state with separately configured provider,
     decode, upload and UI-apply lanes. Feed it only selected/visible/overscan
     compact identities; never enumerate a whole directory to populate it.
  2. Route IShellItemImageFactory, SHGetFileInfo, .lnk resolution and approved
     property reads through the provider lane. Keep IPreviewHandler/PDF/unsafe
     fallbacks in their disposable broker boundary and count each broker as an
     active provider slot.
  3. Apply results only by identity + revision + generation, under a per-frame
     upload/apply budget, then invalidate affected visible rows only. Never
     change row count or scrollbar geometry from an asset completion.
  4. Migrate type-icon caching to stable type/provider identity and content
     thumbnails to file/provider identity + revision + size. Add explicit byte
     and handle budgets and an expiring transient-negative result; cancellation
     is not a negative result.
  5. Publish lane counts/cancellations in task snapshots without causing a
     repaint. Execute WTEST-040–048, WTEST-087, WTEST-106 and WTEST-123 plus
     the 10k media and Flat 1M/4M regressions at multiple DPI values.
Windows cases claimed from macOS: none.
```

### 2026-08-25 — native Properties lifetime correction

```text
Reported symptom:
  - choosing Properties from Ferail's original Windows Shell menu appears to do
    nothing.
Root cause and source correction:
  - the context-menu broker correctly recognized the canonical `properties`
    verb, but replaced the selected command with SHObjectProperties (or
    SHMultiFileProperties) and exited as soon as that API reported successful
    invocation;
  - property sheets may be modeless. The sheet and its property-page handlers
    therefore lost their STA owner/message pump with the disposable broker;
  - the broker now invokes the exact selected IContextMenu offset, preserving
    built-in and third-party pages, then pumps messages while an owned property
    sheet exists. Once observed, the dialog is user-modal and has no timeout;
  - synchronous handlers and handlers which transfer UI to another process do
    not become errors. The pre-popup eight-second extension deadline and normal
    right-click fast path are unchanged.
Host proof:
  - cargo check -p ferail-shell-win32 --target x86_64-pc-windows-msvc
  - cargo clippy -p ferail-shell-win32 --target x86_64-pc-windows-msvc --
      -D warnings
Windows validation still required:
  1. WTEST-072 on one file, one directory and same-parent multi-selection.
     Open each built-in/third-party tab, Apply/Cancel/OK, and verify the exact
     selected objects are shown.
  2. While Properties is open for two minutes, confirm one context-menu broker
     remains, Ferail is responsive, and normal browsing creates no menu work.
     Closing the sheet must let that broker exit promptly.
  3. Repeat 100 open/close cycles and record process, USER/GDI handle and COM
     growth for WTEST-077. Crash/hang a third-party property page if a safe test
     handler is available; only the broker may fail.
  4. Re-test 7-Zip/Git/Defender verbs to prove the general InvokeCommand path
     and normal menu latency did not regress.
Windows cases claimed from macOS: none.
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

### 2026-08-26 — Windows continuation checkpoint after source completion

```text
Commits in this uninterrupted Windows pass:
  dc9fd49 path Open/Reveal + .lnk resolution
  f3c7212 Explorer cut/copy/drop-effect interop
  d50b19a approved IPropertyStore/Get Info data
  4fefc9b explicit-only, tab-cancellable WSL activation
  199d1de This PC/Recycle Bin/MTP namespace browsing
  efcf23a bounded streaming namespace broker
  2e1c298 PIDL-native More… / Shift+right-click menu
  a435dcc namespace/filesystem command isolation
  930cdc6 disposable bounded property-handler broker

Source-complete claims:
  - path-backed Open/Reveal, .lnk navigation/invocation, Explorer clipboard
    effects and outbound Shell ID-list drag formats;
  - approved Windows property DTO/cache path with hostile handler containment;
  - WSL discovery/activation ownership with no implicit distro start;
  - read-only pathless namespace browsing, filesystem handoff, MTP container
    traversal and explicit native Shell actions through the broker;
  - ordinary FileEntry/NodeStore/Flat rows retain no WSL or PIDL state.

Not source-complete and therefore not claimed:
  - native provider transfer via FILEDESCRIPTOR/FILECONTENTS and direct
    provider property DTOs. These remain capability-hidden; the official
    Windows menu supplies provider verbs such as Properties/Restore when the
    installed provider advertises them;
  - release qualification WTEST-072, 095, 100–106, 110–115 and 130–139,
    including a real MTP disconnect, stopped WSL distro, privacy-canary
    inspection, Flat 1M/4M comparison and hostile extension observation;
  - Authenticode/signing and publication. Do not tag a successor to 0.6.8
    until the exact portable artifact passes those real-machine gates.

Validation accumulated during this pass:
  - 31 ferail-shell-win32 tests pass after namespace/context integration;
  - focused WSL (4), platform-location (4), property protocol (2) and PIDL
    validation tests pass;
  - strict Clippy passes for ferail-shell-win32 and ferail-gpui after each
    completed slice; git diff --check is clean at every commit boundary.
```

The original crash report's principal P0 user flows now pass in real Windows
use. The Mac-first series has prepared the namespace, WSL, shortcut, action,
transfer-selection, properties/cache, asset-budget and command-consistency
contracts. Resume implementation in this order:

1. Qualify the completed process-owned asset integration with the 10k and
   hostile-provider matrix. Thumbnails, type/path icons and shortcut
   resolution now share the bounded provider/apply owner with generation and
   row validation; keep ordinary and Flat row models unchanged.
2. Complete path-based interoperability: the actionable Open/Reveal path,
   targeted post-Shell-verb refresh and path-backed `.lnk` resolver are wired;
   finish namespace-only shortcut identities plus the Explorer clipboard/drag
   format matrix through symbolic selections.
3. Qualify the source-complete WSL path-backed location slice through
   WTEST-130–139: cached installed distributions, no implicit start, explicit
   stopped→starting→path activation, then a `NativeFs` handoff. Only after that
   gate, follow with PIDL-backed This PC, Recycle Bin, provider roots and MTP.
   Never put WSL provider state, PIDLs or COM state on ordinary filesystem or
   Flat View rows.
4. Complete metadata and Properties by connecting the prepared bounded
   revision cache/neutral DTO, then add the Windows `IPropertyStore` worker and
   direct native Properties action.
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
    uses a five-second `readlink -f --` resolver with generation/cancel guard
    and checked /mnt/<drive> conversion;
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

### 2026-08-26 — WIN-017 implicit-start and cancellation audit

```text
Implemented on the real Windows development machine:
  - removed the failed-empty-listing readlink fallback. It could invoke
    `wsl.exe -d` merely because ordinary UNC enumeration failed after a distro
    stopped, violating the no-implicit-start release gate. Only explicit row
    activation may now invoke the WSL resolver;
  - attached stopped-distro activation cancellation to the originating tab.
    Navigation, tab close and window close now set the worker flag, whose
    bounded process loop kills and waits for wsl.exe before returning;
  - late activation completion must still own the exact tab cancellation
    token and load generation before either same-tab or new-tab navigation;
  - native Windows context menus fail explicitly for WSL paths instead of
    handing them to arbitrary Shell extensions, and symbolic selections over
    20,000 items are rejected before paths are materialized;
  - UNC parsing rejects dot/dot-dot components before conversion to a Linux
    argv path.
Qualification boundary:
  - these changes close the source-level implicit-start, stale-apply and child
    ownership defects found during review;
  - WTEST-130–139 remain unchecked until their manual environment/process,
    privacy and performance observations have been recorded. Do not turn this
    source audit into a claimed end-to-end pass.
```

### 2026-08-26 — fast recursive enumeration / Disk Usage handover

```text
Date / machine: 2026-08-26, macOS development machine
Commit: uncommitted; record the eventual isolated commit before moving to PC

Implemented and verified on macOS:
  - one shared directory reader now serves ordinary streamed listings, Flat
    View, recursive search and Disk Usage;
  - macOS reads name, type, sizes, dates, flags, file id, link count and mount
    status with getattrlistbulk; a direct APFS test proves the normal fixture
    returns with zero per-entry metadata fallbacks;
  - recursive callers use a bounded coordinator and up to eight workers only
    on local, non-removable APFS; other media/platforms stay serial;
  - worker results are capped at two 256-entry batches per worker, cancellation
    clears queued directories, and all policy/counters/callbacks remain on the
    single coordinator thread;
  - Disk Usage uses scan-local ids plus one parent id per node. Closing or
    refreshing the tool drops its path index instead of retaining millions of
    paths in NativeFs;
  - skipped subdirectories are counted. A macOS TCC denial exposes Full Disk
    Access contextually and in Settings > Performance; FDA changes coverage,
    not the fast-reader availability.

Host proof:
  - cargo check -p ferail-fs-native
  - cargo test -p ferail-fs-native --lib (190 passed)
  - cargo check -p ferail-gpui
  - cargo test -p ferail-core i18n::extract
  - cargo test -p ferail-core i18n::pack (bundled packs fully cover source)

Windows behavior in this slice:
  - directory_reader uses the existing portable DirEntry metadata path, which
    is backed by WIN32_FIND_DATA and keeps the OneDrive no-hydration rule;
  - recommended_recursive_workers returns one, so no Windows concurrency or
    behavior change is claimed before a real-machine run;
  - no administrator requirement, prompt, raw-volume access or MFT parser has
    been added to the main application.
  - a macOS cross-check of ferail-fs-native for x86_64-pc-windows-msvc stops in
    existing C dependencies before Rust application code: bzip2/lzma cannot
    find the Windows stdlib headers and blake3 cannot find ml64.exe. This is a
    host toolchain boundary; it is not a Windows compile claim for this slice.

Required Windows implementation (next isolated feature):
  1. Keep Ferail itself asInvoker. Add a narrowly scoped helper that is started
     with runas only after the user explicitly chooses Fast NTFS scan; never
     relaunch the whole GUI elevated.
  2. Put a versioned, length-prefixed protocol over a per-request named pipe
     protected for the invoking user. Send the requested root only in memory;
     do not log, persist or include it in reports. Authenticate the helper with
     a random nonce and reject oversized/malformed frames.
  3. Probe the volume first. Offer the fast engine only for local NTFS and keep
     the portable scanner for FAT/exFAT/ReFS/network/WSL and every denial,
     cancellation, helper crash, timeout or parser inconsistency.
  4. Read NTFS volume data and MFT extents sequentially, parse FILE records
     with fixup validation, and emit owned neutral records containing FRN,
     parent FRN, name, attributes, logical size and allocated size. Account for
     multiple FILE_NAME attributes/hard links, sparse/compressed files and
     records changing on a live volume. Never expose raw parser pointers.
  5. Reconstruct ancestry by FRN. A subdirectory request may use the same
     volume index but must emit only descendants of the requested root. Drop
     the entire index/helper when Disk Usage closes or cancellation completes.
  6. Stream bounded batches to the existing Disk Usage coordinator and keep
     the current generation/cancel/backpressure gates. Do not adapt MFT rows
     into Flat View until Disk Usage correctness and memory gates pass.
  7. Add an explicit engine label in the Disk Usage surface: Portable or Fast
     NTFS (administrator). Suggest Fast NTFS for a volume-sized portable scan,
     never at Ferail startup; remember a preference, not administrator state.

Real-Windows acceptance gates (none claimed from macOS):
  - build/package remains asInvoker and a normal launch shows no UAC prompt;
  - standard-user denial/cancel/helper crash all fall back visibly and leave
    the GUI usable; no orphan helper, handle or named pipe remains;
  - compare portable and fast totals/trees on a local NTFS fixture containing
    hard links, sparse/compressed files, Unicode/long names, inaccessible
    folders, junctions, mount points and files mutating during the scan;
  - scan C:\ and a deep subdirectory; the latter contains no sibling records;
  - cancel during volume read, parse and delivery, then close the tool and
    verify memory is reusable and helper/private index memory is gone;
  - repeat 20 open/cancel/close cycles and record GUI/helper private bytes,
    handles, elapsed time and output counts;
  - re-run ordinary listing, OneDrive no-hydration, WSL, 10k media and Flat
    1M/4M baselines. Fast DU must not alter their worker count or row model;
  - inspect logs, reports, metadata.db and crash bundles for requested paths,
    MFT names, pipe nonce and raw records: none may persist.
```

### 2026-08-26 — Open result location in a new tab

```text
Date / machine: 2026-08-26, macOS development machine
Commit: uncommitted; record the eventual isolated commit before moving to PC

Implemented in shared UI code:
  - Open in New Tab now accepts one folder or one file from an ordinary file
    list, including recursive Search results. A folder opens directly; a file
    opens its containing folder and queues selection of the exact filename;
  - every exact-duplicate or similar-image member has the same command in its
    card context menu, independently of the panel's marked-for-trash set;
  - a Disk Usage square has the same single-item semantics. Its scan-local
    path is reconstructed only when the user invokes the command;
  - building any of these menus performs no filesystem I/O. Multi-selection
    remains folder-only with the existing fan-out confirmation, so selecting
    millions of result files cannot accidentally create millions of tabs;
  - the implementation is cross-platform and does not invoke the Windows
    native-shell menu path. It therefore does not add prefetch work or change
    ordinary listing, Search, Duplicate Finder, Flat View or Disk Usage worker
    counts.

Required real-Windows checks (not claimed from macOS):
  1. Right-click one ordinary folder and one ordinary file; verify the folder
     opens directly and the file opens its parent with the exact row selected.
  2. Repeat for a recursive Search file result, an exact-duplicate member, a
     similar-image member, and both a folder and a file Disk Usage square.
  3. Repeat with spaces, non-ASCII names, a long-path-enabled NTFS path, a
     OneDrive placeholder, and a WSL UNC row. Confirm no placeholder hydration
     or WSL distribution start occurs merely from opening the context menu.
  4. Verify multi-selected folders retain the existing confirmation and that
     Open in New Tab is absent for a multi-selection containing only files.
  5. Run each command during and after result streaming, then close the source
     result tab. Confirm the destination tab remains usable, selection appears
     after its listing arrives, and there is no crash, hang or stale selection.
  6. Re-run the native Windows context-menu tests separately: this shared menu
     command must not re-enable background native-menu prefetch or alter the
     Ctrl/right-click native-shell path.
```
