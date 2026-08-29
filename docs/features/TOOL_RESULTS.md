# Tool Result Surfaces

Status: shipped for Search, Duplicate Finder, docked Disk Usage, Archive and
checksum Verify.

Tool result surfaces are tab-local tools that temporarily replace the normal
directory listing while keeping the tab rooted at a real folder. They are the
shared UX contract for "run a tool here and inspect its results here" rather
than a new worker framework.

## User model

A result surface answers three questions clearly:

- What tool is active?
- Which folder did it run in?
- How do I get back to browsing?

The breadcrumb row carries the shared result pill and a close button. The
breadcrumb path remains the tool root, so the user can still understand where
the result came from and navigate away normally.

Tool bodies should adapt to their host. When docked, avoid repeating the root
path already visible in the shell breadcrumb; when windowed, include enough
context in the tool header for the window to stand alone.

Host placement is expressed through `ToolHostContext` and `ToolHostEvent`
(`crates/ferail-gpui/src/tool_results.rs`). A host move is dispatched through
`ToolResultSurface::handle_host_event`, which forwards
`ToolHostEvent::HostChanged(Docked | Windowed)` to the active tool body. The
tool then decides which chrome, labels, controls, or shortcuts make sense in
that host. Tool-specific attachment state, such as Disk Usage's dock-back
callback, stays inside that tool instead of leaking into the shared host event.

The shared actions are in the command catalogue:

- `view.close_results` — closes the active result surface.
- `disk_usage.open_in_window` — pops docked Disk Usage into a standalone
  window.

Standalone Disk Usage windows opened from the shell also show a Dock in Tab
button. It sends the same root back to the owning shell and closes the window.
Disk Usage host moves preserve the same `DiskUsageView` entity rather than
constructing a fresh scan; the moved view receives a host-context change event.

## Implemented surfaces

- Search: replacing the file list with streamed recursive or Spotlight results.
- Duplicate Finder: replacing the file list with grouped rows or the dedicated
  virtualized card panel.
- Disk Usage: docking the treemap/top-files view into the active tab, with
  Open in Window and Dock in Tab host moves.
- Archive: an editable archive workbench hosted by the tab.
- Verify: a virtualized per-manifest status report with coalesced progress,
  cancellation, problem filtering and explicit mismatch/missing/unsafe/change
  outcomes.

All are represented by `Tab::tool_result`, whose variants live in
`crates/ferail-gpui/src/shell/tab.rs`.

## Architecture

`ToolResultSurface` is intentionally enum-shaped:

- `Search(SearchMode)`
- `Duplicates(DupeViewMode)`
- `DiskUsage(DiskUsageMode)`
- `Archive(ArchiveMode)`
- `Verify(VerifyMode)`
- `ToolHostContext` / `ToolHostEvent`

This avoids forcing unrelated workers through a trait abstraction. Search and
duplicates stream rows into the existing table model. Disk Usage owns a GPUI
entity because its treemap state, scan queue, layout cache, and controls are
not table-shaped.

The shell owns placement and lifecycle:

- entering a result surface cancels tab-local directory/search/duplicate work
  that would otherwise update hidden rows;
- host moves dispatch `ToolHostEvent` through the `ToolResultSurface` enum;
- watcher reloads skip tabs with active tool results;
- navigation clears the result surface and commits the destination directory;
- closing the result reloads the tab root as a normal directory;
- Escape closes the active result after any more-local cancellation layer
  (autocomplete, inline edit, or selection) has had its turn; editable archive
  results run the same unsaved-change confirmation as their close button;
- the status bar and task popover still use the shared `TaskRegistry`.

The tool owns domain state:

- worker launch and cancellation;
- streamed result data;
- summary values shown in the shared pill;
- tool-specific controls inside the result body.

## Disk Usage sizing

Disk Usage can render in a standalone window or inside a tab. The view therefore
does not size from the native window viewport after the first frame. It measures
its host element with `on_prepaint` and sizes the treemap from that container.

## UX rules

- Use a tool result surface when the output is a browsable result set rooted at
  a folder.
- Keep results tab-local unless there is a strong cross-window reason.
- Do not hide cancellation/progress only inside notifications; long-running
  tools must register tasks.
- Keep the shared header small. Tool-specific controls belong inside the body.
- Closing a result returns to the directory; it must also cancel any still-live
  worker owned only by that result.

## Follow-ups

- Let Search and Duplicate Finder expose richer tool-specific filters inside
  their result bodies.
- Consider saved tool-result identities for Smart Folders / Saved Searches.
- Let Verify reveal rows, copy/export failures and optionally enumerate extras.
