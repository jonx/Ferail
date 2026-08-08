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

## 4b. `context_menu` builds once and can never refresh its contents

**Hit during:** "Open With" arriving empty on the first right-click of a row
(docs/features/CONTEXT_MENU.md).

`ContextMenuExt::context_menu` runs its builder exactly once, from a
`window.defer` scheduled inside its mouse-down listener, and then holds the
resulting `Entity<PopupMenu>` in private element state. `PopupMenu`'s items are
`pub(crate)` and every mutator is builder-style (`mut self -> Self`), so an
already-built menu can be neither rebuilt nor edited from outside the crate.

That is fine for menus made of static labels, and wrong for a file manager. Our
menu content includes data that is *illegal* to fetch on the UI thread (the
Prime Directive): LaunchServices "Open With" candidates, per-row capabilities,
anything that touches the shell. Those arrive from a background task
milliseconds later. With a build-once menu, a cache miss is permanent for that
open — the user gets a "loading" placeholder that never resolves and has to
close and reopen the menu. It makes a *cache* load-bearing for correctness,
which is exactly backwards: prefetching should buy latency, never content.

**Workaround:** a second small fork,
`crates/ferail-gpui/src/multi_table/context_menu.rs` — upstream's element
plus a `revision: impl Fn(&App) -> u64` closure polled each frame while the
menu is open. When the value changes, the builder re-runs and the open menu is
replaced in place. `TableDelegate::context_menu_revision` plumbs it to the
delegate; `FileListDelegate::menu_revision` ticks when the off-thread Open With
fetch reports back. Cost: the rebuild resets the menu's hover/keyboard
highlight, so revisions must only tick on genuine content changes.

Second-order cost worth remembering: the rebuild made a latent
`TableState::set_selected_row` behavior fatal — it clears `right_clicked_row`,
and the Shell's lead mirror calls it right after every right-click, so a
rebuild found no row and produced an *empty* menu (popup vanished mid-use).
Hence the fork's `mirror_lead_row`, which mirrors the lead without pretending a
click happened.

**What upstream could do:**
- Let the builder be re-run: `.refresh_on(impl Fn(&App) -> u64)`, or a handle
  to the built `Entity<PopupMenu>` so consumers can rebuild it themselves.
- Or make `PopupMenu` mutable after construction (`pub fn set_items`), which
  would also serve async submenus.
- Longer term, first-class support for a menu item whose content resolves
  asynchronously — every OS file manager needs it for "Open With".

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

## 7. `gpui_windows` has no `Window::render_to_image` — headless capture needs a workaround

**Hit during:** Windows port — the `--screenshot` CLI harness
(`ferail-gpui/src/screenshot.rs`).

`gpui_macos` implements `Window::render_to_image` (MetalRenderer samples an
offscreen target, so a *hidden* window can be captured). `gpui_windows` does
**not** implement it — the method returns `Err("render_to_image not implemented
for this platform")`. So the unified `capture_window()` the macOS side wrote
(assuming both platforms had `render_to_image`) panics on Windows. A stale code
comment referenced a `gpui-windows-render-to-image` `[patch]` that was never
actually wired into the root `Cargo.toml`.

**IMPLEMENTED (local patch, PR-ready).** We implemented `render_to_image` in
`gpui_windows` and run against it via a `[patch]` to a local zed clone:

- `DirectXRenderer::render_to_image(scene, bg)` — draws the scene into the
  existing render target (created at window construction, so no window need be
  shown), copies it into a `D3D11_USAGE_STAGING` texture, `Map`s it, and
  converts BGRA → RGBA — the Windows analogue of `gpui_macos`'s offscreen Metal
  path. The batch loop is factored into a shared `draw_batches` so `draw`
  (present) and `render_to_image` (readback) stay in sync. Gated on
  `cfg(any(test, feature = "test-support"))` to match the trait method.
- `WindowsWindow::render_to_image` overrides the `PlatformWindow` trait default.
- The diff is captured at [`patches/gpui-windows-render-to-image.patch`](../patches/gpui-windows-render-to-image.patch)
  — ready to open as a zed PR. Once merged upstream, bump the gpui rev in the
  root `Cargo.toml` and drop the `[patch]` + the PrintWindow fallback.

With the patch active, the screenshot harness opens the window with
`show: false` and captures via `render_to_image` — **truly headless, no flash**.
The PrintWindow path (`ferail_shell_win32::capture_window_rgba` + off-screen
move) remains as the fallback for builds without the patch.

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
2026-08-08 bump.

Not a complaint — an API note. gpui's `on_drag` is purely in-window (the app
paints its own ghost; nothing reaches the OS). Dragging `ExternalPaths` as
the *value* does *not* make it a native drag — a misreading we shipped for
seven weeks. Real drag-out requires chaining
`.external_drag_payload::<T>(resolver)` after `.on_drag(...)`: when the
pointer leaves the viewport, gpui calls the resolver (UI thread — keep it
allocation-cheap and I/O-free; we feed it cached `EntryKind` dir-ness) and
promotes to a native `NSDraggingSession` / Wayland drag. The platform then
draws per-type file icons and the in-window ghost hands off. Payloads are
real on-disk paths only (`ExternalDragPayload::Files`) — nothing exists yet
for promise-based/deferred content (our archive-entry drags stay in-window).

## 10. Drag-out operation mask is hardcoded to Copy — no move, no modifiers

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

<!-- Add new findings above this line as the bump surfaces them. -->
