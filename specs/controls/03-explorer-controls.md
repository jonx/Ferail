# Explorer-Specific Controls

These controls exist because Feraille is a file explorer. They compose the
primitive controls and obey the nonblocking paint contract.

## VirtualizedList

Purpose: render large file lists with stable scroll/input latency.

Rules:

- Paint only visible rows plus overscan.
- Draw from cached row data.
- Never format metadata, read files, resolve paths, or fetch icons in row paint.
- Sorting/filtering operate on app-owned data outside paint.

Current:

- Name, Size, Kind, Magic, Modified columns.
- Click-to-sort.
- Hover and selection.
- Cached icon callback.

Todo:

- Column resizing/visibility.
- Type-ahead.
- Multi-select polish.
- NodeId-native row model.

## FileTree

Purpose: left navigation tree.

Rules:

- Lazy-load children.
- Draw visible rows only.
- Emit expand/navigate events.
- Never enumerate from paint.

Current:

- Home and volume roots.
- Expand/collapse.
- Auto-reveal current path.
- Ant Trail heat stripe.

Todo:

- Finder Favorites/iCloud/Network roots.
- Async child enumeration.
- Drag-over auto-expand.

## BreadcrumbBar

Purpose: current path display and path edit.

Current:

- Clickable segments.
- Text edit mode.

Todo:

- Segment dropdowns.
- Completion.
- Overflow collapse.

## TabStrip

Purpose: independent navigation contexts.

Current:

- New/close/activate.
- Traffic-light inset for Mac chrome.

Todo:

- Reorder.
- Persistence.
- Overflow.

## ContextMenuHost

Purpose: native-feeling action menu.

Current:

- Hardcoded Mac menu slice.

Todo:

- Native NSMenu.
- Open With, services, background menu, multi-selection, disabled states.

## StatusBar

Purpose: current item/folder status and background task progress.

Current:

- Selection/item count/status text.

Todo:

- Progress task aggregation.
- Errors/toasts.
- Worker queue summary.

## Search/Filter

Purpose: filter current folder immediately.

Current:

- Modal filter input.
- Filters by name, kind, and magic.

Todo:

- Inline chrome search field.
- Chips/advanced predicates.
- Recursive worker-backed search.

## Preview Pane

Purpose: inspect without opening.

Current:

- Metadata/info pane.

Todo:

- Async text/image/Quick Look providers.
- Preview cache.
- Cancellation and progress.
