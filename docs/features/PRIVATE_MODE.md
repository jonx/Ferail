# Private Mode (capture-safe presentation lock)

← [Feature notes](README.md) · [Screenshot harness](SCREENSHOTS.md) ·
[Diagnostics privacy](DIAGNOSTICS.md) · [Architecture](../ARCHITECTURE.md)

**Status: implemented: semantic live projection, August 2026.** Private Mode is a process-wide,
session-only presentation lock for making screenshots of a real Ferail session
without displaying personal names, paths, content or identifying metadata. It
is not a second file-browser mode and it never changes the filesystem model.

The user prepares the exact screen to capture, activates Private Mode, and the
whole Ferail process becomes read-only from an interaction perspective. The
only Ferail actions that remain available are:

- click the highlighted title-bar shield again;
- dispatch `view.toggle_private` again (`Cmd+Shift+K` on macOS,
  `Ctrl+Shift+K` elsewhere);
- press `Escape`;
- close a window or quit Ferail.

Everything else is blocked until Private Mode is left. This deliberately
avoids defining what rename, delete, navigation, selection, drag/drop or any
future feature should do while the user is looking at disguised data.

`--screenshot` enables Private Mode by default. Capturing real data requires an
explicitly alarming `--unsafe-real-data` override.

The first generic replacement surface was intentionally removed: it protected
the data but made screenshots useless. The shipped implementation now renders
the real prepared interface and sends semantic values (`Leaf`, `Path`,
`Label`, `Digest`, byte counts, timestamps and dimensions) through the
process-session presenter at paint time. Personal content pixels become
same-size placeholders. A transparent root shield freezes the prepared UI,
and the native menu is reduced to Private Mode and Quit. It paints no badge,
button, glyph or message of its own: the existing title-bar shield is simply
highlighted. Only visible controls are projected, so entering the mode never
walks a Flat View with millions of rows.

---

## 1. Promise and boundary

### 1.1 What Ferail promises

When Private Mode reports itself **active**, every pixel rendered inside every
Ferail-owned window must be safe to share:

- real file/folder names and paths are replaced with plausible deterministic
  aliases;
- user-written labels, search/filter text, volume/device names, host/account
  names and process-user names are replaced;
- thumbnails, image/PDF/video/audio content, text/code/NFO contents and viewer
  frames are not painted;
- EXIF, GPS, media tags, Finder tags, owner/group data, source URLs, custom
  tool paths, checksums and other identifying values are replaced or hidden;
- values derived directly from private files (sizes, dates, dimensions,
  similarity distances and hashes) use deterministic presentation values;
- window titles, tabs, breadcrumbs, tooltips, overlays, toasts, task labels,
  drag ghosts and all other Ferail-rendered chrome obey the same policy;
- a semi-transparent Viewer or window is forced fully opaque so the pixels
  behind Ferail cannot show through it.

The original models remain untouched. Opens, joins, refreshes and file
operations continue to use raw names after Private Mode is left.

### 1.2 Deliberate boundary

Ferail does not promise to sanitize pixels owned by the operating system or by
another process. The user remains responsible for excluding these from a
screen capture:

- the macOS menu bar, Dock and desktop;
- the Windows taskbar, desktop and third-party native Shell menus;
- native Properties/Open/Save dialogs and external applications;
- another application visible beside or behind the Ferail window;
- the screenshot filename or destination chosen by the user.

This is a clear renderer-ownership boundary, not an attempt to compensate for
capturing the whole desktop carelessly. Ferail-controlled transparency is
inside the boundary and is therefore disabled while private.

### 1.3 What this is not

- It is not diagnostics redaction. `redact.rs` keeps its destructive,
  share-a-report contract and remains enabled independently.
- It is not filesystem virtualization. No fake rows enter `FileEntry`,
  `NodeStore`, search results, the metadata DB or a file-operation request.
- It is not persisted. Ferail always launches with Private Mode off.
- It is not a security boundary against memory inspection. Raw models remain
  in process so the application can resume instantly when the mode is left.

---

## 2. Settled product behaviour

### 2.1 Entering

