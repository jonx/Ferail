# Upstream gpui / gpui-component: friction log

Things in [`gpui`](https://github.com/zed-industries/zed) (Zed) and
[`gpui-component`](https://github.com/longbridge/gpui-component) that cost us
extra work — limitations we had to work around, APIs we wish existed, and
behavior we had to fork or duplicate. The goal is to (a) remember *why* a
workaround exists so we don't "simplify" it back into breakage, and (b) have a
ready list of upstream issues/PRs to file when we get the time.

Each entry: what we hit, the workaround we shipped, and what upstream could do
to remove the need.

---

## 1. gpui-component pins `gpui` to an explicit rev — consumers must mirror it

**Hit during:** bump from `ba44512` → `c112e7b` (2026-06-16).

`gpui-component` declares its `gpui` dependency by git URL. Until ~mid-2026 it
left that dependency **unpinned** (no `rev`), and our strategy was to also leave
*our* `gpui`/`gpui_platform` unpinned so cargo would unify both onto one source.

As of upstream `c112e7b`, gpui-component's `Cargo.toml` pins gpui to an explicit
rev (`1d217ee…`). Our still-unpinned gpui then floated to the *latest* zed
`main` (`84b753cb…`), which is a **different source** than the rev gpui-component
demands. Result: `cargo update` produced **three** gpui revs in the graph
(`1d217ee` from the component, `84b753cb` from our float, `d2953a2b` stale) —
duplicate `gpui` types that don't interoperate.

**Workaround:** pin our `gpui` and `gpui_platform` to the *exact same* rev
gpui-component pins (`1d217ee…`). Collapses the graph back to one gpui. This is
now the documented rule in the root `Cargo.toml` pinning comment: when bumping
gpui-component, open its `Cargo.toml`, read `gpui = { rev = ... }`, and copy
that rev into ours.

**Update (2026-08-08, bump `c112e7b` → `6d7847e`):** gpui-component *removed*
its gpui pin again, so the rule is now conditional and both styles are
documented in the root `Cargo.toml` comment: when gpui-component is unpinned,
we leave ours unpinned too (both float onto one source; the committed
`Cargo.lock` is the real pin, moved deliberately with `cargo update -p gpui`);
when it pins, we mirror the rev. Same invariant either way — one gpui source
in the graph.

**Update (2026-08-28, bump `6d7847e` → `e8f54eb`):** the component remains
unpinned. Ferail moved the one shared Zed source to `f66ed399`, the exact Zed
revision exercised by gpui-component's own lockfile at `e8f54eb`. The committed
Ferail lock contains one Zed source and one gpui-component source; do not add a
`rev` query to only one dependency, because Cargo treats that as a distinct
source even when the commit hash is identical.

**What upstream could do:**
- gpui-component could re-export the `gpui` it builds against (e.g.
  `pub use gpui;`) so consumers depend on *its* gpui transitively instead of
  declaring their own — removing the rev-matching dance entirely.
- Or document the required gpui rev in a machine-readable spot (a
  `package.metadata` key) so a consumer can assert it rather than reading the
  Cargo.toml by hand.
- gpui itself publishing semver'd releases to crates.io would end the
  git-rev-pinning problem at the root.

---

## 2. Table events don't carry click `Modifiers` — forced a full table fork

**Filed upstream 2026-08-21:** [gpui-component#2795](https://github.com/longbridge/gpui-component/issues/2795).

**Hit during:** original multi-select work; re-confirmed during the `c112e7b`
bump (the fork is `crates/ferail-gpui/src/multi_table/`).

gpui-component's `Table` emits row events (`SelectRow` etc.) that do **not**
include the original click `Modifiers` (Cmd/Shift) or the modifier state at
dispatch time. A file manager needs modifier-aware selection (Cmd-add,
Shift-range), drag-select rubber-banding, a press-on-selected drag delay,
multi-row drag payloads, and empty-area-click-to-clear — none of which the
upstream event surface can express.

**Workaround:** we maintain a **full local fork** of the table + virtual-list
(`multi_table/`), whose key divergence is that `TableEvent::RowClicked` carries
the original `Modifiers` and `TableEvent::LeadMoved` carries
`window.modifiers()` at dispatch. Cost: every gpui-component bump risks our fork
(this bump's 4 `flex_grow/shrink` breaks were all in it, see #3), and we forgo
upstream's table fixes (e.g. `row_selector`→`row_header`, selection fixes) unless
we port them by hand.

**What upstream could do:**
- Include `Modifiers` (and ideally the raw `MouseDownEvent`/`window.modifiers()`)
  in `TableEvent` row variants, so consumers can do modifier-aware selection
  without forking.
- Expose hooks for: drag-threshold-before-collapse on a selected row, custom
  drag image/payload, and an empty-area (below-last-row) click event.
- A `TableDelegate` callback for per-cell click interception would cover the
  inline-rename / cell context-menu cases too.

## 3. gpui `flex_grow()` / `flex_shrink()` became 1-arg (silent churn)

**Hit during:** `c112e7b` bump (gpui rev `1d217ee`); upstream gpui-component
adapted the same in their PR #2433.

gpui changed `Styled::flex_grow()` / `flex_shrink()` from no-arg (implicit
`1.0`) to requiring an explicit `f32`. Four call sites in our forked table broke
(`flex_grow()` → `flex_grow(1.0)`, same for shrink). Pure mechanical fix, but
it's the kind of unannounced gpui-`main` signature churn that makes every bump a
compile-and-fix exercise rather than a lockfile change.

**What upstream could do:**
- gpui shipping versioned crates.io releases with a changelog would turn
  "discover breaks by compiling against a moving `main`" into normal semver.
  (Same root cause as #1.)

## 4. `context_menu` keeps open/close state private — no callback

**Filed upstream 2026-08-21:** [gpui-component#2796](https://github.com/longbridge/gpui-component/issues/2796).

**Hit during:** breadcrumb tooltip-vs-menu overlap fix.

`ContextMenuExt::context_menu` stores its `open` flag in private element
state (`ContextMenuState`) and exposes no `on_open`/`on_close` callback or
queryable "is this menu open" accessor. `Root` doesn't track open context
menus centrally either. So a consumer can't react to the menu opening/closing —
e.g. to suppress a sibling hover tooltip on the same element while the menu is
up (the breadcrumb crumb showed its full-path tooltip overlapping its own
right-click menu).

**Workaround:** track it ourselves — set `Shell::breadcrumb_menu_open` in the
menu builder closure (the one place upstream calls us when the menu opens),
gate the crumb tooltip on `!breadcrumb_menu_open`, and clear the flag on the
next left mouse-down at the shell root (which is also how the menu dismisses).
Works, but it's a bespoke state machine for something a callback would make a
one-liner.

**What upstream could do:**
- Add `.on_open_changed(|open| ...)` (or `.on_dismiss(...)`) to `context_menu`.
- Or expose the open state through `Root` so consumers can query "is a context
  menu currently open in this window".

## 4b. Async context-menu contents — RESOLVED for Ferail

**Filed upstream 2026-08-21:** [gpui-component#2797](https://github.com/longbridge/gpui-component/issues/2797).

**Hit during:** "Open With" arriving empty on the first right-click of a row
(docs/features/CONTEXT_MENU.md).

`ContextMenuExt::context_menu` still runs its root builder once, but
gpui-component #2609 added `PopupMenu::rebuild`: a retained submenu entity can
replace its own items after asynchronous work while keeping its identity,
parent, focus and layer priority.

That is fine for menus made of static labels, and wrong for a file manager. Our
menu content includes data that is *illegal* to fetch on the UI thread (the
Prime Directive): LaunchServices "Open With" candidates, per-row capabilities,
anything that touches the shell. Those arrive from a background task
milliseconds later. With a build-once menu, a cache miss is permanent for that
open — the user gets a "loading" placeholder that never resolves and has to
close and reopen the menu. It makes a *cache* load-bearing for correctness,
which is exactly backwards: prefetching should buy latency, never content.

**Current implementation:** Ferail creates the Open-With submenu immediately
with a loading row, keeps a weak handle to that entity, and calls
`PopupMenu::rebuild` when the off-thread association lookup completes. The
delegate cache is updated first so displayed slot N and dispatched slot N stay
identical. There is no per-frame polling, `menu_revision`, or root-menu
replacement anymore.

The small element wrapper in `multi_table/context_menu.rs` remains only for a
separate Windows rule: Shift+right-click belongs exclusively to the isolated
native extended Shell menu. It no longer owns dynamic-content behavior.

**Remaining upstream gap:** #2797 still applies to dynamic *root* menu content,
but Ferail currently has no such need. The submenu API solves our real case.

## 5. `img` can't be rotated/transformed (only `svg` can)

**Hit during:** viewer per-item rotate feature (pictures + videos).

gpui's `svg` element has `.with_transformation(Transformation::rotate(..))`,
but `img` (`Img`) exposes no rotation/transform — there's no element-level
transform on the image path at all. So a "rotate this photo 90°" view feature
can't just transform the element; it has to rotate the underlying pixel buffer
(`RenderImage`) on the CPU and hand gpui a new image.

**Workaround:** rotate the decoded RGBA buffer with `image::imageops::rotate*`
and cache the rotated `RenderImage` per (item, orientation); swap stage width/
height for 90°/270°. Since the video viewer now pulls frames into
`RenderImage`s too (see below / VIEWER.md), video rotates by the *same* CPU
path as stills — no separate layer-transform code.

**What upstream could do:**
- Give `Img` the same `with_transformation` / rotation support `svg` already
  has, so 90° view-only rotation is a render-time transform, not a re-encode.

### 5a. A layer transform doesn't move AppKit hit-testing — RESOLVED

*Resolved 2026-06-18 by switching video off the native overlay.* The original
video design floated an `AVPlayerView` over the gpui window and rotated its
layer; that rotated the pixels but **not** the view's hit region, so native
controls rendered rotated yet weren't clickable (we hid them and drew a gpui
transport). Rather than keep fighting AppKit hit-testing under gpui chrome, the
viewer now drives a windowless `AVPlayer` and pulls frames into `RenderImage`s,
so the video is an ordinary gpui `img`: no overlay, no foreign hit region, and
the transport/seek-bar hit-test normally at any rotation. The lesson stands for
anyone tempted to overlay an interactive native view under gpui chrome —
prefer pulling its content into a gpui element if a frame source exists.

### 5b. objc2 encoding checks need exact struct names for CF/CM types

Passing/returning `CMTime` and CoreVideo pixel buffers through `msg_send!`
requires hand-rolled `Encode`/`RefEncode` impls whose struct **name** matches
what the runtime reports, or objc2's verification aborts: `CMTime` is reported
anonymous (`{?=qiIq}` — name must be `"?"`, not `"CMTime"`); `CMTime` is also a
pointer arg (`itemTimeForDisplay:` out-param), needing a `RefEncode` whose
pointee encoding matches; and `copyPixelBufferForItemTime:` *returns*
`^{__CVBuffer=}`, so a bare `*mut c_void` (`^v`) is rejected — the return must
type as `*mut CVBuffer` where `CVBuffer`'s `RefEncode` names the struct
`"__CVBuffer"`. objc2's check is a genuine safety net (it caught each before it
became a memory bug), but a small `objc2-core-media` / `objc2-core-video`
dependency, or ready-made `CMTime` / `CVPixelBuffer` types, would remove the
guesswork.

## 6. TextView preview scroll cluster — bounded-box vs `scrollable(true)`

**Hit during:** preview-pane polish (horizontal scroll, wheel containment,
sluggish scroll while text is selected).

`TextView` has two modes: `scrollable(false)` (our usage) expands to full
content height with no scrollbar — we bound it in our own
`max_h + overflow_scroll` box; `scrollable(true)` virtualizes via an internal
uniform_list with its own *vertical-only* scrollbar but "requires the parent
to have a fixed height." Neither cleanly gives all three of: horizontal scroll
for no-wrap code, nested-wheel containment (scroll the box first, bubble to the
outer pane only at the boundary), and cheap scrolling while a selection is
active (full-content mode re-lays-out the whole document per frame).

**Status:** under investigation; needs runtime A/B on the bumped build before
committing a direction (the fix may be ours, the bump's TextView scrollbar
rework, or a mix). The per-file element-id fix for selection bleed across
previews is already in.

**What upstream could do:**
- A `scrollable` mode that supports both axes (horizontal for code), or lets
  the host own the scroll container while TextView still virtualizes rows.
- Document/expose wheel scroll-chaining behavior so nested scroll containers
  bubble at the boundary predictably.

## 7. Windows `Window::render_to_image` — RESOLVED upstream

**Merged 2026-08-26:** [zed#63012](https://github.com/zed-industries/zed/pull/63012).

The original Windows backend had no offscreen readback, so Ferail's hidden
`--screenshot` window could not use the same `Window::render_to_image` path as
macOS. The merged implementation renders into the existing DirectX target,
copies through a CPU-readable staging texture, and converts BGRA to RGBA without
showing the window.

Ferail now pins Zed `f66ed399`, which contains the merged change. Our vendored
`gpui_windows` is based on that exact revision and carries only the separate
outbound Shell/OLE drag delta documented in its README; it does not carry a
second render-to-image implementation. The historical patch file remains only
as review provenance and is not applied by Cargo.

The macOS host can compile the ordinary workspace but cannot complete the
Windows GPUI build-script without the Windows resource compiler/SDK. Native
Windows CI or the Windows development box must therefore keep the no-flash
acceptance check: hidden window, nonblank PNG, no taskbar/Alt-Tab entry. The
old `PrintWindow` implementation remains emergency/debug fallback code, not the
normal screenshot path.

## 8. gpui grew a *direct* GPL-3.0 `ztracing` dependency

**Hit during:** the 2026-08-08 bump to zed `38ca9106` (edge added upstream in
`00cba838a`, 2026-08-05).

Until then the only GPL reach was `gpui → sum_tree → ztracing`, severable by
forking `sum_tree` (the old `vendor/sum-tree`). With `ztracing` in `gpui`'s
own `[dependencies]`, that fork stopped being sufficient — and forking gpui
itself is not a maintainable option.

**Workaround:** patch `ztracing` at the source root with a clean-room
MIT/Apache no-op stub, [`vendor/ztracing`](../vendor/ztracing/README.md).
Outside Zed's `--cfg ztracing` profiling builds the real crate is pure no-ops,
so the stub is behaviourally identical; it also drops GPL `ztracing_macro` and
`zlog` from the graph and retired the `sum_tree` fork (no more per-bump
re-sync). Instrumentation edges may keep spreading through zed's crates; the
stub covers all of them at once, but a bump that fails on an unresolved
`ztracing::…` item means upstream grew the API — add the missing name to the
stub as a no-op.

**What upstream could do:** relicense the tracing shim permissively (it is
~60 lines of no-op glue outside profiling builds), or gate it behind an
optional feature default-off. Tracked upstream as zed#55470 (acknowledged,
stuck in legal).

## 9. External file drag-out finally exists — via `external_drag_payload` (zed #58161)

**Hit during:** "drag to Finder does nothing" investigation, fixed with the
2026-08-08 bump; then the same symptom on Windows, fixed locally 2026-08-25.

Not a complaint — an API note. gpui's `on_drag` is purely in-window (the app
paints its own ghost; nothing reaches the OS). Dragging `ExternalPaths` as
the *value* does *not* make it a native drag — a misreading we shipped for
seven weeks. Real drag-out requires chaining
`.external_drag_payload::<T>(resolver)` after `.on_drag(...)`: when the
pointer leaves the viewport, gpui calls the resolver (UI thread — keep it
allocation-cheap and I/O-free; we feed it cached `EntryKind` dir-ness) and
asks the platform backend to promote the gesture to a native drag. The
platform then draws per-type file icons and the in-window ghost hands off.
Payloads are real on-disk paths only (`ExternalDragPayload::Files`) — nothing
exists yet for promise-based/deferred content. Ferail implements archive
member drag-out directly with `NSFilePromiseProvider` on macOS; see #11 for
the extra cross-window handoff this requires.

The GPUI core contract is cross-platform, but the pinned Windows backend —
and upstream `main` when checked on 2026-08-24 — leaves
`can_start_external_drag`/`start_external_drag` at their default `false`.
Ferail therefore carries a narrow `gpui_windows` patch: absolute PIDLs feed
`SHCreateDataObject`, then `SHDoDragDrop` runs a normal OLE file drag with
copy/move/link effects. That synchronous modal loop **must not** start inside
GPUI's input callback: it pumps messages while `App` is still mutably borrowed
and a timer or paint can panic with `RefCell already borrowed`. A private
window message defers it until the callback unwinds. Once the pointer first
leaves the source, the Shell image remains the sole visual owner until the OLE
session ends, including after re-entry. GPUI still restores the typed payload
so Ferail's own drop targets work; a shared atomic flag makes its restored drag
badge render empty instead of duplicating the Shell image. OLE `DragOver` can
arrive at the mouse report rate, so the Windows backend forwards positions into
GPUI at most every 8 ms while effect negotiation and the native helper still
run for every callback. The OLE return path always emits
`FileDropEvent::Ended`, including cancellation and failure, so
`platform_owned_drag` cannot remain suspended indefinitely.

## 10. Drag-out operation mask is hardcoded to Copy — no move, no modifiers

**Raised upstream 2026-08-21:** [zed discussion #63013](https://github.com/zed-industries/zed/discussions/63013) (their tracker routes feature requests to Discussions; posted in the "Zed GPUI" category, offering the PR once the API shape is agreed).

**Hit during:** "drag out only copies; can a modifier make it a move?" —
follow-up to #9.

`gpui_macos`'s `NSDraggingSource` returns `NSDragOperationCopy` for the
outside-application dragging context, hardcoded
(`dragging_session_source_operation_mask`). Consequences: external drops can
only copy (a same-volume drop in Finder that should default to *move*
copies instead), ⌥ / ⌘ / ⌃ change nothing (AppKit's modifier filtering ANDs
against the source mask, and Move/Link aren't in it), and the system badge
row (green “+” for copy, curved arrow for alias) never varies. There is no
gpui API to widen the mask.

**Workaround:** `ferail_shell_mac::install_native_drag_operations()` —
runtime `class_replaceMethod` on `GPUIWindow` / `GPUIPanel` (registered by a
`#[ctor]` in gpui_macos, so they always exist) swapping in a mask of
Copy | Link | Generic | Move for the outside context, keeping upstream's
Copy | Move within the app. Destination + standard modifier semantics then
just work. Called from `boot.rs` after window open; degrades silently to
copy-only if upstream renames the classes (a boot log warns, and a
`boot::tests` unit test fails on rename).

**What upstream could do:** carry allowed operations on
`ExternalDragPayload` (e.g. `Files { paths, operations }`) and return them
from the mask callback — the source knows whether its items are movable;
a file manager's are, a text snippet's usually aren't.

**Esc-cancel (same friction, second symptom):** once promoted, the session
can't be cancelled either — gpui exposes no hook, and AppKit itself has no
public "cancel this `NSDraggingSession`" API. The same
`install_native_drag_operations()` pass therefore also adds
`draggingSession:willBeginAtPoint:` and wraps
`…endedAtPoint:operation:` (chaining gpui's original) to track session
liveness, and `cancel_native_drag()` cancels by the one lever a source
owns: collapse the mask to `None`, force a destination re-query with a
synthetic 1-px drag, then end the gesture with a delayed synthetic
mouse-up — AppKit resolves that as a failed drag and animates the items
back. The Shell's global Esc keystroke observer routes to it when
`has_active_drag()` is false but a native session is live. Upstream could
expose a `Window::cancel_external_drag()` (and gpui could bind Esc itself,
matching Finder).

## 11. Native file promises are rejected by GPUI drop destinations

**Hit during:** archive-member drag-out from a standalone workbench.

Two layers reject it. First, `gpui_macos` registers each window with
`registerForDraggedTypes: @[NSFilenamesPboardType]` only. AppKit sends
drag-destination callbacks to a window only when the session pasteboard
carries a registered type, and `NSFilePromiseProvider` never writes the
legacy filename list (only `com.apple.pasteboard.promised-file-*`,
`com.apple.NSFilePromiseItemMetaData`, `Apple files promise pasteboard type`),
so for a promised-file drag a GPUI window is simply not a destination: no
`draggingEntered:`, no `draggingUpdated:`, no `performDragOperation:` — the
drop lands nowhere while Finder, which registers for the promise types, works.
This was the actual "drag to Ferail does nothing, drag to Finder works" bug;
no amount of pasteboard marking or callback shimming can help a window AppKit
never calls. Second, even when called, `gpui_macos::dragging_entered`
recognizes a native file drag only when `NSFilenamesPboardType` yields a
property list, which a promise cannot provide by design, so GPUI returns
`NSDragOperationNone` before its own drop handlers can see the gesture. Its
platform-owned internal-drag state also restores only into the source window;
another GPUI window receives `ExternalPaths`, which cannot represent archive
coordinates.

**Workaround:** Ferail's promise items are an `NSFilePromiseProvider`
*subclass* that additionally declares a private, data-free marker type (the
documented way to add types to a promised item; Finder ignores it). At drag
start, before `beginDraggingSession`, Ferail calls `registerForDraggedTypes:`
on every `GPUIWindow`/`GPUIPanel` with that marker plus the legacy filename
type (registration accumulates, but passing both keeps Finder→Ferail drops
safe either way). That makes AppKit route the gesture to GPUI's window class,
where a narrow `draggingEntered:` shim recognizes Ferail's own promise session
(an in-process flag raised before the session starts; the marker is the
fallback) and admits it without calling GPUI's legacy filename parser; every
ordinary drag still chains to GPUI unchanged. GPUI's existing Updated/Submit
callbacks then drive the retained in-process `ArchiveEntryDrag`, so another
Ferail window consumes the archive coordinates while Finder consumes the
promises. A foreground timer observes only the atomic native-session flag and
clears the retained drag when AppKit ends. Escape cancels both halves.

There is a second required shim: GPUI's `FileDropEvent::Exited` takes and drops
`active_drag` unless it previously moved that value into its private
platform-owned slot. A directly started AppKit promise session never enters
that slot. Ferail therefore ignores `draggingExited:` only for its marked
archive promises, preserving the payload across window edges; ordinary drags
still chain to GPUI. MouseUp on a successful drop, session-end cleanup, or
Escape remains the terminal owner.

Do not emulate this with an empty `NSFilenamesPboardType` array: GPUI's Cocoa
`NSFastIterator` dereferences the first item without accepting an empty list,
which aborts the process in `dragging_entered` before any drop handler runs.

**What upstream could do:** register its windows for the modern file-promise
pasteboard types as well as `NSFilenamesPboardType`, accept
`NSFilePromiseReceiver` / those types as file drags, and expose a deferred
external payload API.
For same-process cross-window drags, restore the original typed payload in any
GPUI destination window rather than only the source window.

## 12. gpui-component `Input` paint leaked strong handles — upstream shape fixed, teardown containment retained

**Hit during:** the 2026-08-24 Windows session, chasing the 0.6.5 tester's
"crash on quit" reports (`Exited with leaked handles: … InputState`).

Two stacked problems:

1. **gpui-component:** every paint of an `Input` element registers a
   `window.handle_input(...)` **and** a `window.on_next_frame(...)` callback,
   both capturing a **strong** `Entity<InputState>` clone
   (`crates/ui/src/input/element.rs`, the `Root.focused_input` dance). In a
   single-window app the next frame consumes the queue and teardown drops the
   rest. With a **second window open** (Get Info), the handles accumulate —
   deterministic repro: `--screenshot --properties` leaks one `InputState`
   handle per paint of the main window's filter input (27 in a 3 s run),
   tripping gpui's leak assertion at quit. The user-visible form: quitting
   after normal use exits 101 with a leaked-handles crash report.
2. **Feature packaging:** gpui's `leak-detection` rides along with
   `test-support`, which we enabled workspace-wide for `render_to_image`
   (item 7). That shipped the **diagnostic assert to end users** — the 0.6.5
   tester's four "crash" files were exactly this assert.

**Workaround:** `ferail-gpui` now owns a default-on `screenshot-harness`
feature that forwards `gpui/test-support`; the packaging scripts build with
`-p ferail-gpui --no-default-features` (the `-p` is load-bearing — from the
virtual workspace root cargo silently ignores `--no-default-features`).
Dev and `cargo test` keep the leak detector; users never see the assert, and
`--screenshot` still works in packaged Windows builds via the PrintWindow
fallback. The real strong-capture leak remains upstream; our own subscription
cycles of the same class were fixed separately (`00aefe9`).

Ferail also drains the bounded next-frame callback queue while each dev/test
window is still alive. Screenshot mode, which calls `App::quit` without a
native close, additionally removes its windows before quitting so the Root and
current input handler are released before gpui checks the entity map. The exact
`--screenshot --properties` repro first drained 76 callbacks but still leaked
the Root-held `InputState`; with the explicit window removal it exits 0. This
makes the harness deterministic, but does not remove gpui-component's strong
captures during normal rendering. Ferail's app-level Quit action uses this
same dev/test cleanup and waits one event-loop turn; it previously called
`cx.quit()` directly and bypassed every window's `should_close` hook.

**2026-08-26 follow-up:** draining callbacks alone was incomplete for a native
window close. If an Input retained keyboard focus (the main filter is the
deterministic case), the Windows platform IME/input handler still owned one
strong `Entity<InputState>`. After that was retired, a second report exposed
the same ordering problem in the final frame's element/listener graph as one
leaked `Entity<PopupMenu>`. Dev/test teardown is therefore deliberately
generic: disable focus and draw the real component root, drain its callbacks
while `Root` still exists, replace it with an inert root, draw again to discard
all old-frame input handlers, element state, listeners and overlays, then
remove the window. On Windows the harness also uses explicit last-window quit
and waits one foreground turn, ensuring the removed Window box is dropped
before the entity-map assertion. Repros with a focused edited filter, preview,
and a context menu still open all exit 0 under `LEAK_BACKTRACE=1`. This remains
teardown containment for upstream strong captures, not a claim that their
normal-render ownership disappeared.

**2026-08-28 migration follow-up (`gpui-component` `e8f54eb`):** the old
per-paint `window.on_next_frame` closure is gone. Input kinds now share the
`gpui-base` engine, and the UI layer synchronizes `Root::focused_input` only
with the actual focus state. Ferail's deterministic
`LEAK_BACKTRACE=1 --screenshot --properties` reproduction exits cleanly after
the bump, as does the full workspace test suite. We retain the generic
focus/root/frame teardown sequence because it also protects current overlay and
platform-handler ownership, and it must be revalidated on a native Windows
close before any simplification. There is no upstream PR candidate here unless
a new minimal reproducer demonstrates a remaining framework leak.

**What upstream could do:** capture `WeakEntity<InputState>` in the
`on_next_frame` reset closure and in `Root.focused_input`, or drain
`next_frame_callbacks` on window teardown; and consider splitting
`leak-detection` out of `test-support` so `render_to_image` doesn't drag the
exit assert into production builds.

<!-- Add new findings above this line as the bump surfaces them. -->
