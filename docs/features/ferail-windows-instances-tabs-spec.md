# Ferail - Windows, Instances & Tabs Specification

Behavioral spec for multi-window, multi-tab, and cross-window/cross-app drag-and-drop. Written against `ARCHITECTURE.md` (the "UI must never stop" directive, the shell/tab ownership model) and the selection/DnD spec.

A note on terminology, because "instance" is ambiguous and the distinction drives the whole design:

- **Process**: one running copy of the `ferail-gpui` executable.
- **Window**: one OS window. A process owns one or more windows.
- **Tab**: one browsing context inside a window. A window owns one or more tabs.

The central architectural decision in §1 is: **Ferail is single-process, multi-window.** "Multiple instances" from the user's point of view means multiple windows, all served by one process. This is deliberate and explained below.

---

## 1. Process model: single-process, multi-window

### 1.1 The decision

Ferail runs as **one process** per user session. Launching Ferail again, from the dock, from Finder, from `open -a`, from the CLI, does **not** start a second process. It signals the existing process to open a new window (or activate an existing one).

This is the standard macOS application model (Finder, Safari, Mail all work this way) and it's the right call for Ferail specifically because:

- **The shell is the coordination point.** The architecture doc puts the file watcher, task registry, notifications, preview cache, metadata caches, and `NodeStore` in the shell. These are process-wide resources. Two processes would mean two file watchers fighting over the same FSEvents streams, two SQLite connections to `ferail-meta`, two preview caches doing duplicate work, and two `NodeId ↔ PathBuf` maps that don't agree. `NodeId` identity is only meaningful *within* the process that minted it: cross-process it's noise.
- **Cross-window DnD is in-process and cheap.** Dragging nodes from one window to another is just moving a payload between two windows the same process owns: the `NodeId` payload is valid on both ends. Cross-*process* would force everything through the OS pasteboard as paths, losing identity and the controlled handoff model.
- **One task registry, one source of truth.** A file operation started from window A and a navigation in window B both touch shared state. One process, one shell, one registry.

### 1.2 Process lifetime and the singleton mechanism

- On launch, the process attempts to become the **primary instance**. Mechanism: a well-known Mach service name registered with the system, or an exclusive lock file in the app's container, checked atomically at startup. (macOS's `NSApplication` already provides most of this through the standard "reopen" and "open files" Apple Events: prefer the platform mechanism via `ferail-shell-mac`; the lock file is the fallback for CLI-initiated launches that bypass `NSApplication`.)
- If a process starts and finds a primary already running: it forwards its **launch intent** (open a new window, open these paths, run this CLI verb against the GUI) to the primary via the same channel, then exits. The primary handles the intent.
- The primary process lives as long as **at least one window is open** OR the app is configured to stay resident with no windows (macOS apps commonly stay running with zero windows: dock icon present, `Cmd+N` reopens). Decide per product taste; default recommendation: **stay resident with zero windows** (matches Finder/Safari), quit only on `Cmd+Q` or app menu Quit.
- The CLI `ferail` binary for non-GUI utilities (`magic`, `du`) is **separate** and does not participate in the singleton: those are one-shot processes that do their work and exit. Only `ferail-gpui` is the singleton. A CLI verb that wants to *drive the GUI* (e.g. "open this folder in the app") forwards an intent to the primary `ferail-gpui` process if one exists, or launches it.

### 1.3 What "open Ferail" does, by entry point