Private Mode is entered from its command/menu item or visible icon. Entry is
global to the process: Shell, Settings, Viewer and every other Ferail window
switch together and use the same presentation session.

Entry must be fail-closed:

1. create or reuse the process-session disguise key;
2. set the global state to `Arming(generation)`;
3. install the interaction gate before requesting repaints;
4. suppress thumbnail/preview content and Viewer frames at paint time;
5. force Ferail-owned transparent windows to opacity 1.0;
6. repaint every Ferail window through the private presenter;
7. advance the generation on the next UI turn;
8. change the state to `Active(generation)` and highlight the title-bar shield.

While `Arming`, roots already use the private presenter. The screenshot harness
waits for `Active(generation)` and a settled render before capturing.

Ferail-rendered dialogs, result panels and popovers stay in their prepared
state so they can be captured. They are frozen by the interaction gate and
their values are presented privately. Native/external windows remain outside
the promise above.

### 2.2 While active

Private Mode is an **interaction lock**, not a snapshot of background state.
Workers already running may complete, watchers may deliver changes and task
progress may advance, but every resulting paint remains private. Activating
the mode starts no new filesystem work.

Blocked inputs include:

- pointer clicks, double-clicks, hover tooltips and context menus;
- keyboard navigation, type-to-select and ordinary shortcuts;
- scrolling and zooming inside Ferail views;
- selection changes, inline editing and text-field input;
- inbound and outbound drag/drop;
- application-menu commands and programmatic GPUI actions;
- all navigation, viewer, mutation, clipboard and tool actions.

Allowed inputs are deliberately tiny:

- the existing highlighted title-bar shield, whose measured bounds are the
  interaction shield's sole in-app pointer exception;
- `view.toggle_private` (`Cmd/Ctrl+Shift+K`);
- `Escape`, intercepted at the root capture phase and consumed;
- window close and application quit. Closing the final window may terminate
  Ferail normally while Private Mode is active.

OS window movement, resize and minimize remain available. A resize may repaint
the private presentation but cannot dispatch a Ferail feature.

### 2.3 Leaving

Clicking the highlighted title-bar shield, dispatching `view.toggle_private`,
or pressing `Escape` leaves the mode for all Ferail windows. Exit is explicit,
so real data may appear on the next paint.

On exit:

- retain only the process-session key (there is no whole-model alias cache);
- restore prior per-window opacity;
- resume ordinary Viewer and viewport preview painting;
- remove the interaction gate and repaint all windows.

No state is written to app settings. Relaunch always starts non-private.

### 2.4 Visible affordance

The normal title-bar shield is the only Private Mode affordance. It is
unselected while off and highlighted while arming or active. Its actual painted
bounds are remembered from the preceding frame and form the interaction
shield's sole in-app pointer exception, so clicking the same icon toggles the
mode off without exposing any neighboring title-bar command. There is no
duplicate badge, floating exit button, placeholder shield glyph or explanatory
message in the captured interface.

No “content visible but names hidden” sub-mode exists. Showing real content
means leaving Private Mode. Synthetic screenshot fixtures remain the way to
demonstrate a thumbnail/photo feature without exposing a personal library.

---

## 3. Architecture

```text
raw filesystem / metadata models
              │
              ├── operations, sorting, filtering ──► raw values (unchanged)
              │
              └── render boundary
                     │
                     ▼
              PrivatePresenter ──► safe strings / values / placeholders

pointer, keyboard, menu, DnD, GPUI actions
                     │
                     ▼
              PrivateActionGate ──► default deny
                                      allow: exit / Escape / close / quit

screenshot arguments ─► prepare real UI ─► arm privacy ─► safe-paint ack
                                                     └──► capture PNG
```

### 3.1 Pure core presenter

Add a platform-neutral pure module, preferably
`ferail_core::private_presentation`. It does not consult GPUI and owns no
process-global toggle.

Conceptual API:

