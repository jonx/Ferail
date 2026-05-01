# Explorer-Specific Controls

Seven controls that exist *only* because this app is a file explorer. Each composes primitives from [02-primitives.md](02-primitives.md) plus its own paint/event logic.

---

## 1. VirtualizedList — the file pane

The single most important control. Everything else is decoration.

**Purpose:** display ≥ 1,000,000 rows at 144 Hz.

**API:**
```rust
pub struct VirtualizedList<'a, Item> {
    items: &'a [Item],                          // borrowed; never cloned
    row_height: RowHeight,                      // Fixed(28) or Variable(closure)
    columns: &'a [Column<Item>],                // header + cell renderer
    selection: &'a Selection,                   // anchor + cursor + set
    sort: Option<SortKey>,
    on_event: Callback<ListEvent>,
}
```

**Virtualization:** only rows in `[scroll_top - overscan, scroll_top + viewport + overscan]` are rendered. Overscan = 8 rows. The render loop never iterates `items` end-to-end except for sorting.

**Item rendering — the hot path rule:**
> The cell renderer closure receives `(&Item, paint_state) -> CellPaint` and **must not allocate, must not perform I/O, must not resolve paths.** Any string formatting (size in MB, mtime ago, etc.) was done at item-construction time, not render time.

This is the rule from Ferail's [CLAUDE.md](../../../Ferail/CLAUDE.md) carried forward as a hard contract. Inspired by what GPUI's `crates/gpui/src/elements/list.rs` *almost* enforces but doesn't quite: GPUI lets you allocate per-row, and Zed gets away with it because their rows are syntax tokens, not 1M files.

**Selection model:** see [`specs/ux/02-selection.md`](../ux/02-selection.md).

**Keyboard navigation:**
| Key | Action |
|---|---|
| ↑ / ↓ | move cursor by 1 row |
| Page Up / Down | move cursor by viewport |
| Home / End | first / last item |
| Space | toggle item in selection (when in multi-select mode) |
| Ctrl+A | select all |
| type-ahead | jump to first item starting with typed prefix; resets after 800ms idle |

**Mouse:**
- Click row: replace selection.
- Ctrl+click: toggle row.
- Shift+click: range from anchor.
- Drag in empty space: lasso/marquee selection rectangle.
- Drag on selected row: drag-out (DnD source).

**Rendering layers, bottom to top:**
1. Background `bg.base`.
2. Selection fills (`accent.subtle` or `accent.subtle-inactive`).
3. Hover row tint `bg.layer3` (only one row at a time).
4. Row content (icon + cells).
5. Column dividers (`border.subtle`, 1 DIP).
6. Drop indicator if drag is active.
7. Focus ring on cursor row.

**Header:** sticky top row, `bg.layer2`, height 28 DIPs. Click sorts; Shift+click adds secondary sort. Drag column edge resizes; double-click edge auto-fits.

**Performance budget:**
- Per-frame paint of visible rows: ≤ 4 ms CPU at 200,000 items.
- Scroll input → paint latency: ≤ one frame (≈ 7 ms at 144 Hz).
- Memory per item: ≤ 96 bytes including cached display strings.

---

## 2. FileTree — left navigation pane

Same virtualization core as VirtualizedList, but:

- Each row has an indent + a chevron.
- Rows can have children (lazy-loaded on first expand).
- Three special root nodes: **Quick Access**, **This PC**, **Network** (Windows); macOS dev mode shows just **Locations** with the user's home tree.
- Multi-selection is *not* supported (single-select only — matches Explorer).

**API:**
```rust
pub struct FileTree<'a, Node> {
    nodes: &'a NodeStore<Node>,                 // ID-keyed; UI never holds paths
    expanded: &'a HashSet<NodeId>,
    selected: Option<NodeId>,
    on_event: Callback<TreeEvent>,
}
```

**Node-store contract** (carried from Ferail):
- Tree rows are `NodeId`-only. They never carry `PathBuf` or `String`.
- Resolving an ID → display name → path is the host's job, done at action boundaries (drag-out, context menu invoke), never during paint.

**Indent guides:** vertical `border.subtle` line at each ancestor's chevron column. Drawn only for the visible window of rows.

**Lazy children:** expanding a node fires `TreeEvent::Expand(id)`; the host responds by populating `nodes.children(id)` and re-rendering. While loading, the chevron animates (LoadingSpinner replaces it). Cap: visible loading after 200 ms; before that, nothing visible.

**Drag-over auto-expand:** if pointer hovers a collapsed node ≥ 600 ms during a drag, expand it.

---

## 3. BreadcrumbBar — the address bar

**Purpose:** show the current path as clickable segments, with two modes: **breadcrumb** (default) and **edit** (text input).

**API:**
```rust
pub struct BreadcrumbBar<'a> {
    segments: &'a [Segment],                    // each: display name + nav handle
    mode: BreadcrumbMode,                       // Breadcrumbs | Editing(String)
    on_navigate: Callback<NavHandle>,
    on_edit_submit: Callback<String>,
}
```

**Segments:** `[<root icon>] > [Users] > [jkn] > [Source] > [Feraille]` — each chevron `>` is itself a control: hover shows a dropdown of *that* directory's children (sibling navigation in Explorer style).

**Edit mode entered by:**
- Click on the empty rail right of the last segment.
- `Ctrl+L`.
- Pressing `F4` (Windows convention).

