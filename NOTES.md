# Feraille — Architecture and Decision Log

This file is updated **as decisions land**, in my voice, per the slow-ai
method. Not reconstructed at the end.

## Architecture at a glance

- Workspace of 7 crates: `feraille-{core, design, render, controls, fs-native, shell-win32, app}`. Direction of dependency is one-way: `app → controls → {design, render}`; `app → core → fs-native`; `shell-*` sits behind the `FsBackend` trait in `core`.
- Soft renderer (CPU rasterization via softbuffer + ab_glyph) is the only renderer for now. GPU backend (wgpu/Vello) is iter-3 — clean swap behind the existing `Renderer` trait.
- Design tokens are the *only* source of visual values; no raw colors or pixel literals outside `feraille-design`.
- Specs in `specs/controls/` and `specs/ux/` are the contract; controls implement them. Specs are re-evaluated each iteration.
- macOS is the v1 target. Windows lands in iter-4+ behind the same `FsBackend` and `ContextMenu`/`DragDrop` traits.

## Key decisions

### Iter-2 scope: maximal
Chose maximal scope: kill spec violations + Zed-aligned tokens + real $HOME enumeration + Sidebar + Splitter + TabStrip + BreadcrumbBar + minimal FileTree. Slow-AI four gates pushed back on size; accepted the stretch with a mandatory checkpoint after the first 10 steps. If frame budget or scope feels strained at the gate, we ship as iter-2 and push tabs/tree to iter-2.5.

### Soft renderer kept; GPU deferred
Glyph caching gets the soft renderer to 60fps on real workloads. wgpu/Vello is a foundation choice that adds no features in iter-2; lands in iter-3 as a single-crate swap behind `Renderer`.

### Sidebar is transient
Sidebar control built in step 9, replaced by `FileTree` in step 15. ~140 LOC of deliberate scaffolding so the binary keeps running while tabs/breadcrumb/tree land. Acceptable because the alternative (skip Sidebar, build FileTree first) blocks every intermediate gate.

### Skipped step 8: Label + Button as separate primitives
Plan called for `Label` + `Button(Ghost)` primitives. On reaching the step I asked: does anything *use* them? Status-bar text already works with `draw_text` directly; sidebar rows paint inline because they need their own hover/click logic; tree rows won't be Button instances (they're virtualized). Per the slow-AI "no abstractions until they pay back" rule: skipped. If breadcrumb's edit-mode needs a submit button in step 13, I'll extract one there.

### Iter-2 shipped (final tally)

All 17 planned steps closed:

- **Tokens** rewritten Zed-style (steps 1, 17): two fg tiers, single accent, sharp corners, four bg layers.
- **Renderer** got a glyph cache (step 2): rasterize each (GlyphId, px_size_q8) once, FIFO-evict at 2048.
- **Selection** model with anchor/cursor/Range/Discrete (step 3); 7 unit tests.
- **Real FS enumeration** in `feraille-fs-native` with humanized sizes/mtimes (step 4); 4 tests.
- **VirtualizedList** rewritten for `&[FileEntry]` API + FocusRing overlay layer (step 5).
- **Scrollbar, Splitter, FocusRing** primitives (steps 3, 6, 7); Scrollbar has 4 tests, Splitter has 2.
- **Sidebar** built and then deprecated in favor of FileTree (steps 9, 15).
- **App v1** working real-data browser (step 10). Checkpoint passed.
- **BreadcrumbBar** read-only with hover (step 13).
- **TabStrip** with activate/close/new (step 14).
- **FileTree** with lazy children, expand/collapse, virtualized rows (step 15).
- **App v2** full layout (step 16).

### Steps where I cut scope

- **Step 8 (Label + Button)** — skipped entirely; status-bar text uses `draw_text` directly, sidebar paints rows inline. No primitive earned its keep.
- **Step 12 (TextInput)** — skipped; Breadcrumb's edit mode (the only iter-2 caller) slid to iter-2.5 alongside SearchBox where text input pays back.
- **VirtualizedList API** — kept `&[FileEntry]` direct rather than the spec's `Item` generic. Generic-over-Item lands when TabStrip needs the same primitive (it doesn't — TabStrip has its own paint).
- **Mouse range/marquee selection** — defer to iter-3. Single-click replaces; keyboard handles range/discrete.