```rust
pub struct PrivateSession {
    key: [u8; 32],
}

impl PrivateSession {
    pub fn leaf(&self, raw: &str, kind: PresentedKind) -> Cow<'_, str>;
    pub fn path(&self, components: &[PresentedComponent]) -> String;
    pub fn label(&self, raw: &str, kind: PresentedKind) -> Cow<'_, str>;
    pub fn digest(&self, raw: &str, width: usize) -> String;
    pub fn bytes(&self, identity: u64, raw: u64) -> u64;
    pub fn timestamp(&self, identity: u64, raw: i64) -> i64;
    pub fn dimensions(&self, identity: u64, raw: (u32, u32)) -> (u32, u32);
}
```

The exact signatures may use existing display-path types rather than allocate
a component vector. What matters is that the session/key is explicit and the
core remains deterministic and testable.

The key is generated once per Ferail process, never persisted, logged or sent
to diagnostics. A keyed hash chooses aliases and fake values. The same raw
component therefore has the same presentation in rows, breadcrumbs, window
titles and tool panels for the lifetime of that process; a later launch has a
different mapping.

### 3.2 GPUI global and lifecycle

`ferail_gpui::private_mode` contains:

- `PrivateModeState::{Off, Arming { generation }, Active { generation }}`;
- an `Arc<PrivateSession>` created for the current process;
- render-time semantic presentation helpers;
- entry/exit hooks shared by all Ferail window types;
- the action allowlist and root interaction shield;
- test-only leak canaries and generation acknowledgements.

The GPUI Global mirrors the live-update mechanism used by theme and thumbnail
preferences. Every render root reads it so changing state invalidates all
windows. The session is passed explicitly to core presentation functions.

### 3.3 Raw/display compiler chokepoint

`FileEntry.name` remains the on-disk operation truth and `display_name` remains
the ordinary user-facing string. Visible GPUI code requests a projected value
from `PrivatePresenter`; operation and model code never consults Private Mode.
Call sites are classified as one of:

- **operation/indexing**: raw display name is correct;
- **rendered text**: must use the presenter;
- **structured message**: present sensitive arguments before formatting;
- **test fixture**: state explicitly whether raw or private output is under
  test.

Sorting, filtering, type-ahead and file operations continue to use raw data.
Private Mode locks their interaction anyway. Do not rebuild sort keys from
aliases and do not iterate a million-row surface merely to enter the mode.

Other raw render paths need the same explicit audit: `Path::display`, path
lossy conversions, favorite/location labels, `NodeStore::display_name`,
archive-member names, Disk Usage labels and platform namespace DTOs.

### 3.4 Bounded cache and millions-first rule

Entering Private Mode must be proportional to visible UI, not model size.

- Compute aliases only for controls painted in active windows and their normal
  virtualization margin. The keyed presenter is allocation-bounded by that
  paint and owns no model-sized cache.
- Never attach a fake string to every `FileEntry`, path-arena node or Disk
  Usage node.
- Never re-sort or re-filter a surface when entering/leaving.
- The off path returns ordinary display values and does no keyed hashing.

The Flat View regression gate remains four million rows. Private activation
must have bounded time and memory independent of those four million rows.

### 3.5 Name and path rules

Aliases are plausible rather than destructive ellipses, while still avoiding
recoverable structure:

- choose words from a bundled neutral ASCII wordlist using the keyed hash;
- use enough word combinations to make visible collisions negligible;
- preserve a leading dot for hidden files;
- preserve only the final recognized extension and an explicit whitelist of
  compound extensions such as `.tar.gz`; never treat arbitrary suffix chains
  as safe (`alice.smith.pdf` must not preserve `.smith.pdf`);
- retain only a coarse length class so truncation and column layout remain
  representative;
- preserve filesystem anchors by syntax, not by raw text;
- mask UNC server/share names, volume labels, account names, WSL distribution
  names and device names;
- preserve well-known folders only when Ferail already knows their semantic
  identity. Do not whitelist a component merely because its text happens to be
  “Documents”.

Aliases must contain no substring copied from a private component except an
approved extension. Non-UTF-8 Unix names and raw UTF-16 Windows names continue
to use the existing display conversion before presentation; operations retain
their native identity unchanged.

### 3.6 Structured values, not free-form guessing