| Entry point | Behavior |
|---|---|
| Dock icon, no windows open | Open a new window at the default location (last-closed window's folder, or Home). |
| Dock icon, windows already open | Activate the app, bring the frontmost Ferail window forward. Do **not** open a new window. |
| `Cmd+N` in-app | New window. |
| `Cmd+T` in-app | New tab in the current window. |
| Finder "Open With → Ferail" on a folder | If a window is open: new tab (or new window: see §4.3 preference) at that folder, in the frontmost window. If no window: new window at that folder. |
| `open -a Ferail <folder>` | Same as Finder "Open With". |
| Double-click a folder when Ferail is the registered folder handler | Same as Finder "Open With". |
| CLI `ferail-gpui <path>` while primary running | Forward intent to primary: open `<path>` per the §4.3 preference. Launching process exits. |
| CLI `ferail-gpui` while primary running | Forward "new window" intent. Launching process exits. |
| Crash recovery relaunch | See §6: restore the prior window/tab session. |

---

## 2. Window model

### 2.1 What a window owns

A window is a top-level GPUI window with native macOS chrome and the gpui-component title bar. Each window owns:

- An **ordered list of tabs** and an **active tab index**.
- Its **frame** (position + size) and its **window state** (normal / zoomed / fullscreen).
- A **window id**: process-local, stable for the window's lifetime, used by intents and session restore.
- The **tab bar** UI and the **title bar** UI.
- The split-panel layout *geometry* for that window (sidebar width, preview width). Whether panel widths are per-window or app-global is a preference; default **per-window**, so a user can have a wide-sidebar window and a no-preview window simultaneously.

A window does **not** own: the file watcher, task registry, notifications, caches, `NodeStore`. Those are shell/process-wide (§1.1). A window *displays* slices of them (e.g. the task registry's progress can be shown in any window's title bar or a shared notifications surface).

### 2.2 Window lifecycle

- **New window** (`Cmd+N`, or intent): created with one tab. The tab's starting location follows §4.3. New window opens offset from the frontmost window (cascade), or at the platform-restored frame if it's a session restore.
- **Close window** (`Cmd+W` closes the active *tab*; closing the *last* tab closes the window: see §3.4 for the tab-vs-window close rule). `Cmd+Shift+W` closes the whole window regardless of tab count.
- **Closing a window** with in-progress file operations that were *initiated from* that window: the operations are **process-owned**, not window-owned (they run in shell workers). They continue. The task registry remains; its progress just needs another surface to show in. If the closed window was the only one showing the task UI, surface a notification so progress isn't invisible. Never cancel a file operation just because a window closed.
- **Last window closed**: per §1.2, the process stays resident (default). Session state is snapshotted (§6).
- **Minimize / zoom / fullscreen**: standard platform behavior, state is part of the window's session snapshot.

### 2.3 Multiple windows, shared shell - consistency rules

Because all windows share one shell, the same directory can be visible in multiple windows/tabs at once. Rules:

- **A file operation or external change reloads every affected view in every window.** The architecture doc's "reload affected UI state through the shell" applies process-wide. If window A moves a file out of `~/Downloads` and window B has a tab showing `~/Downloads`, window B's tab reloads. The file watcher is shared, so this falls out naturally, but it must be explicit: the reload fan-out targets *all* tabs in *all* windows whose current directory matches.
- **Each view reconciles its own selection** after such a reload, per the selection spec §2.6. Window B's selection in that tab is independent of window A's.
- **Caches are shared, so the second window is fast.** A directory already enumerated/warmed for window A's tab is warm for window B's tab. Streaming enumeration still runs per-navigation (each tab navigation is its own worker + generation), but metadata/magic/icon caches are hit, not recomputed.
- **`NodeId` is valid across all windows** of the process. This is what makes cross-window DnD (§5) clean.

---

## 3. Tab model

### 3.1 What a tab owns

Per the architecture doc, each tab owns its: current directory, node id, history (back/forward), filter, sort, selection, and scroll state. This spec adds: a tab also owns a **tab id** (process-local, stable for the tab's lifetime: survives reordering and moving between windows), and a **title** derived from the current directory's display name.

A tab is a pure browsing context. It does not own a window, a watcher, or caches.

### 3.2 Tab bar UI

- The tab bar lives in/below the title bar. It shows one tab item per tab: icon + truncated title, a close button (appears on hover or always: preference), and the active tab visually distinguished.
- **New-tab button** (`+`) at the end of the strip.
- **Overflow**: when tabs exceed the strip width, either (a) tabs shrink to a minimum width then the strip scrolls, or (b) an overflow menu (`»`) lists the hidden tabs. Recommendation: shrink-to-min then scroll, with the active tab always scrolled into view. A hard cap on tab count is unnecessary; a soft warning past a large number is optional.
- Tab titles truncate from the end with the basename kept visible; full path on hover via Tooltip.
- A tab showing an in-progress enumeration may show a subtle spinner in place of its icon until first batch; a tab with a background task associated may show a small progress affordance. Keep it quiet.

### 3.3 Tab operations

| Operation | Behavior |
|---|---|
| **New tab** (`Cmd+T`, `+` button) | Create a tab per §4.3 starting location, insert it after the active tab (or at the end: pick one; "after active" matches Safari), make it active. |
| **Close tab** (`Cmd+W`, close button, middle-click on tab) | Close the tab. Activation moves to the **next** tab, or the previous if it was last. If it was the only tab → §3.4. |
| **Close other tabs** | Context menu on a tab. Closes all but that one. |
| **Close tabs to the right** | Context menu. |
| **Reopen closed tab** (`Cmd+Shift+T`) | Restore the most recently closed tab, its directory, history, sort, filter, from a per-window (or per-process) closed-tab stack. Selection/scroll restore is best-effort. |
| **Select tab N** (`Cmd+1`…`Cmd+9`) | Activate tab N; `Cmd+9` conventionally = last tab. |
| **Next / previous tab** (`Cmd+Shift+]` / `Cmd+Shift+[`, also `Ctrl+Tab` / `Ctrl+Shift+Tab`) | Cycle activation. |
| **Reorder tab** | Drag a tab within the strip. Tabs are manually ordered; insertion indicator between tabs, like the Favorites reorder model. Drag threshold shared with the other DnD specs. |
| **Move tab to new window** | Drag a tab *off* the strip and release in empty space → the tab tears off into a new window (§3.5). |
| **Move tab to another window** | Drag a tab onto another Ferail window's tab strip → the tab moves into that window at the drop index (§3.5). |
| **Duplicate tab** | Context menu. New tab with the same current directory, sort, and filter; fresh empty history (or copied history: pick one; fresh is simpler and usually expected). |
| **Pin tab** | Optional, later. Pinned tabs shrink to icon-only, sit at the strip's left, survive "close all". Not v1. |

### 3.4 The last-tab / window-close relationship

- Closing the **last tab** in a window closes the **window**.
- `Cmd+W` semantics: closes the active tab. If it's the last tab, the window closes. This matches Safari and is the least surprising.
- A preference may offer "Cmd+W closes window, not tab" for users who want that, but the default is tab-close.
- `Cmd+Shift+W` always closes the entire window (all its tabs) regardless of count.
- Closing a window this way pushes all its tabs onto the closed-tab stack so `Cmd+Shift+T` can bring them back, and snapshots into session state (§6).

### 3.5 Tear-off and merge

Tearing a tab out / moving it between windows is a **pure ownership transfer of an in-process object**: this is the big payoff of single-process (§1.1). The tab's entire state (directory, history, filter, sort, selection, scroll, tab id) moves intact. Nothing is serialized, nothing is re-enumerated, `NodeId`s stay valid.

- **Tear-off to new window**: drag tab off the strip, release in empty space. A new window is created; the tab is removed from the source window's tab list and inserted as the sole tab of the new window. The new window opens near the drop point. If the source window now has zero tabs, it closes (§3.4).
- **Merge into existing window**: drag tab onto another window's tab strip. The tab is removed from the source window's list and spliced into the target window's list at the insertion index. Target window comes forward and activates the moved tab. Source window closes if it's now empty.
- **Merge-all** (menu: "Merge All Windows", like Finder/Safari): collapses every window's tabs into one window. The shell iterates windows, moves all tabs into the frontmost (or a chosen) window in order, closes the now-empty windows.
- During a tab drag, the same auto-scroll rules apply to the tab strip as to lists. The drag visual is the tab itself (or a representation of it) following the cursor.
- A tab drag and a *node* drag are different payloads and must be distinguished at drag-start by where the press lands (on a tab item in the strip = tab drag; on a row in the file table = node drag). They never get confused because they originate from different surfaces.

### 3.6 Tabs and the shared shell

- All tabs across all windows share the shell's caches and watcher. A tab is cheap.
- Each tab's navigation is still an independent streaming enumeration with its own generation and cancellation (per `STREAMING_ENUMERATION.md`). Switching tabs does **not** cancel the inactive tab's in-flight enumeration, it may finish in the background and its result is applied to that tab's model, gated by that tab's generation, ready when the user switches back. (If memory pressure ever makes this a problem, an inactive tab's enumeration *could* be deprioritized, but never silently dropped, that's a future optimization, not v1 behavior.)
- An inactive tab whose directory is changed by an external event or a file op still reloads (through the shell fan-out, §2.3) so it's correct when the user switches to it.

---

## 4. Starting locations and intents

### 4.1 Intents

An **intent** is the unit of cross-process and cross-surface communication. The singleton primary process receives intents from: secondary launches (§1.2), CLI, Finder/`open`, and Apple Events. Intent types:

- `OpenNewWindow { location? }`
- `OpenLocation { path, disposition }` where disposition ∈ { new tab in frontmost window, new window, reuse frontmost tab }
- `ActivateApp` (just come forward, open a window only if none exist)
- `RunGuiVerb { … }` for future command-palette-style remote verbs

Intents are processed on the primary's main thread, immediately and without I/O on the hot path: an intent that names a path hands that path off at the controlled boundary (per the architecture's `PathBuf`-at-boundaries rule) and the resulting navigation is a normal scheduled streaming enumeration.

### 4.2 Path-bearing intents and identity

A path arriving via intent (from another process, CLI, or Finder) is a raw `PathBuf`. It enters at the shell-mac / intent boundary, gets registered in the `NodeStore` to obtain a `NodeId`, and from there it's a normal node. The launching process's notion of identity is irrelevant, only the primary's `NodeStore` mints valid `NodeId`s. This is the same discipline as inbound external DnD (selection/DnD spec §3.7).

### 4.3 New-tab / new-window starting location - preference

Where a new tab or window starts is a user preference with sensible defaults:

- **New tab default**: the same directory as the currently active tab (so `Cmd+T` is "another view of where I am"), OR Home, OR a user-chosen fixed folder. Default recommendation: **same as active tab**.
- **New window default**: Home, OR same as frontmost window's active tab, OR last-closed location. Default recommendation: **Home**.
- **Folder opened from Finder/CLI**: preference for "new tab in frontmost window" vs "new window". Default recommendation: **new tab in frontmost window** if a window exists, else new window. This matches modern Finder behavior and avoids window sprawl.

---

## 5. Cross-window and cross-app drag-and-drop

This builds directly on the selection/DnD spec §3. That spec covers *what* a node drag is and how a drop schedules a file operation. This section covers the *routing* across windows, processes, and apps.

### 5.1 Within the same process - window A to window B

- The drag payload is `NodeId`s + their cached path map (selection/DnD spec §3.1). Because both windows are the same process, the `NodeId`s are **valid on the receiving end**: no serialization, no identity loss.
- The drop target in window B resolves exactly as an in-window drop (selection/DnD spec §3.5–3.6): a folder row, empty space (→ that tab's current directory), a tree node, a volume, a favorite, a breadcrumb segment.
- Dragging over window B should bring it forward (spring-loading): hovering a background Ferail window during a drag raises it after a short delay so the user can see the drop target. Hovering a background window's tab during a drag activates that tab after a short delay (spring-loaded tabs), so you can drop into any tab of any window.
- The dropped operation is scheduled on a shell worker exactly as in-window: it doesn't matter which window initiated it; the task registry is process-wide. Both source and destination directories reload across all windows (§2.3).
- Cross-window **move** within the same volume is still a move; cross-window does not imply cross-volume. Volume relationship is determined by the nodes, not the windows (selection/DnD spec §3.6).

### 5.2 Dragging out to Finder or other apps

- Ferail publishes standard macOS pasteboard flavors (file URLs, and promised-file flavors where the content may need producing). This is `ferail-shell-mac`'s job: the GPUI layer starts the drag with a `NodeId` payload; shell-mac translates to the platform pasteboard representation by resolving those nodes' paths at the controlled boundary.
- A drag-out that the OS resolves as a **move** (e.g. into a Finder window on the same volume) will result in the source files being moved by the *receiving* side. Ferail's file watcher sees the change and reloads the source view through the shell: Ferail doesn't perform the move itself in this case, it just reacts to the watcher. A drag-out resolved as **copy** likewise: the OS/receiver copies, Ferail's source view is unaffected.
- Promised files: if Ferail ever drags content that isn't a plain on-disk file (a saved-search result set, a synthesized item), it uses the promise mechanism and produces the file when the receiver asks, on a worker, not the UI thread.

### 5.3 Dragging in from Finder or other apps

- Inbound drops arrive as external paths on the pasteboard. They cross the shell-mac boundary, get registered in `NodeStore`, and the operation proceeds as a normal copy/move into the Ferail drop target (selection/DnD spec §3.7).
- The inbound drop targets are the same set as in-process drops: folder rows, empty space, tree nodes, volumes, favorites, breadcrumb segments, in any window.
- Modifier semantics for inbound drops follow the OS: dragging from Finder with Option = copy, etc. Ferail reads the drag's proposed operation from the pasteboard/drag info rather than imposing its own.

### 5.4 Distinguishing the three payload types

At any moment a drag in Ferail is exactly one of:

1. A **node drag**: originates from a file table row, tree node, breadcrumb, or volume entry. Payload: `NodeId`s.
2. A **tab drag**: originates from a tab item in the tab strip. Payload: a tab (ownership transfer, §3.5).
3. A **favorite drag**: originates from a Favorites entry. Payload: a favorite shortcut (reorder/tear-off, per the Favorites spec).

They're disambiguated entirely by **origin surface** at drag-start. They have different drop-target rules, different visuals, and never interconvert. An inbound *external* drag is always treated as a node drag (it carries paths → nodes).

A tab drag never leaves the process (you can't tear a Ferail tab into Finder). A favorite drag never leaves the sidebar's Favorites section as a *reorder*, though dropping a node onto Favorites is a node-drag drop, not a favorite drag. A node drag is the only one that crosses the app boundary in either direction.

---

## 6. Session persistence and crash recovery

### 6.1 What's persisted

Per the architecture doc, `ferail-meta` stores window/layout state, and persisted state is treated as cache/preference, never as filesystem truth. Session state to persist:

- The set of **windows**: each window's frame, window state (zoomed/fullscreen), and panel geometry.
- For each window, its **tabs** in order, and the active tab index.
- For each tab: its **current directory path**, sort, filter, and history (back/forward stack as paths). Selection and scroll are best-effort: persist if cheap, restore if still valid, never block on them.
- The **closed-tab stack** (recently closed tabs for `Cmd+Shift+T`): optional to persist across launches; at minimum it lives for the process lifetime.

Paths are persisted, not `NodeId`s: `NodeId`s are process-local and meaningless across launches. On restore, paths re-enter through `NodeStore` to get fresh `NodeId`s, exactly like an intent (§4.2).

### 6.2 When it's snapshotted

- Snapshots are written on a debounced schedule after meaningful changes (tab opened/closed/navigated/reordered, window opened/closed/moved/resized), and on clean quit. Debounced so a rapid sequence of navigations doesn't hammer SQLite.
- Writing the snapshot is shell work, off the hot path. It serializes already-cached state (paths, indices, geometry), no filesystem queries needed to produce a snapshot.

### 6.3 Restore behavior

- **Clean relaunch** (user quit, then reopened): preference-driven. Default recommendation: **restore the previous session**: all windows, tabs, locations. Alternative preference: open a single fresh window. macOS's own "reopen windows when logging back in" should also be honored.
- **Crash relaunch**: the architecture doc already describes a panic hook and crash report path in `obs.rs`. On a relaunch following a crash, offer to restore the prior session (or restore automatically + show a non-blocking "restored from unexpected quit" notification). Each restored tab navigates via normal streaming enumeration; a path that no longer exists restores as a tab showing a clean "this folder is no longer available" state (reuse the missing-target handling pattern), not a crash and not a blocking error.
- Restore must be **non-blocking and incremental**: windows and tab strips appear immediately with their structure; each tab's contents stream in via the normal enumeration path. The architecture's prime directive holds during restore, no synchronous directory reads to "rebuild" the session.
- In-progress file operations are **not** restored across a quit or crash: a process exit ends its workers. The task registry starts empty on launch. (A future enhancement could journal operations for resumability; explicitly out of scope for v1.)

---

## 7. Component / crate mapping

- **GPUI windows** + `Root` per window: the window model (§2). Each window is its own GPUI window with its own view tree.
- **gpui-component TitleBar**: hosts the tab strip, global nav, filter input, per the architecture doc.
- **gpui-component Tabs**: the tab strip rendering, active state, close buttons, overflow. Tab reorder/tear-off uses GPUI drag-and-drop on top.
- **gpui-component Menu**: tab context menus (close others, close to right, duplicate), the "Merge All Windows" app-menu item, dock menu.
- **gpui-component Notification**: "progress continues in background" when a window with the task UI closes, "session restored after unexpected quit", drag-out/in errors.
- **`ferail-shell-mac`**: the singleton mechanism (Apple Events, `NSApplication` reopen/open-files), the OS pasteboard for cross-app DnD (§5.2–5.3), dock-icon behavior, native window state.
- **`ferail-meta`**: session/window/tab persistence (§6).
- **The shell** (`ferail-gpui::Shell`): owns the window list, the process-wide task registry/watcher/caches, the intent dispatcher, the cross-window reload fan-out.
- **`ferail-core`**: the command catalogue entries for all window/tab commands (`Cmd+N`, `Cmd+T`, `Cmd+W`, tab cycling, merge windows), so app actions, macOS menu items, and future command-palette share one identity layer.

---

## 8. Implementation order

1. **Single-process singleton** (§1.2): make a second launch forward an intent and exit. New launches open a window in the primary. Get this right before anything else; it's the foundation.
2. **The window model** (§2): shell owns a list of windows; `Cmd+N` opens one; close removes one; process stays resident at zero windows.
3. **Basic tabs** (§3.1–3.3, minus tear-off): a window owns ordered tabs, `Cmd+T` / `Cmd+W` / `Cmd+1–9` / cycle, the tab strip UI, new-tab button, overflow.
4. **Tab ownership of state**: confirm each tab independently owns directory/history/filter/sort/selection/scroll, and tab switching swaps the file table's model cleanly. (Much of this exists already per the architecture doc; this step is wiring it to the tab strip.)
5. **Cross-window reload fan-out** (§2.3): a file op or watcher event reloads matching directories in *all* windows/tabs, each reconciling its own selection.
6. **Closed-tab stack + reopen** (§3.3) and **tab reorder** within a strip (§3.3).
7. **Tear-off and merge** (§3.5): tab ownership transfer between windows and to new windows. Depends on 2–4 being solid.
8. **Cross-window node DnD** (§5.1), including spring-loaded window raise and spring-loaded tabs.
9. **External DnD out and in** (§5.2–5.3), via shell-mac pasteboard. Drag-out-as-move reacting through the watcher; drag-in registering paths through NodeStore.
10. **Intents from CLI / Finder / `open`** (§4): path-bearing intents, the new-tab-vs-new-window preference.
11. **Session persistence and restore** (§6): snapshot on debounce + clean quit; incremental non-blocking restore; crash-relaunch restore offer.
12. **Polish**: "Merge All Windows", duplicate tab, dock menu, the per-window vs app-global panel-geometry preference, pinned tabs (if ever).

---

## 9. Acceptance checklist

Process & windows:
- [ ] A second launch (dock/Finder/CLI/`open`) never starts a second process; it forwards an intent and the primary handles it.
- [ ] Dock icon with windows open activates and brings forward; with no windows open, opens one.
- [ ] The process stays resident with zero windows; `Cmd+Q` quits.
- [ ] `Cmd+N` opens a new window, cascaded from the frontmost.
- [ ] Closing a window with an in-progress file operation does not cancel the operation; progress stays visible somewhere.
- [ ] The same directory open in two windows reloads in both on any change; each reconciles its own selection.
- [ ] Caches/watcher are shared: the second window showing a warm directory is fast.

Tabs:
- [ ] `Cmd+T` new tab, `Cmd+W` closes active tab, closing the last tab closes the window, `Cmd+Shift+W` closes the whole window.
- [ ] `Cmd+1–9`, next/previous tab cycling, all work.
- [ ] `Cmd+Shift+T` reopens the most recently closed tab with its directory/history/sort/filter.
- [ ] Tabs reorder by drag within the strip with an insertion indicator.
- [ ] Tab overflow is handled (shrink-then-scroll, active tab kept in view).
- [ ] Each tab independently owns directory, history, filter, sort, selection, scroll.
- [ ] Switching tabs does not cancel an inactive tab's in-flight enumeration; it completes and is ready on return.
- [ ] An inactive tab whose directory changed externally is correct when switched to.

Tear-off & merge:
- [ ] Dragging a tab off the strip creates a new window owning that tab; the source window closes if empty.
- [ ] Dragging a tab onto another window's strip moves it there at the drop index, intact (history/selection/scroll preserved, no re-enumeration).
- [ ] "Merge All Windows" collapses all tabs into one window in order.
- [ ] Tab drag and node drag never get confused: distinguished by origin surface.

Cross-window & cross-app DnD:
- [ ] Dragging nodes from one Ferail window to another works with `NodeId` identity intact (no path round-trip).
- [ ] Spring-loaded: hovering a background window during a drag raises it; hovering a background tab activates it.
- [ ] Cross-window drop schedules a process-wide shell worker; source and destination reload in all windows.
- [ ] Dragging files out to Finder works (move and copy both, per OS modifiers); source view reloads via the watcher when it was a move.
- [ ] Dragging files in from Finder registers paths through NodeStore and runs as a normal copy/move into the drop target, in any window.
- [ ] All three drag payload types (node, tab, favorite) are correctly distinguished and never interconvert.

Intents & session:
- [ ] A folder opened from Finder/CLI lands per the new-tab-vs-new-window preference.
- [ ] Path-bearing intents register through NodeStore; the launcher's identity is irrelevant.
- [ ] Session (windows, tabs, locations, geometry) snapshots on debounce and on clean quit, off the hot path.
- [ ] Clean relaunch restores the session per preference; restore is incremental and non-blocking, contents stream in.
- [ ] A restored tab whose folder no longer exists shows a clean unavailable state, not a crash or a block.
- [ ] After a crash, relaunch offers (or performs + notifies) session restore.
- [ ] In-progress file operations are not expected to survive a process exit; the task registry starts empty.