### What landed differently from the original plan

- Plan called for a `stroke_rect_inset` trait method. Reading the existing impl, `stroke_rect` was already inset (top/bottom/left/right paint inside the rect bounds). Removed the trait method from scope — saved an API change.
- Plan put Breadcrumb's height as 32 DIPs and TabStrip's at 32 DIPs; matched. App layout: header (40) / tabstrip (32) / breadcrumb (32) / status (24). Total chrome 128 DIPs out of 760-DIP default window — content-rich.

### Iter-2.5 shipped

- **Tree click UX fix.** Iter-2 required clicking the small chevron to expand a folder; the user found this immediately. Iter-2.5 makes a row click both navigate and expand (Finder/Explorer convention); chevron-only retains the expand-without-navigating affordance for users who want it.
- **TextInput primitive.** Single-line edit, cursor + Backspace/Delete/arrows/Home/End/Enter/Esc, paste via host. No selection, no IME, no caret blink — those land in iter-3 with the macOS shell crate. 4 unit tests.
- **Breadcrumb edit mode.** Ctrl+L / Cmd+L pre-fills the current path and gives focus to the inline TextInput. Enter navigates (with `~` expansion), Esc reverts. While editing, all keyboard text routes to the breadcrumb instead of the file pane.
- **Modifier tracking.** Added `ModifiersChanged` handler so Ctrl+L / Cmd+L can be detected reliably across macOS dev mode and (future) Windows builds.

### Iter-2.5 known gaps

- TextInput uses `approx_text_width` (chars × size × 0.55) for caret positioning. Wrong for any non-monospace font with variable-width characters; wider letters drift the caret. Real measurement requires the renderer; deferring to iter-3 (`Renderer::measure_text` already exists).
- Tab completion in the path edit field — not implemented; user types the full path. Iter-3 with `FsBackend::complete()`.
- Tree doesn't auto-reveal the current path after navigating from breadcrumb / file-pane Enter. Visible only if the user expanded that branch via the tree itself. Iter-3 with `tree.reveal_path(path)`.

### Iter-2.6 shipped — headless screenshot CLI

Added `--screenshot` mode to the binary so I (and any future contributor) can render the UI to PNG without opening a window. State is driven via flags (`--navigate`, `--expand`, `--select-name`, `--splitter`, `--scroll`, `--theme`, `--edit-mode`, etc.). Mouse drags and animations skipped by design — every state worth screenshotting can be set declaratively.

Implementation:
- New `crates/feraille-app/src/screenshot.rs` (~250 LOC: hand-rolled arg parser + headless render flow + PNG encode via the `png` crate).
- `App::render` refactored to take/replace its `SoftRenderer` and `softbuffer::Surface` so a separate `App::paint_to(&mut dyn Renderer)` can be called in the GUI path or headlessly. No more interleaving of paint and present.
- `App` got a focused `pub` API of state setters: `new_for_headless`, `set_dimensions`, `new_tab_at`, `switch_to_tab`, `set_splitter`, `set_scroll`, `select_row`, `select_name`, `enter_breadcrumb_edit_mode`. Internal control state (`tabs`, `tree`, etc.) became `pub` for the same reason — same-binary access only, not exported.

