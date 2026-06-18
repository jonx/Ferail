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
bump (the fork is `crates/feraille-gpui/src/multi_table/`).

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

<!-- Add new findings above this line as the bump surfaces them. -->
