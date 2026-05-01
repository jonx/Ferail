# Error and Empty States

The most-skipped category in spec docs and the most-encountered category in real use. Every state below has its own visual treatment, defined here, not invented per-screen.

## Empty: folder has no items

**When:** enumeration completed, returned zero items.

**Visual:** centered EmptyState primitive (see [02-primitives.md](../controls/02-primitives.md)).
- Icon: empty-folder glyph.
- Title: "This folder is empty"
- Body: "Drop a file here to add it"
- Action: none.

The pane *itself* is still a drop target — the EmptyState text reinforces this affordance.

## Empty: search returned nothing

**When:** search box has a value, results are empty.

**Visual:** EmptyState with:
- Icon: search-with-slash glyph.
- Title: "No matches for "{query}""
- Body: "Try removing a filter, or {searchSubdirsLink}"
- Action: "Search subfolders" (kicks off recursive search if enabled).

Filter chips remain visible above the EmptyState so the user can see why they got nothing.

## Empty: tab just opened, no folder selected

**When:** new tab, no navigation yet.

**Visual:** "Recent" view — a grid of the last 12 visited folders, plus pinned items. This is a v1.5 feature; in v1 the new-tab default is the user's home folder.

## Loading: long enumeration

**When:** enumeration > 50 ms but < 4 s.

**Visual:** *no full-screen blocker.* The list paints partial results as they stream. A thin indeterminate ProgressBar appears at the bottom edge of the file pane (above the status bar). It vanishes on completion.

Status bar shows "Loading: 1,243 of ?".

## Loading: very long enumeration

**When:** enumeration > 4 s.

**Visual:** same partial-results streaming. Status bar adds "(this is taking a while; press Esc to cancel)". Esc cancels enumeration cleanly — leaves whatever was loaded so far.

## Error: folder doesn't exist

**When:** navigated to a path that returned `ERROR_PATH_NOT_FOUND` or equivalent.

**Visual:** ErrorState primitive.
- Icon: warning glyph (`status.warning`).
- Title: "This folder no longer exists"
- Body: "It may have been moved or deleted."
- Actions: "Go to parent" (primary), "Refresh" (secondary).

Address bar remains editable so the user can type a different path.

## Error: permission denied

**When:** enumeration returned `ERROR_ACCESS_DENIED` or equivalent.

**Visual:**
- Icon: lock glyph (`status.danger`).
- Title: "You don't have access to this folder"
- Body: "Take ownership or sign in as a user with permission."
- Actions: "Take ownership" (primary, invokes shell ownership flow on Windows), "Open in Terminal" (secondary, drops into elevated shell).

## Error: drive not ready

**When:** drive ejected, removable not inserted, network share unreachable.

**Visual:**
- Icon: drive-with-slash glyph.
- Title: contextual: "{drive letter}: is not ready" or "Can't reach {server}"
- Body: "Insert the drive and try again." or "Check your network connection."
- Action: "Retry" (primary), "Go to parent" (secondary).

The error appears *after* a 4-second timeout on the actual I/O — before that, we show a LoadingSpinner.

## Error: enumeration partially failed

**When:** some items enumerated successfully, but the OS reported an error mid-stream (e.g. one subfolder is corrupt).

**Visual:** the items that *did* enumerate are shown normally. A non-blocking Toast at the bottom-right: "Couldn't read 3 items in this folder." with a "Details" action linking to a side panel listing the failures.

This is a v1.5 feature; in v1 we just show what we got and silently log the rest.

## Generic crash recovery

**When:** the app's own state ended up wedged (a panic on a worker thread, an inconsistency that triggered a debug assertion in release).

**Visual:** never. We do not show "Oops!" screens. We:
1. Log the failure with full context.
2. Restore the affected pane's state from the last known-good snapshot (per-tab snapshots taken on every successful navigation).
3. Show a Toast: "Something went wrong; restored your previous folder."

The app continues. We never throw the whole window away.

## Visual contract

All ErrorState and EmptyState renderings:
- Centered horizontally and vertically in their host pane.
- Max content width: 320 DIPs.
- Icon size: 48 DIPs.
- Title: `text.xl`, `weight.semibold`, `fg.primary`.
- Body: `text.md`, `fg.secondary`, max 2 lines, line-height 1.5.
- Action buttons: aligned center, primary first, gap `space.sm`.
- Icon-to-title gap `space.md`, title-to-body gap `space.xs`, body-to-actions gap `space.lg`.

The component should look the same whether it's an "empty" or an "error" — only the icon and copy change. This is on purpose: users learn the visual once.

## Copy guidelines

- Title is what *happened*, not why. ("This folder no longer exists" — not "Path not found.")
- Body explains why or what to try.
- Actions are imperatives starting with a verb.
- Never use "An error occurred" or "Something went wrong" as a title — those are body lines, if at all.
- Never use exclamation marks.
- Never apologize ("Sorry, …"). The user came to do work; sympathy is friction.