What it caught immediately:
- **Scrollbar thumb was painted in `fg.disabled` (#B0B0B0)** — invisible against the white file pane. Bumped to `fg.secondary`. I would not have noticed without the screenshot tool.
- Tree chevrons used U+25B8/U+25BE (the *small* triangles) which Arial doesn't ship — switched to U+25B6/U+25BC. Confirmed visible in the screenshot.

Sample screenshots were taken to /tmp/feraille-{home,fixed,light-selected,dark,tree}.png during dev.

### Iter-2.7 shipped — chrome trim + tree reveal + perf fix

- **Removed the standalone header.** The 40-DIP "current path" strip duplicated the breadcrumb. Tabstrip is now the topmost element below the OS title bar — Files App / Finder convention. Saves vertical chrome and reads cleaner in screenshots.
- **`App.navigate(path)` now reveals the path in the tree.** Walks the chain of ancestors from the appropriate root (Home or `/Volumes/<x>`), calling `populate_children` on each that hasn't been loaded yet, then selects + auto-scrolls to the leaf. Closes the iter-2.5 known gap.
- **Perf fix on tree re-reveal.** First version of `reveal_in_tree` always re-enumerated every ancestor — fine the first time, slow on every subsequent navigation that crossed cached ancestors. Now skips `fs.enumerate` when `tree.is_loaded(id)` is already true. Catches the user's observation that "folding is instant but not unfolding": with this fix, re-unfold of any cached folder is instant; only the *first* expand of a folder pays I/O cost.
- **`FileTree::ensure_visible(id, viewport_h)`** added — auto-scrolls the tree to keep a selected node visible, mirroring `VirtualizedList`.
- **`FileTree::is_loaded` and `contains` accessors** for the host's reveal logic.

Verified via headless screenshot: navigating to `~/Source/Feraille` now shows the full chain expanded in the tree with Feraille selected and visible in the tree's viewport.

### Iter-2.7.1 — fold redraw bug

User report: "fold is slow, ~1 second; unfold is fast." Misdiagnosed at first as a perf issue; actually a redraw bug.

`FileTree::toggle_expand` returns `None` when collapsing an already-expanded folder (state mutated, no event for host). My host-side click handler had:

```rust
if !tree_events.is_empty() {
    self.request_redraw();
}
```

Empty Vec → no redraw. The fold *did* happen in tree state, but the screen showed stale (still-expanded) state until the next event (mouse move, etc.). User experienced this as "1 second of slow," but it was actually a missing redraw, period.

Fix: `tree.click()` now returns `Option<Vec<TreeEvent>>`. `None` = missed all rows (fall through). `Some(events)` = hit a row, redraw needed even if events is empty. The empty-Vec case used to mean "missed"; now it correctly means "handled, no events for you."

Same class of bug existed for cached-unfold (chevron toggle on a previously-loaded folder also returns None) but the user hit it less because most unfolds are first-time and trigger an `ExpandRequested` that *does* drive a redraw.

The right takeaway: anywhere we have "control mutated state but emitted no event," the API has to signal that distinctly — empty event vec is overloaded otherwise.

### Iter-3.1 — table-stakes file-explorer actions

The app was a viewer until iter-3. iter-3.1 adds the four keystrokes that turn it into something you'd actually use:

- **Enter on a file** — `feraille_fs_native::open_with_default` shells out to `open` (macOS), `cmd /C start` (Windows), `xdg-open` (Linux). Enter on a directory still navigates.
- **F5** — `App::refresh_active_tab` re-enumerates the current folder, preserving the cursor on the same entry name when possible. Also invalidates the tree's cached children for that node so a future expand re-fetches.
- **Ctrl+H or Cmd+Shift+.** — toggles `App::show_hidden`. The hidden filter applies at the call sites of `fs.enumerate` (file pane + tree) so re-toggling refreshes both.
- **Delete** — `feraille_fs_native::move_to_trash` renames the file into `~/.Trash` with collision suffixing (`name 2`, `name 3`, …). Falls back to copy+remove on cross-volume errors. Refuses on non-macOS for now (iter-4 with the macOS shell crate replaces the rename approach with `NSWorkspace.recycle`).

Architectural note: `move_to_trash` deliberately doesn't go through Cocoa yet — bridging `NSWorkspace` lives behind the macOS shell crate boundary that lands in iter-4. The rename-to-`~/.Trash` approach is what the OS does for files within the boot volume; it's not what Trash actually wants for cross-volume files (those need `NSURL.trash`), but iter-3 doesn't need the perfect answer.

Slow-AI receipts: this iter consciously *did not* add the F2 inline-rename, copy-path-to-clipboard, or new-folder actions. Each requires either a rename overlay over the list row (significant new infrastructure) or a clipboard dependency. Both pay back later — they don't pay back yet.

### Iter-3.2 — file-type icon hue + window title sync

Two small polish wins. Both visible without any input:

- `icon_color_for_file` keys icon color off the extension. Categories chosen for visual scanning, not file-system semantics: code purple, image green, media pink, archive orange, data/config cyan, document blue, default gray. Inline match arms — drop new extensions into the obvious one. No new tokens; these are file-type signals not theme colors.
- `App::sync_window_title` sets the OS title bar to `<folder> — Feraille` on every `navigate` / `switch_tab`. Cmd+Tab now shows the active folder.

### Iter-3.3 — list-row hover + ant trail (in-memory)

User said "continue" — pushing iter-3 further.

- **List-row hover.** `VirtualizedList::hover: Option<usize>` + `update_hover(bounds, point, count)` mirroring the tree/sidebar pattern. Hovered row paints `bg.layer3` underneath when the row isn't already selected. Wired from `CursorMoved` in main.rs alongside the existing tree/breadcrumb/tabstrip hover updates.
- **Ant trail.** Iconic Ferail feature, ported in spirit. `feraille_core::AntTrail`: a `HashMap<NodeId, u32>` of visit counts plus a max. `record(id)` increments; `heat(id) -> 0.0..=1.0` returns log-scaled normalized intensity. `App.navigate` records every visit; `FileTree::paint` now takes a `heat_for: impl Fn(NodeId) -> f32` closure and paints a 2-DIP cyan strip on the left edge of each row whose alpha is modulated by heat.
- **Persistence**: deliberately not yet — `iter-6` ports Ferail's SQLite store. iter-3 keeps it in-memory so the trail resets on every launch. That's a feature for the demo (every session starts clean) and a known gap for shipping (you lose your patterns).

Verified in `/tmp/feraille-trail.png`: visited 9 paths total in the script, with Source 3× and Feraille 5×. The strip is brightest on Feraille, softer on Source, faint on jkn and Documents. Subtle enough to not interfere with normal scanning.

### Iter-3.4 — trackpad scroll routing

User asked for trackpad support. Discovered the existing `MouseWheel` handler already accepts `MouseScrollDelta::PixelDelta` (trackpad / high-precision wheels) — the actual bug was that all scroll events routed to the file list regardless of where the pointer was. Trackpad-scrolling over the tree was a no-op.

Fix: route scroll events based on `pointer_dips`. If the pointer is over the tree pane, the tree scrolls; if over the list pane, the list scrolls; otherwise (breadcrumb, tabstrip, status bar) it's a no-op. Matches Finder behavior.

Also documented the sign convention inline (positive winit delta = "forward" / content moves up; we invert because `scroll_offset` is "DIPs below origin"). winit's `PixelDelta` on macOS is already in logical pixels, so no DPI math is needed.

Other touchpad gestures (`TouchpadMagnify`, `TouchpadPressure`, `SmartMagnify`) are unused for now. Pinch-zoom is most plausibly a treemap interaction (iter-7); force-touch could trigger Quick Look preview when the macOS shell crate lands.

### Iter-3.5 — column system (Name / Size / Kind / Modified) + click-to-sort

User asked for "more columns like Ferail." Ferail had six (Name, Size, Type, Modified, Magic, Description); the last two depend on the magic detector that ports in iter-7. iter-3.5 ships the four that don't.

- **`feraille-controls::Column`** — `id` (enum: Name/Size/Kind/Modified), `label`, `width` (0.0 = flex), `align` (Left/Right). One flex column per row is enough; Name takes whatever's left after the fixed-width Size/Kind/Modified.
- **`SortKey { column, ascending }`** — owned by VirtualizedList; default is name-asc. `sort_entries(slice, key)` sorts in place, with directories grouped before files (Finder/Explorer convention) and the key only ordering within each group.
- **Header row** — 28 DIPs above the list, painted in `bg.layer2`, click-to-sort. Active sort column gets `fg.primary` weight `SemiBold` plus a `▲` / `▼` arrow next to the label. Hovered headers paint `bg.layer3` underneath.
- **Kind column data** — new `display_kind` field on `FileEntry`, populated at enumerate time: "Folder" / "Symlink" / uppercased extension ("RS", "MD", "TOML"…) / "File". macOS shell crate (iter-4) replaces with `NSWorkspace.localizedDescription`.
- **Sort glyph fix** — first try used U+25B4 / U+25BE (small triangles); same lesson as iter-2.6's chevron fix: those aren't in Arial. Switched to U+25B2 / U+25BC (full triangles), which render cleanly.
- **Screenshot CLI flag** — `--sort name|size|kind|modified[-desc]` for verifying sort behavior headlessly. Verified `/tmp/feraille-cols.png` (name asc) and `/tmp/feraille-cols-by-size.png` (size desc).

What's deliberately not in iter-3.5:
- **Resizable column widths.** Drag the boundary to resize. Standard but wants a thin hit-zone hover state and per-tab persistence. iter-3.6 if asked.
- **Configurable column visibility.** Right-click header → toggle which columns show. Wants the context menu host that lands with the macOS shell crate.
- **Per-tab sort.** Currently sort is global on the list (one VirtualizedList instance, shared across tabs). Per-tab is nicer (Documents sorted by date, Source sorted by name, …); easy to lift if asked.

### Iter-3.6 — real macOS icons via NSWorkspace

User: "let's work on the icons now and try to get those from the system?"

Three changes lined up:

- **`Renderer::draw_bitmap(rect, &Bitmap)`** plus a new `Bitmap` type in `feraille-render`. RGBA8 row-major, straight (not premultiplied). `SoftRenderer` impl does nearest-neighbor scaling and alpha blits over the existing buffer. Pure pixels — no platform deps.
- **`feraille-fs-native::fetch_icon_rgba`** (cfg(target_os = "macos")) — calls `NSWorkspace.iconForFile:`, allocates an `NSBitmapImageRep`, draws into it via `NSGraphicsContext`, reads `.bitmapData()`. Returns `(Vec<u8>, w, h)`. Other OSes return `None` (Windows shell extract lands with the Win32 shell crate).
- **App icon cache**: `HashMap<String, Bitmap>` keyed by `cache_key_for(entry)` — extension for files (".rs", ".md"), `"DIR"` / `"SYMLINK"` / `"FILE"` for the rest. `prefetch_icons` runs after every navigate; visible rows fetch synchronously on first hit (~1ms each) and reuse from cache forever after. `VirtualizedList::paint` takes an `icon_for` closure; falls back to the colored-square placeholder when the cache misses.

Two debugging gotchas worth remembering:

1. **`NSBitmapImageRep::alloc()`** needed `use objc2::ClassType` to bring the trait method into scope — wasn't obvious from the error.
2. **`+saveGraphicsState` / `+restoreGraphicsState`** (the *class* methods, not instance) aren't bound by `objc2-app-kit 0.2`. Reach for them via `msg_send![class, saveGraphicsState]`.
3. **First attempt set `NSBitmapFormat::AlphaNonpremultiplied`** — `graphicsContextWithBitmapImageRep` silently returned nil. Apple requires premultiplied alpha for drawing. Fix: use `NSBitmapFormat::empty()` (= 0 = default = premultiplied RGBA) and undo the premult on the read side.

The screenshot tool paid back immediately — without it, this would have taken an order of magnitude longer to debug. The "graphicsContextWithBitmapImageRep returned nil" message led straight to the right StackOverflow / Apple docs answer.

Tree pane still shows colored squares — `FileTree::paint` doesn't take the icon callback yet. Iter-3.7 is the obvious follow-up.

### Iter-3 done; deliberate stop

Pausing autonomous work here. The user is offline and asked me to make my own choices "without overengineering." Iter-3.1 (file actions) and 3.2 (icon hues + title sync) are substantive shipped wins that turn the app from a viewer into something you'd use.

What I deliberately did **not** ship in iter-3:

- **F2 inline rename.** Needs a TextInput overlay that paints in the row's name area while the rest of the row paints normally. Doable but ~150 LOC of new infrastructure. Worth doing well in a single chunk, not rushed.
- **Cmd+F search/filter.** Similar story — wants a TextInput in the chrome plus filtered-view bookkeeping that doesn't break selection-by-index.
- **Clipboard ops** (Ctrl+C/X/V, copy path). Adds an `arboard` dependency. Worth waiting until the macOS shell crate lands and provides clipboard via NSPasteboard.
- **Tabs persistence** across launches. Needs a small SQLite-backed app state store; same crate the iter-5+ ant trail will use.
- **GPU renderer (wgpu/Vello)**, **native macOS chrome** (transparent titlebar / vibrancy), **threaded FS enumeration**. The original iter-3 plan from the planning doc. None of these add features; they're foundation work that should happen alongside their first paying feature, not speculatively.

The slow-AI receipts here: I held to "don't overengineer" by stopping at the iter-3.2 commit instead of pushing into iter-3.3 / iter-4 territory autonomously. The user can review the four iter-3.x commits and direct what comes next on return.

## Trade-offs made under time pressure

- **Sync FS enumeration this iter.** The `FsBackend` trait already encodes streamed batches; iter-2 fills it with a single sync batch. Threading + change-watching land in iter-3 with the macOS shell crate.
- **Glyph cache FIFO not LRU.** 2048 entries with FIFO eviction. Cheap; correct enough. Revisit only if profiling shows thrash on long Unicode strings.
- **TextInput skips IME.** macOS CJK users will have no composition until iter-3 plugs in `NSTextInputClient`.
- **No mouse range/marquee selection.** Single-click replaces cursor; keyboard handles range/discrete. Marquee needs hit-test infrastructure not justified by iter-2's budget.
- **No native macOS chrome.** Transparent titlebar + traffic-light positioning + `NSVisualEffectView` vibrancy all wait for iter-3 (when the macOS shell crate exists). Iter-2 ships standard winit chrome.
- **Skipped TextInput at the iter-2 checkpoint.** The plan called for TextInput to land in step 12 so Breadcrumb could have an edit mode (Ctrl+L). At checkpoint review, no other iter-2 control actually needs it (TabStrip "+" button, FileTree, breadcrumb-segment-click are all read-only). Per slow-AI's "no abstractions until they pay back" rule, slid TextInput to iter-2.5 alongside SearchBox where it earns its keep. Breadcrumb ships read-only.

## With more time, I would

- Native macOS chrome (transparent titlebar, vibrancy, traffic-light positioning) — iter-3.
- GPU renderer via wgpu/Vello — iter-3.
- Shell drag-drop via `NSPasteboard` + native context menu via `NSMenu` — iter-4.
- Streaming/threaded FS enumeration with cancellation — iter-3.
- Ant trail (folder-usage heat) — iter-5+.
- Disk-usage treemap — iter-5+.
- Magic file detection (the type DB ports cleanly from Ferail) — iter-5+.

## Things to discuss in the walkthrough

- "Why a soft renderer instead of wgpu?" — phased adoption. Soft + glyph cache is sufficient on Mac at our scales; GPU is a single-impl swap behind the trait.
- "Why ditch GPUI?" — pre-1.0, Zed-coupled, doesn't solve shell integration (the load-bearing half). Studied; wrote fresh.
- "Why per-tab selection?" — matches Explorer/Files App. Tabs are independent navigation contexts; bleeding scroll/selection across tabs feels wrong.
- "Why a transient Sidebar that gets replaced?" — kept the binary alive while incrementally landing tabs/breadcrumb/tree. ~140 LOC of deliberate scaffolding, not technical debt.