A generic `text(&str)` can be a backstop but cannot be the privacy guarantee.
It cannot reliably recognize a name embedded in prose or distinguish a user
query from ordinary UI copy.

User-controlled messages must remain structured until their sensitive fields
have passed through `PrivatePresenter`, for example:

```text
Copying {presented_name} to {presented_folder}
```

Do not format the raw message and then try to find the path with a regex.
Filter/search text, clipboard-derived checksum fields and arbitrary provider
error text should become generic private placeholders rather than be parsed.

### 3.7 Default-deny action gate

The lock needs defence in depth because a top-level pointer overlay does not
stop native menu actions or programmatic GPUI dispatch.

1. A root capture-phase shield consumes pointer, keyboard, wheel and DnD input.
2. The shared command/action dispatch rejects everything while private unless
   its policy is explicitly `PrivateAllowed`.
3. Buttons/menu items render disabled underneath the shield.
4. Navigation and filesystem-mutation entry points reject a programmatic call
   while private as a final invariant.

The default for every existing and future action is **blocked**. The only
allowlist entries are toggle-private, Escape handling, window close and quit.
Adding a command must not silently enlarge this list.

---

## 4. Surface inventory

### 4.1 Names, paths and user text

Audit at minimum:

- list, grid, tree, rows, column export, tooltips and drag ghosts;
- breadcrumbs, editable path, Go to Folder, completions and filter/search;
- tabs, OS window title and Window-menu title;
- favorites, recents, platform locations, WSL/MTP devices and volume names;
- status bar, task panel, toasts, progress and collision dialogs;
- Search, Flat View, Duplicate/Similar Images and Disk Usage;
- archives, bulk-rename before/after, checksum/SFV and ant-trail surfaces;
- Get Info, lock info, diagnostics, reporter and Settings custom paths;
- Viewer playlist/title and every secondary Ferail window.

### 4.2 Content pixels

Private Mode overrides `ShowThumbnails`; the setting itself is not changed.
No content provider result may paint:

- Quick Look/Explorer thumbnails and grid images;
- image/PDF/text/code/Markdown/NFO preview;
- video poster frames, audio cover art and native preview-handler captures;
- Viewer image/video frames;
- archive member preview and similar-image thumbnails.

Use generic type icons and a reusable private-content placeholder. Existing
memory-only caches need not be destroyed, but new content requests should stop
while private and cached pixels must not survive one stale paint.

