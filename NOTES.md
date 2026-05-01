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