**Edit mode visual:** the entire bar becomes a single TextInput with the resolved path text-selected. Enter navigates; Esc reverts.

**Truncation:** when too narrow, leftmost segments collapse into a single overflow chevron whose dropdown lists them.

---

## 4. TabStrip

**Purpose:** ≤ 12 tabs of independent navigation state.

**API:**
```rust
pub struct TabStrip<'a> {
    tabs: &'a [Tab],                            // title, icon, modified-flag
    active: usize,
    on_select: Callback<usize>,
    on_close: Callback<usize>,
    on_reorder: Callback<(usize, usize)>,
    on_new: Callback,
}
```

**Layout:** horizontal row, height 32 DIPs, `bg.layer2`, divided from content by 1-DIP `border.subtle`. Each tab: icon (16) + title (truncated, max 160 DIPs) + close-X (visible on hover or active).

**States per tab:** idle, hover, active, dragging.

**Interaction:**
- Click → activate.
- Middle-click → close.
- Drag horizontally → reorder.
- Drag downward off the strip → detach into a new window (v2; spec the gesture, ignore the action in v1).
- "+" button at the end → `on_new`.

**Overflow:** when total width exceeds available, tabs shrink uniformly to 80 DIPs min. Below that, scroll arrows appear at each end.

---

## 5. ContextMenuHost

**Purpose:** the *thin* wrapper around the OS shell context menu. The menu items themselves come from the shell on Windows; on macOS dev they come from a hardcoded list (Open / Reveal / Copy Path).

**API:**
```rust
pub struct ContextMenuHost {
    pub fn open_for(
        &mut self,
        anchor: ScreenPoint,
        target: ContextTarget,                  // Files(Vec<NodeId>) | Background(NodeId)
        host_window: WindowHandle,
    ) -> Result<(), MenuError>;
}
```

**Backends:**
- **Windows:** `IContextMenu::QueryContextMenu` → `TrackPopupMenuEx` → `IContextMenu::InvokeCommand`. This is the [shell-win32](../../crates/feraille-shell-win32) crate's job; the control just hands it the HWND and the PIDLs.
- **macOS dev:** custom popup using primitives (Panel + Buttons). Not feature-parity — just enough to validate menu *invocation* during dev.

**Why a thin host instead of building the menu ourselves:** the Windows shell context menu *includes third-party items* (TortoiseGit, 7-Zip, Notepad++, etc.). Reimplementing it loses ~80% of what users expect from "right-click on a file." This is the same reason Files App still calls into IContextMenu.

**Visual contract on Windows:** the menu *is* the system's. We do not theme it. (Win11 already gives it acrylic + rounded corners.)

**Custom additions:** Feraille can prepend its own items (e.g. "Open in Terminal", "Compare With…") via the standard `IContextMenu` `idCmdFirst` slot reservation.

---

## 6. StatusBar

**Purpose:** the bottom strip. Three slots: left (count + selection summary), center (transient operation progress), right (view mode + zoom).

**API:**
```rust
pub struct StatusBar<'a> {
    left: &'a [Slot],
    center: Option<OperationProgress>,
    right: &'a [Slot],
}
```

**Height:** 24 DIPs, `bg.layer2`, top border `border.subtle`. Text size `text.xs`.

**Operation progress:** when a copy/move/delete is in flight, the center shows a 200-DIP-wide ProgressBar plus "12 of 1,043 files (47%)". Click to expand into an overlay with per-file detail. Multi-operation: stacked compact rows.

---

## 7. SearchBox

**Purpose:** find-in-folder input.

**API:**
```rust
pub struct SearchBox<'a> {
    value: &'a str,
    chips: &'a [FilterChip],                    // active filters: type:image, size:>10MB, etc.
    on_change: Callback<String>,
    on_chip_remove: Callback<usize>,
    on_submit: Callback,
    state: SearchState,                         // Idle | Searching | NoResults | Results(N)
}
```

**Layout:** Icon (search) + chips inline + TextInput + clear-X. Height = `hit.input`. Width: flexible, max 480 DIPs.

**Chip behavior:** typed segments matching `key:value` syntax (e.g. `size:>10mb`) are converted to chips on `Tab` or space. Backspace at start of input deletes the rightmost chip.

**State indicator:** when `Searching`, a thin indeterminate ProgressBar overlays the bottom edge of the input.

---

## What these controls explicitly do *not* know

- They do **not** know what a path is. They speak `NodeId`.
- They do **not** call into any FS API. They emit events; the host services them.
- They do **not** know about COM, IFileOperation, or PIDLs. The shell crate translates `NodeId` ↔ shell.
- They do **not** import `feraille-shell-win32` or `feraille-fs-*`. Direction of dependency is one-way.

This is the decoupling the user asked for. It is mechanical, not just polite.

## Composition example (the main window)

```
Window
├─ TabStrip
├─ Toolbar (row of Buttons + BreadcrumbBar + SearchBox)
├─ Splitter
│  ├─ Panel "Navigation"
│  │   └─ FileTree
│  └─ Splitter
│      ├─ Panel "Files"
│      │   └─ VirtualizedList
│      └─ Panel "Preview"  (optional, hidden by default)
└─ StatusBar
```

That is the entire chrome. Everything else is content.