**Stand-in blurs are not an exception to any of that.** Thumbnail surfaces (the
list, the icon grid, the preview pane's image box) paint a blurred picture
instead of a flat grey box, and every pixel of it is invented:
`PrivateSession::thumb_pixels` synthesises a tiny image from the session key
and the row's identity, which `private_thumb` pushes through the same ThumbHash
round trip a real placeholder takes. No byte of the file is read, so this is
not a content provider result and §4.2 holds unchanged.

Why bother: Private Mode exists to *publish* a capture. A grid of identical
grey boxes protects the data by deleting the feature, and a screenshot meant to
show what the grid looks like then shows nothing. A blur that came from
nowhere keeps the shape of the answer without any of the answer.

Three properties make it safe to publish, and the third is the one that
matters: it is stable within a session (a row does not flicker between
frames), it is different per row (neighbouring files look like different
pictures), and it is **keyed per session**, so the same file captured twice is
two unrelated blurs. Nothing can be correlated between two screenshots, and
nothing can be matched back to a file. The cache is dropped on leaving the
mode.

Scope is deliberate: surfaces that paint *nothing* while private, the text
editor's document stage among them, stay that way. A stand-in belongs only
where a picture belongs.

### 4.3 Identifying metadata

Replace real values while preserving enough shape to demonstrate the UI:

- file/folder sizes, dates, dimensions and item counts where identifying;
- SHA/CRC/checksum values and verification expected/actual fields;
- similarity hash distances and image dimensions;
- EXIF camera/lens/date/GPS, media title/artist/album and source URL;
- owner, group, permissions identity, lock process/user/PID;
- Finder tags and custom favorite labels;
- volume capacity/free space, host/account names and configured tool paths;
- diagnostics paths, database/config locations and provider details.

For a complex pane that cannot yet present every value safely, render its
normal structure with generic fixture values or an opaque “Details hidden in
Private Mode” body. It must never fall back to real values. Private Mode is not
user-facing until every reachable Ferail surface is either transformed or
fail-closed in this way.

Disk Usage geometry and aggregate tool layouts require an explicit private
presentation. Do not duplicate the raw million-node model. Prefer a lightweight
display projection with keyed fake weights for the currently rendered layout;
until that exists, use a synthetic private placeholder for the treemap.

### 4.4 Transparency and live media

Viewer opacity is a personal-data channel even when Ferail's own content is
hidden. On entry, record each Ferail window's prior opacity and force 1.0.
Restore it on exit. Pause slideshows and video/audio playback before the safe
paint and remember whether playback should resume.

Always-on-top can remain enabled: it changes z-order, not Ferail-rendered
pixels. The documented boundary still requires the user to capture the Ferail
window rather than unrelated screen area.

---

## 5. Screenshot harness integration

### 5.1 Secure default

Add:

```text
--unsafe-real-data    Disable Private Mode for this screenshot. The resulting
                      PNG may contain names, paths, content and metadata.
```

There is no `--demo` opt-in. `--screenshot` means private unless this explicit
override is present.

### 5.2 Ordering

The harness cannot enable the interaction lock before applying its requested
state. Its lifecycle is:

1. parse arguments and create the window/model;
2. navigate, seed fixtures, choose rows/tabs and open requested Ferail panels;
3. wait for the existing data/metadata settling policy;
4. arm Private Mode without accepting further ordinary input;
5. wait for the target root to acknowledge the private generation;
6. capture only after one complete protected paint;
7. encode a PNG with no text/path metadata and exit.

On Windows, where the PrintWindow route briefly presents a real HWND, the HWND
must not be shown until Private Mode is armed. The first visible presentation
must already be the private one.

The generation acknowledgement replaces reliance on an arbitrary sleep for
privacy correctness. A timeout fails the screenshot command; it never captures
an uncertain frame.

### 5.3 Synthetic feature captures

Real thumbnails/content are never an escape hatch inside Private Mode. Feature
tour images that need photos, videos, NFO text, checksums or metadata use the
existing seeded screenshot-state mechanism with repository-owned fixtures.
Those fixtures still pass through the private presenter so the production path
is exercised.

---

## 6. Implementation sequence

Each phase may land as an isolated commit, but the command/toggle stays hidden
until the release gate in Phase 6 passes.

### Phase 0 - inventory and canary corpus

- Freeze this contract and enumerate every render root/action-dispatch path.
- Add private canary fixtures: unique names, paths, favorite/volume/account
  labels, query text, EXIF/GPS/media data, SHA/CRC, text content and unmistakable
  image pixels.
- Record the four-million-row activation/memory baseline.

### Phase 1 - core presenter and lifecycle

- Add the pure `PrivateSession` and deterministic presentation tests.
- Add the GPUI Global, `Off/Arming/Active` state and bounded LRU.
- Add `view.toggle_private`, the title-bar toggle icon and en/de/fr strings.
- Implement multi-window repaint/generation acknowledgement.
- Keep the entry point hidden from ordinary users.

### Phase 2 - interaction lock

- Add the reusable root interaction shield to Shell, Settings, Viewer and all
  secondary Ferail windows.
- Add default-deny command dispatch and operation/navigation backstops.
- Allow only toggle/Escape/close/quit.
- Add opacity and playback entry/exit hooks.

### Phase 3 - names, paths and structured messages

- Rename `FileEntry.display_name` to `display_name_raw` and classify all broken
  call sites.
- Route every visible path/name through `PrivatePresenter`.
- Convert task/toast/dialog builders away from raw free-form formatting.
- Audit native OS window titles separately from custom title bars.

### Phase 4 - content and metadata

- Override thumbnails, previews, viewer and preview-handler pixels.
- Present or fail-close Get Info, diagnostics, Settings, checksum/SFV,
  archive, duplicate/similar and Disk Usage values.
- Ensure caches never paint a stale content frame after arming.

### Phase 5 - screenshot secure default

- Add `--unsafe-real-data` and the prepare→arm→ack→capture lifecycle.
- Make Windows' first visible screenshot frame private.
- Verify PNG output contains no text chunks or raw canary bytes.
- Update screenshot examples and documentation.

### Phase 6 - enforcement and public enablement

- Run the complete surface/action/multi-window canary matrix.
- Run macOS, Windows and Linux screenshot qualification.
- Pass four-million-row performance/memory gates.
- Update `ICONS.md`, CHANGELOG and the feature tour.
- Only then expose the Settings/menu command in normal builds.

---

## 7. Verification plan

### 7.1 Pure presentation tests

- same key + same input gives the same alias;
- different process keys do not correlate;
- aliases contain no original substring beyond an approved extension;
- hidden and recognized compound-extension rules are exact;
- arbitrary multi-dot names do not leak middle suffixes;
- aliases remain plausible across short/medium/long inputs;
- collisions are absent in a large deterministic corpus;
- POSIX, Windows drive, UNC, WSL and non-Unicode display cases are safe;
- fake hashes have the correct visual width but never equal the real digest;
- fake numeric values are stable, bounded and do not overflow.

### 7.2 Interaction tests

With Private Mode active, dispatch every registered command and prove that only
the allowlist changes state. Explicitly exercise:

- mouse, double-click, wheel, keyboard navigation and type-to-select;
- toolbar/sidebar/status controls and in-app context menus;
- rename, copy, move, trash, paste, checksum generation and archive actions;
- drag source and drop target paths;
- viewer navigation, zoom, playback and opacity controls;
- application menus and direct programmatic action dispatch;
- `Escape`, exit icon, close-window and quit success;
- multiple windows entering and leaving as one process-global state.

Existing workers may complete, but their private labels must remain safe and
they must not accidentally remove the interaction lock.

### 7.3 Surface canaries

Seed a unique secret token into every class of visible source and assert it is
absent from private label-builder output and final captures. Cover at least:

- row/tree/grid/breadcrumb/tab/window/favorite/location labels;
- filter, completion, toast, task and error text;
- archive, bulk rename, Search/Flat, duplicate/similar and Disk Usage;
- Get Info, diagnostics, Settings and lock information;
- SHA/CRC, EXIF/GPS/media values and volume/account identifiers;
- thumbnail, preview and Viewer pixel canaries.

Use pure builder tests where possible. Add raster checks for the image canary
and a test-only presentation audit hook for rendered sensitive strings; OCR is
not the primary enforcement mechanism.

### 7.4 Screenshot tests

- `--screenshot` without extra flags is private on all platforms;
- `--unsafe-real-data` is the only opt-out and is documented as unsafe;
- a screenshot waits for the matching protected-paint generation;
- a timeout exits non-zero without creating a misleading PNG;
- Windows never presents an unprotected first HWND frame;
- PNG chunks contain pixels only, with no embedded source path/name metadata;
- repeated captures in one process are stable, while a new process changes
  aliases.

### 7.5 Performance and memory gates

- Private entry/exit does no filesystem I/O.
- Four-million-row Flat View entry time depends on visible rows, not total rows.
- No whole-model alias cache, resort, refilter or cloned path arena appears.
- LRU capacity is asserted in tests and memory is released on exit.
- Warm private renders add no unbounded allocation per frame.
- The ordinary non-private render path shows no measurable regression.

---

## 8. Release gate

Private Mode is ready only when all of the following are true:

- every Ferail render root subscribes to the process-global state;
- every reachable surface is transformed or explicitly fail-closed;
- all new/existing commands are blocked by default while active;
- only exit/Escape/close/quit pass the allowlist;
- no raw canary text, metadata, digest or content pixel appears in final PNGs;
- activation has bounded viewport-scale cost at four million rows;
- the mode is never persisted and no alias/key/cache enters diagnostics or DB;
- `--screenshot` is private by default on macOS, Windows and Linux;
- the renderer-owned versus OS/external boundary is stated in user-facing help.

Until this entire gate passes, the feature remains hidden and screenshots of
real personal data must not be described as safe.
