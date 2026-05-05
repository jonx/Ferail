# Selection Model

## The data structure

```rust
pub struct Selection {
    pub anchor: Option<NodeId>,     // last single-click target
    pub cursor: Option<NodeId>,     // current keyboard focus row (may differ from anchor)
    pub set: SelectionSet,          // the actually-selected items
}

pub enum SelectionSet {
    None,
    Single(NodeId),
    Range { from: NodeId, to: NodeId },
    Discrete(BTreeSet<NodeId>),
}
```

`Range` is a separate variant from `Discrete` because for ranges of millions of items we never materialize the full set in memory. A range stores its endpoints; membership tests are bounds checks.

This is the same idea Zed uses for its file picker / search panel — see `crates/gpui/src/elements/list.rs` (search "selected_indices") for inspiration on the membership-as-predicate trick.

## Gestures (pointer)

| Gesture | Effect |
|---|---|
| Click row | `set = Single(row)`, `anchor = row`, `cursor = row` |
| Cmd+click row | toggle `row` in `set` (Single -> Discrete{anchor, row}); `cursor = row`; `anchor` unchanged |
| Shift+click row | `set = Range { from: anchor, to: row }`; `cursor = row` |
| Cmd+Shift+click row | extend Discrete by union with [anchor..row]; `cursor = row` |
| Drag in empty space | marquee/lasso: `set = Discrete(rows intersecting rect)`; updates live during drag |
| Cmd+drag in empty space | additive marquee: union with existing `set` |
| Click in empty space | clears: `set = None` |
| Drag a *selected* row | start drag-out (DnD source); selection unchanged |
| Drag an *unselected* row | first replace selection with `Single(row)`, then start drag-out |

## Gestures (keyboard)

| Key | Effect |
|---|---|
| ↑ / ↓ | move `cursor` by 1; if no modifier, `set = Single(cursor)`, `anchor = cursor` |
| Shift+↑/↓ | move `cursor` by 1; `set = Range { from: anchor, to: cursor }` |
| Cmd+↑/↓ | move `cursor` by 1 *without* changing `set` (focus-only) |
| Page Up / Down | move `cursor` by viewport; selection follows the same modifier rules as ↑/↓ |
| Home / End | first / last; same modifier rules |
| Space | when `cursor != None`: toggle `cursor` in `set` |
| Cmd+A | `set = Range { from: first, to: last }` |
| Esc | `set = None`, `anchor = None`. `cursor` remains. |

## Type-ahead and selection

Type-ahead jumps the **cursor** but does *not* alter the **set** unless the user has no current selection (in which case `set = Single(matched)` for ergonomics). This matches common file-manager behavior.

Reset window: 800 ms of keyboard idle clears the type-ahead buffer.

## Selection across navigation

Navigating to a new folder clears selection. Going *back* via history restores the prior selection (cursor and set).

## Selection during drag-and-drop

- Drag *out* of the file pane: selection at the moment of drag-start is the payload. Mid-drag selection changes in this pane are ignored.
- Drag *into* a folder row: that row becomes a "drop target" with a highlight; the user's existing selection is preserved unchanged.

## Visual rendering

- Selected rows in the **focused** list: `accent.subtle` background, `fg.primary` foreground.
- Selected rows in an **unfocused** list (e.g. focus is in the tree): `accent.subtle-inactive` background, foreground unchanged.
- Cursor row (regardless of selection): FocusRing overlay if list is focused; otherwise no visual.
- A selected *and* hovered row: background lerps from `accent.subtle` toward `accent.subtle` × 1.1 — subtle, but distinguishable.

## Multi-select mode toggle

Some users prefer an explicit "multi-select" toggle so they don't have to hold modifiers. We expose this as a status-bar toggle and `Cmd+Shift+M`. When on:
- Single click toggles instead of replaces.
- Each row paints a Checkbox in a 24-DIP gutter prepended to the row.

The mode state is per-tab.

## Performance notes

- `set: SelectionSet` is `O(1)` to test for `Single` and `Range`, `O(log n)` for `Discrete`.
- Range membership during marquee drag is computed by comparing row Y bounds, not by iterating items.
- Marquee drag updates the rendered selection at most once per frame, even if the pointer moves between frames.
