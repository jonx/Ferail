# Changelog

Notable changes to Ferail, newest first. This tracks what you'd notice as a
user; the full detail lives in the git history. Dependency-pin bumps are logged
separately in [CHANGELOG-DEPS.md](CHANGELOG-DEPS.md).

**Unreleased** collects work not yet in a tagged build.

## Unreleased

- **Private Mode shows blurred stand-in thumbnails instead of grey boxes.**
  The mode exists so you can publish a screenshot of a real session, and a
  grid of identical grey rectangles protected the data by deleting the very
  thing the screenshot was meant to show. Files now get a soft blur in the
  list, the icon grid and the preview pane. **None of it comes from your
  files**: every pixel is invented from a key created when Private Mode
  starts, so the same photo captured twice gives two unrelated blurs, and
  nothing in the picture can be traced back to anything on your disk.

- **Ferail speaks Polish.** A complete Polish translation now ships inside the
  app, contributed by **Bohun**. Pick it in
  Settings, or let Ferail follow your system language. One rough edge, worth
  knowing: counts of two, three and four items currently use the same wording
  as five and above, so a few labels read slightly off until the pack gains
  its `few` forms.

- **The Windows Recycle Bin shows where each item was deleted from.** A second
  column on every row gives the original folder, the way Explorer does, so you
  can tell two files with the same name apart before restoring either. Windows
  records this itself, so it works for everything in the bin whatever put it
  there. Rows in that view also had no icon at all, from a wrong asset path;
  they do now.

- **The Trash has its own right-click menu, with Put Back.** Browsing the
  Trash used to offer the ordinary file menu, which made no sense there: it
  proposed renaming, duplicating, compressing and tagging things you had
  thrown away, and *Move to Trash* on items already in it. The menu is now
  short and about deleted items: Open, **Put Back**, Get Info, Quick Look,
  Reveal in Finder, Copy Path, Delete Immediately and Empty Trash. Right-click
  on empty space there offers Select All, Empty Trash and Refresh, and no
  longer offers to create a folder in your Trash.

- **Put Back returns an item to where it came from.** Ferail remembers the
  original location of everything it moves to the Trash, and puts it back
  there, recreating the folder if it has gone since. It never overwrites: if
  something else took the name, it says so instead of replacing it. One limit,
  stated plainly: Ferail can only put back what *it* trashed. macOS keeps the
  Finder's own put-back information in a private store that Ferail does not
  read, so an item trashed by Finder reports that its original location is
  unknown rather than guessing. (On Windows the Recycle Bin restores through
  Windows' own command, which needs no such record.)

- **You can rearrange the context menus and turn off the entries you never
  use.** Settings ▸ Menus lists every entry the right-click menu shows on a
  file or folder, and the one on empty space, in the order they appear. Each
  has a switch, rows can be dragged into whatever order you like, and
  separators are rows too: drag them where you want a gap, remove the ones you
  do not, and **Add Separator** makes a new one. Hiding an entry changes
  nothing about what Ferail can do: the command keeps its keyboard shortcut and
  stays in the command palette, and entries still appear only where they apply,
  so turning one back on does not make it show up on files it cannot act on.
  Open and Get Info cannot be hidden, and **Reset This Menu** puts a menu back
  the way it shipped. A command added in a future version lands next to the
  entry it was designed to follow rather than at the bottom of your
  arrangement.

- **Closing Ferail now actually ends it, and says why when it does not.**
  Some closes left the process running with no window and no taskbar icon, so
  the only way to install an update was to kill it by hand. Ferail exits when
  its last window is gone, and an orphaned sub-window could keep that from ever
  happening. Quitting now starts a watchdog: if the process is still alive a
  few seconds later it writes `reports/ferail-shutdown-<pid>-<n>.txt` naming
  the windows that outlived the quit, and then exits on its own rather than
  leaving you to Task Manager. That report is written however you started
  Ferail, so if this happens to you, attach it: Settings → Diagnostics shows
  which folder it is in. To keep a stuck process alive for debugging instead,
  start Ferail from a terminal with `FERAIL_NO_SHUTDOWN_EXIT=1` set.

- **The Windows Recycle Bin can restore again, and right-click works there
  at all.** Browsing the Recycle Bin showed its contents but the context menu
  came up empty, so the only thing you could do with it was empty it: Ferail
  offered the Shell menu only to rows without a file path, and a deleted item
  has one. Every namespace row now gets the native menu, and a **Restore**
  entry sits above it, which puts the selection back where it came from with
  its original name, dates and permissions. Restoring uses Windows' own
  restore command, so an item it refuses to put back says so instead of
  failing silently.

- **Double-clicking a checksum file now runs the check.** Opening an `.sfv`,
  `.md5`, `SHA256SUMS` or similar list in a text editor shows a column of
  hashes, which is rarely what you wanted from it: the verification report
  opens instead, with the same per-file OK / mismatch / missing outcomes as
  *Verify Checksums…*. Open and Open With still open the file as text.

- **Entering a folder now previews the item it selected for you.** Ferail
  selects the first row when you open a folder, but nothing asked for that
  row's preview, so the pane showed the file's details next to an empty
  thumbnail well until you clicked the row that was already selected. The
  preview is now requested with the selection.

- **Long text previews show a scrollbar.** The preview pane's text box stops
  at a fixed height so a long `.nfo`, log, or source file cannot bury the
  file details underneath it, but nothing on screen said the file continued
  past the last visible line. A vertical scrollbar now sits on the box
  whenever its content overflows, and stays visible rather than fading out.

- **Ferail holds noticeably less memory over a long session.** The internal
  table that gives every file a stable identity stored each path twice; it
  now shares one copy, which cuts it from 241 to 144 bytes per path (about
  90 MB less per million files seen). The table still grows for the life of
  the process, so a very long session or a recursive scan of a huge tree can
  still accumulate: that groundwork is tracked, and Ferail now records the
  size in its issue reports so the growth is visible.

- **Double-clicking a selected folder opens it again instead of renaming
  it.** Clicking the name of an already-selected item starts a rename, and
  that gesture was firing on the first click of a double-click, so the name
  editor appeared and swallowed the second click. The rename now waits out
  the double-click interval before it starts, and a double-click cancels it.
  Click, pause, click still renames, as before.

- **The built-in text editor already had find and replace; now you can see
  them.** Cmd+F searches and Cmd+Shift+F replaces (Ctrl+F and Ctrl+H
  elsewhere), with previous/next, a match counter, a case toggle and Replace
  All, and they have worked since the editor shipped: nothing in the window
  pointed at them. The toolbar now has a button for each. Escape also stops
  closing the whole window when the find panel is open: it closes the panel,
  and a second Escape closes the window.

- **The text editor can reload a file from disk, and tells you where you
  are.** A **Reload from Disk** button re-reads the file, keeping your place
  in it, and asks first if you have unsaved edits: useful when something else
  changed the file while it was open. A new strip along the bottom shows the
  line and column of the cursor, how many lines the file has, and its
  encoding and line endings, so a file that came in as CRLF or with a BOM no
  longer hides that fact. The toolbar's new overflow menu turns line
  wrapping and line numbers on and off.

- **The command palette no longer leaks the file list underneath it.** With
  the shortcuts palette open (Cmd+/), hovering it showed the tooltip of
  whatever file row sat behind it, and the scroll wheel could go to that row
  instead of the palette. The palette now blocks the window beneath it, so
  hover, tooltips and the wheel all belong to the palette while it is up.
  Scrolling the command list itself with the wheel still stops short of the
  end; use the arrow keys to reach the last commands until that is fixed.

- **The Edit menu is no longer almost empty.** Cut, Copy, Paste and Move
  Items Here were on the keyboard and in the right-click menu but nowhere in
  the menu bar, which listed only Copy Path.

## 0.7.6 - 2026-08-30

- **Enter in a path or filter field now uses exactly what you typed.**
  Pasting a folder path that contains subfolders and pressing Enter used
  to append the first suggested subfolder, landing you somewhere you
  never asked for. Suggestions are now opt-in: Up/Down move the
  highlight, **Tab** accepts it, and Enter always goes to (or searches
  for) what the field actually holds. Applies to the breadcrumb path
  editor, the Go to Folder prompt, and the filter box.

- **Escape now unwinds Ferail's transient UI consistently.** It closes filter
  suggestions before clearing a query, cancels inline name/path edits, exits
  archive and other docked result surfaces, hides the preview pane, and closes
  standalone viewers, Get Info, archive, and built-in editor windows. Unsaved
  text, image, or archive edits still require an explicit Save / Discard /
  Cancel decision. Editor command-icon tooltips now show their shortcuts, and
  secondary windows are listed in the app's Window menu and removed from it as
  soon as they close.

- **Compact controls no longer hide their meaning.** The filter autocomplete
  popover can grow independently of the narrow title-bar field, and clicking
  the field's clear button closes the popover instead of reopening the empty
  syntax list. Tab close buttons now sit to the left of their labels, keeping
  the pointer at one fixed position while several tabs are closed in sequence.

- **Disk Usage and Flat View refresh selection details immediately.** Clicking
  a deeply nested file in Disk Usage's largest-files list highlights its
  nearest visible treemap ancestor when the exact leaf is below the drawing
  depth. Flat View now starts the visible metadata/description pass as soon as
  enumeration completes instead of waiting for the first scroll.

- **Disk Usage filters stay inside Disk Usage.** Typing or pressing Enter in
  the shared filter field now filters the already-scanned treemap instead of
  replacing it with a generic Search result. The full map remains available
  with non-matches dimmed, while a new icon toggle redraws the treemap and
  side list from matching files only. Filtering no longer restarts or waits
  for a scan to finish: a bounded queue briefly holds incoming facts while a
  debounced, cancellable background projection reads a stable snapshot, then
  resumes the same scan. Pending projections show the current map dimmed
  instead of an empty pane. The side list drops its redundant heading, and the
  macOS Full Disk Access action is now a compact icon with a tooltip.

- **Filter keyboard behavior is predictable again.** `type:` is accepted and
  autocompleted as a friendly alias for `kind:`. Token suggestions now remain
  available after a plain-name term and append the chosen criterion instead of
  replacing the current search. Escape closes token suggestions before clearing
  the query, and modal filter help receives Escape instead of the still-focused
  field behind it. Clearing a Disk Usage filter keeps its scan and treemap alive.

- **Nested Disk Usage labels no longer show through their children.** The
  treemap layout now records the exact label strip reserved by each rendered
  container, and both the live view and HTML export clip the parent name to
  that strip instead of painting it beneath translucent child tiles.

- **A built-in image editor for quick redaction and annotation.**
  Right-click an image and choose **Edit Image** to black out sensitive
  parts (rectangle or brush, always opaque) or annotate them (coloured
  outlines and brush strokes, seven colours, three brush sizes), with
  step-by-step undo. Cmd+S saves an "edited" copy beside the original
  (the original is never touched by default) and Cmd+Shift+S overwrites
  it after an explicit confirmation, keeping its Finder tags and
  permissions. Edits always render at the image's full resolution, and
  closing with unsaved edits asks first. Covers PNG, JPEG, BMP, TIFF and
  WebP; GIF is excluded so animations can't be silently flattened.
  It is also listed in the File menu, and its location button returns to the
  exact Ferail source tab and reselects the image.

- **A built-in text editor: open, fix, save, close.** Right-click a file
  and choose **Edit** (or press Cmd+E) to edit it in a small dedicated
  window, with undo, find, line numbers, and syntax highlighting by file
  type, instead of round-tripping through TextEdit or Notepad. Cmd+S
  saves; closing with unsaved changes asks Save / Don't Save / Cancel.
  Saves keep the file's identity (Finder tags, permissions, creation date)
  and its exact CRLF/BOM shape, and always write the new text durably to
  disk before touching the original. Very large or non-text files are
  politely refused with a one-click hand-off to the system editor; the
  existing "Edit in TextEdit / Notepad" entry remains.
  A location button returns to the exact source tab and reselects the file.

- **List-view horizontal scrolling is back at the bottom of the table.** The
  updated scrollbar component no longer mistakes the fixed column header for
  the scrolling viewport, and keyboard column navigation now moves headers and
  rows in the same frame.

- **The command palette now uses gpui-component's native Command control.**
  Search, grouped results, live shortcut hints, arrow-key navigation, Enter,
  Escape and virtual scrolling share the maintained upstream implementation.

- **“Open With” now fills in without rebuilding its context menu.** A cold
  association lookup updates only its retained submenu, preserving the root
  menu's focus and highlighted item while all shell queries remain off-thread.

- **An opt-in performance HUD helps diagnose slow frames and freezes.** Run
  with `--performance-hud` / `FERAIL_PERFORMANCE_HUD=1`, or choose “Toggle
  Performance HUD” in the command palette. It reports GPUI frame timing and
  process resources without forcing continuous redraws.

- **Pathological text previews now have a render-aware safety bound.** Large
  TextView replacements still parse off the UI thread, while heavily wrapped
  content is capped on whole visual lines instead of growing layout without
  limit.

## 0.7.5 - 2026-08-29

- **Large background listings can no longer outrun the interface.** Ordinary
  folders and recursive searches now use the same finite producer queue and
  time-sliced, coalesced UI apply as Flat View; duplicate and folder-size
  result streams are bounded too. Warm folder-size cache hits are sliced just
  like cold results, and a continuously refilled duplicate queue yields between
  foreground batches instead of monopolising a frame. Rapid selection
  cooperatively cancels both native and text previews, and bursty
  volume/download progress notifications have finite queues. Results are not
  capped: producers simply wait when the UI is busy, keeping memory, redraws
  and cancellation latency predictable. Background tabs still receive their
  data, but no longer refresh tables or redraw the window for invisible
  batches; selecting the tab refreshes its accumulated model and warms only
  its then-visible viewport.

- **Deceptive filenames now elide without hiding their warning.** Highlighted
  control characters, unusual whitespace, bidirectional marks and homoglyphs
  stay on one line in list and icon views. When narrowing the Name column cuts
  out the dangerous portion, the ellipsis itself carries the warning colour
  and tooltip instead of letting the name appear harmless.

- **Rename now happens directly on the file in list and icon views.** F2,
  context-menu Rename and single-item Bulk Rename replace the visible name
  with one persistent editor, select a file's stem while preserving its
  extension, validate as you type, accept with Enter or a click elsewhere,
  and cancel with Escape. A later plain click on an already-selected name also
  starts editing, while a real double-click still opens it. Holding F2 can no
  longer stack rename dialogs. The path bar uses the same quiet inline-editor
  frame, and editable Windows paths no longer expose the internal `\\?\`
  canonicalization prefix.

- **Rectangle selection is restored in list view.** Drag from the empty area
  below a listing to sweep whole rows, or hold Shift/Cmd/Ctrl to add them to
  the current selection. The gesture uses virtual-row geometry instead of
  scanning the directory, so it remains responsive with millions of files;
  icon view uses the same shared selection lifecycle.

- **Windows: Fast NTFS now checks its administrator helper before elevating
  it.** Fast NTFS runs a small separate program with administrator rights, and
  a portable install lives in a folder you can write to, so nothing stopped a
  different program from taking that helper's place. Ferail now recognises the
  exact helper it shipped with, and refuses to elevate anything else: a helper
  that is stale, damaged, half-updated or substituted makes Disk Usage say
  *"Portable fallback: the Fast NTFS helper does not match this build"* and
  scan the ordinary way instead. This is a stopgap, and worth being plain
  about: it reliably catches a broken or replaced helper, but someone who can
  already modify files in Ferail's own folder can work around it. Signing the
  Windows build with a certificate is what closes that properly, and it is
  still on the list.

- **Ferail now has a public privacy policy.** It documents local file
  processing and persistence, the absence of telemetry and automatic report
  uploads, opt-in update checks (off by default), path-redacted bug bundles
  (on by default), permissions, minidump caveats, and how to delete local app
  data. The README, About dialog, and bug-report guide link to it.

- **macOS folder icons now appear consistently in list view.** Native,
  path-specific folder artwork is warmed only for the visible rows (plus a
  small overscan), through the same shared dispatcher as the icon grid. A
  folder no longer has to be visited before its system icon appears, and very
  large directories do not trigger a whole-list icon sweep.

- **Back, Forward and Parent navigation reveal the correct row after sorting.**
  Navigation now retains the destination as a semantic file identity until
  streaming and the final sort are complete, then rebinds and centres that
  row instead of scrolling to whichever item inherited its provisional row
  number.

- **The icons-only sidebar now follows very narrow window resizes.** Its strip
  uses less width in ordinary windows, contracts a little further when the app
  is squeezed, and recentres the icons instead of preserving the previous
  oversized left gutter. Every icon-only location, favorite, recent and tree
  folder now shows its name in a tooltip.

- **Several dense controls now resize cleanly.** Search and editable paths are
  real single-line controls with lightweight completion menus, so their text
  is centred with compact input padding instead of inheriting a document
  editor's gutter. Escape clears search and cancels path or filename editing;
  Settings has a wider, user-resizable navigation sidebar and responsive field
  rows; and Disk Usage keeps its summary and controls on one wrapping header
  row without repeating the volume free-space meter already shown in the
  status bar.

- **The archive workbench now behaves like a browser, not an extractor.** Its
  actions are compact icons with translated tooltips; Enter and double-click
  expand an internal folder or open the selected file in the private preview
  path, while permanent extraction remains an explicit command. “Open as
  Archive” is offered for any single file and probes content off-thread, so
  Office documents, JAR/APK packages, extensionless archives and misleading
  names remain inspectable. Narrow result breadcrumbs are clipped at their
  splitter instead of painting over the preview pane.

- **Fresh folder navigation now selects the first sorted row.** Back, Forward,
  Parent and Refresh retain their stronger semantic-selection rules. Accepting
  a Windows inline rename no longer lets the same Enter key fall through and
  try to open the file's previous path.

- **Quarantine removal is recursive for directories on macOS and Windows.**
  The cancellable worker removes Gatekeeper provenance or `Zone.Identifier`
  throughout the selected tree, includes macOS packages, never follows
  symlinks or Windows reparse points, and scrubs cached metadata in bounded
  batches.

## 0.7.4 - 2026-08-28

- **Audio detection is content-capable without mistaking executables for
  MP3s.** Known audio extensions remain the zero-extra-I/O fast path. Renamed
  or extensionless audio reuses Ferail's bounded magic result, while the
  MPEG/AAC fallback requires multiple coherent frames instead of accepting one
  random sync word inside a large binary. Get Info, rich Description lines and
  embedded cover art now share that policy.

- **Bug reports and Fast NTFS diagnostics now have a public recipe.** The new
  guide lists the exact crash/hang text and dump files to attach, privacy-safe
  capture guidance, useful scale/resource context, and the elevated
  `ferail-ntfs-helper.exe --diagnose <path>` procedure and expected aggregate
  output.

- **The Windows installer now includes the Fast NTFS helper.** The portable
  ZIP already staged it, but the Inno file list did not; setup installs now
  preserve the same narrow elevated-helper boundary as portable builds.

- **Windows no longer shows inert Finder-tag controls.** Platform shell
  capabilities now gate Get Info tags, row dots, tag menus and background tag
  refresh as one feature instead of leaving controls that can never work.

- **The path bar and sidebar are easier to discover and personalize.** Click
  the empty breadcrumb tail or its edit icon to type a path. The sidebar cycles
  through normal, compact and icons-only widths with `Cmd/Ctrl+Shift+B`, keeps
  the user's normal width, supports persistent section disclosure and
  drag-reordering, and can restore its default order. French now uses the
  shorter “Accès” section title.

- **Windows releases now include both portable and installed update paths.**
  CI builds the portable ZIP and Inno Setup package. An installed copy prefers
  the setup asset and offers “Install and Restart”; a portable copy keeps the
  ZIP flow, even when another Ferail installation exists on the same machine.

- **Private Mode now produces useful screenshots instead of replacing Ferail.**
  The prepared browser and tool layouts remain visible while names, paths,
  user/provider labels, checksums, sizes, dates and image dimensions receive
  stable process-session aliases. Personal thumbnails, preview documents and
  Viewer frames become neutral same-layout placeholders. A process-wide input
  shield and a reduced native menu freeze the view. The existing title-bar
  shield is the sole visible indicator: it stays highlighted and toggles the
  mode back off, with `Cmd/Ctrl+Shift+K` and `Escape` as keyboard exits. Window
  close and Quit remain available. The screenshot CLI keeps privacy enabled by
  default.

- **Windows freeze reports now include an automatic all-thread minidump.**
  When the UI watchdog detects a hang it starts a pristine hidden Ferail
  broker, which writes a matching `ferail-hang-*.dmp` beside the text report
  without relying on locks in the frozen process. The dump carries thread
  contexts/stacks, module history and handle/thread metadata and can be opened
  with the exact release PDB bundle; no administrator rights are required for
  Ferail to dump its own process.

## 0.7.3 - 2026-08-28

- **Disk Usage remains responsive while Portable scans millions of files.**
  The depth-limited treemap now uses incrementally maintained subtree totals
  instead of revisiting every hidden descendant, broad directories use a
  linear iterative squarifier instead of recursively copying sibling tails,
  and the largest-files panel retains only its 50 candidates instead of a
  temporary row per file. Presentation refreshes back off on very large trees,
  scan completion derives visual state without rewriting every node, and
  privacy-safe timing breadcrumbs make any remaining slow layout or Top-N pass
  identifiable in a hang report.

- **Private Mode makes Ferail screenshots safe by default.** A new toolbar
  shield and `Cmd/Ctrl+Shift+K` command replace every Ferail-owned window with
  an opaque, non-interactive private presentation. Names use process-session
  aliases, content and identifying metadata never paint, Viewer transparency
  is forced off, and only the visible Private badge, the shortcut, Escape,
  window close, and app quit remain available. The mode is never persisted.
  The screenshot harness now enables it automatically; the deliberately
  alarming `--unsafe-real-data` flag is the only opt-out.

- **Large counts are readable everywhere, not just in the footer.** The
  status bar already grouped its figures, "1.104.619 items", but
  everywhere else a big number arrived as one unbroken run of digits:
  Disk Usage's header ("1104619 files, 743.4 GB"), the Flat View and
  duplicate-scan tab subtitles, background-task labels, copy/move/trash
  notifications, the checksum verifier's OK/mismatch tallies, the
  image viewer's position counter, the similar-images scan progress,
  and an archive's member count. Every count Ferail shows is now
  grouped the same way, in every language.

- **Folder contents use the same separator as everything else.** The
  Description column's recursive rollup grouped with commas
  ("1,204 files · 88 folders") the one place that disagreed with the
  status bar. It now reads "1.204 files · 88 folders".

- **Windows: copied paths no longer start with `\\?\`.** Copy File List,
  Copy Path and Disk Usage's Copy Paths pasted the extended-length spelling
  Windows returns when Ferail resolves a folder, `\\?\C:\opg\scene\…`
  instead of `C:\opg\scene\…`: which most shells and apps reject
  (`cd '\\?\C:\opg\scene'` is not a valid command). They now copy the
  ordinary drive-letter form, and network locations paste as `\\server\share\…`.

- **The app version now sits next to the Ferail name in the toolbar.** Ferail
  draws its own title bar, so the window caption the OS knows about never
  appears on screen: a screenshot could not say which build it came from.
  The version is now shown, muted, beside the wordmark, so a screenshot sent
  in a bug report carries it.

## 0.7.2 - 2026-08-27

This is the first all-platform release since 0.6.5. It brings the accumulated
Flat View, similar-image search, checksum/sidecar, viewer and Windows
reliability work to the platform-specific downloads together.

- **Recursive work is faster across the normal filesystem path.** macOS keeps
  names, types, sizes, dates, flags and identities on `getattrlistbulk`, reuses
  one native query buffer per worker, and now uses the same bulk records for
  opaque package totals and Description-column folder rollups. Disk Usage and
  recursive search cache their iCloud prefix once per scan, while common
  lowercase file extensions no longer allocate a temporary folded string per
  row. Local APFS remains bounded to at most eight directory readers; network,
  removable and unknown media remain deliberately serial.

- **macOS and Linux catch up with the shared features released on Windows
  since 0.6.5.** This includes uncapped compact Flat View, similar-image search
  with adjustable criteria and large-image viewing, SHA-256 generation and
  clipboard comparison, NFO/ANSI preview, SFV/checksum verification and
  generation, improved large-selection behavior, and the accumulated viewer,
  navigation and sidecar polish described in the intervening sections below.

- **Fast NTFS remains an explicit Windows preview.** Version 0.7.2 carries the
  0.7.1 session helper, live phases, bounded parallel parser and OneDrive MFT
  traversal unchanged. A code audit confirms that direct volume reads really
  require the isolated elevated helper; broader VHDX qualification and
  Authenticode signing remain open before promotion beyond preview.

- **Cross-platform Disk Usage no longer emits Windows-only warnings on macOS.**
  Fast-engine imports, states and duration formatting are now gated to the
  platforms/builds that use them, and strict Clippy is clean again.

## 0.7.1 - 2026-08-27 (Windows-only release)

- **Fast NTFS no longer appears frozen and no longer requests elevation for
  every folder in one Ferail session.** The elevated helper stays attached to
  its authenticated private pipe and serves subsequent scans until Ferail
  exits; the GUI now reports MFT reading, index construction and subtree
  traversal progress, then shows the completed scan duration measured after
  elevation (UAC and credential-entry time are excluded). The raw scan reads
  bounded 8 MiB windows, parses records in parallel without per-record copies
  or run-list allocations, and builds parent adjacency in linear time instead
  of sorting every volume link twice.

- **Fast NTFS now handles OneDrive Files On-Demand directories as real
  containers.** Reparse directories with actual MFT children are traversed
  without resolving their external target, while leaf junctions remain
  opaque and ancestor tracking still prevents corrupt cycles. Reparse tags
  carried by NTFS extension records are retained when those records are
  merged into their base file.

- **The Fast NTFS helper has a standalone diagnostic mode.** From an elevated
  terminal, `ferail-ntfs-helper.exe --diagnose <path>` prints aggregate volume,
  parser, index, subtree and phase timing data without printing names or the
  requested path. `ferail-ntfs-client-diag <path> [repeat-count]` exercises the
  real UAC/private-pipe path and verifies that one helper serves repeated
  requests.

## 0.7.0 - 2026-08-27 (Windows-only release)

- **Disk Usage gains an explicit Fast NTFS engine on Windows.** Eligible local
  fixed NTFS volumes can be scanned through a dedicated, one-shot elevated
  helper that reads the MFT sequentially and streams bounded parent-before-
  child batches back to the unelevated GUI. Ferail itself remains
  `asInvoker`: startup, browsing and Portable scans never request elevation,
  and cancelling UAC or any helper/protocol failure atomically restarts a
  clean Portable scan. The private pipe authenticates the elevated process,
  revalidates the volume and root identity, preserves raw UTF-16 names for
  actions, never writes the volume, and never persists or logs names, paths or
  raw records. Fast results account for hard links once, treat reparse
  directories as leaves, expose apparent/allocated sizes and clearly label a
  concurrently changing volume as a best-effort snapshot. The engine choice,
  eligibility and active/fallback state are visible in Disk Usage and the
  remembered preference lives in Settings. Fast NTFS is new in 0.7.0 and will
  receive more real-world testing.

- **Portable Disk Usage is faster and more accurate on Windows.** Directory
  walks now use bounded `FileIdBothDirectoryInfo` batches from one handle per
  folder instead of opening every child. Exact NTFS file identities prevent
  hard-linked names from inflating totals while Unicode paths, allocation
  sizes and the Portable fallback contract remain intact.

- **Windows packaging now treats the Fast NTFS helper as part of the release
  identity.** The portable ZIP stages the exact sibling helper; the dependency,
  signing and byte-for-byte gates cover it, and the symbols archive contains
  its matching PDB and manifest entry.

- **Ferail now understands NFO, SFV and checksum sidecars.** Content sniffing
  distinguishes scene/Kodi/MsInfo NFO and common checksum lists; the preview
  pane decodes CP437 and UTF-16, safely reconstructs ANSI layout and colours
  with a box-art-friendly terminal font, summarizes
  Kodi metadata locally, and exposes immediate folder sidecars without
  persisting their contents. SFV and GNU/BSD checksum manifests open a
  cancellable virtualized report with explicit mismatch, missing, unsafe-path
  and changed-during-read outcomes, labelled/aligned expected-checksum columns,
  compact icon controls, and an action to select an existing problematic
  target in Ferail. CRC32, MD5, SHA-1 and SHA-2 verification
  stream through bounded buffers, while SFV/SHA256SUMS generation is atomic,
  no-clobber and available for a selection or current folder. Checksums are
  labelled as integrity checks, never proof of authenticity. Refreshing a
  directory now also discards its bounded in-memory NFO/text/thumbnail and
  sidecar-discovery results, so edited files are reread without restarting
  Ferail while unrelated directories remain cached.

- **Text files can be sent straight to the platform editor from their context
  menu.** The single-file action opens TextEdit on macOS, Notepad on Windows,
  and the desktop text association on Linux, without blocking the UI. Long
  filesystem paths in the Preview/Get Info pane now use Ferail's
  `beginning…middle…end` elision and a full-path tooltip instead of overflowing
  the inspector.

## 0.6.9 - 2026-08-26 (Windows-only release)

This release publishes Windows x64 portable and symbols archives only. macOS
and Linux remain on 0.6.5; Ferail's updater selects the newest release that
actually has an asset for the current platform.

- **The Windows release dependency gate recognizes `propsys.dll` as a system
  component.** The approved `IPropertyStore` integration introduced this
  built-in Windows dependency; packaging now accepts it while continuing to
  reject non-system runtime DLLs.

- **Linux/WSL locations on Windows are now opt-in.** A new Files ›
  Locations setting controls the sidebar section and defaults to off. While
  off, Ferail does not discover or start WSL distributions; disabling it live
  also cancels discovery/activation already in progress and rejects late
  results.

- **Development builds now retire GPUI window owners cleanly at exit.** The
  ordered teardown first blurs the real component root and drains its final
  callbacks, then renders an inert root to discard the old frame's input
  handlers, element state, listeners, and overlays. This fixes both the
  `InputState` leak after preview/filter use and the later `PopupMenu` leak;
  packaged builds remain unaffected by the dev-only leak detector.

- **Opening a stopped WSL distribution no longer crashes Ferail.** The
  starting-state repaint now uses the `Shell` update already in progress
  instead of trying to acquire the same GPUI entity a second time; the final
  state is still propagated to every window after activation completes.

- **Get Info dates can now be edited without leaving Ferail.** Creation,
  modification, and last-access rows open a validated local date/time editor;
  writes run off the UI thread and refresh the affected listing afterward.
  Windows uses `FILE_WRITE_ATTRIBUTES`, so folders and read-only files work
  without changing their attributes, and each write preserves the other two
  timestamps. Ferail now reads the selected value back after Windows closes
  the handle, so a provider that ignores or materially changes a folder date
  produces an error instead of a false success. Creation-time editing is
  Windows-only for now; Unix exposes the portable modification/access pair.

- **This PC and Recycle Bin now have a safe browse-only Windows surface.**
  Their Shell-only children, including connected provider/MTP containers,
  browse in a dedicated virtualized surface while real drive/folder paths
  immediately return to Ferail's normal filesystem engine. Shell enumeration
  runs in a cancellable, time-bounded disposable process; the tab retains only
  copied opaque PIDL bytes, never COM objects or fabricated paths.
  Enumeration streams from the broker through bounded queues, so large device
  folders can paint progressively without building duplicate full-list
  snapshots; repeated refreshes reuse identical copied identities. Pathless
  rows expose the real Windows menu through **More…**, while Shift+right-click
  opens that menu directly; its PIDLs travel over a validated broker pipe and
  selected commands refresh the namespace afterward. Filesystem-only
  shortcuts, toolbar commands and pane drops are suppressed in virtual
  locations, so they can never act on the directory that happened to be open
  before This PC or Recycle Bin. Ferail-owned open, properties, restore/delete,
  clipboard and drag operations for pathless items remain future work; only
  operations offered by the official Windows menu are currently available.

- **WSL browsing no longer starts a distribution implicitly.** Ferail invokes
  `wsl.exe` only after an explicit distribution or symlink activation; a
  failed ordinary UNC listing now remains a recoverable error. Navigating
  away or closing the tab/window cancels, kills and reaps an activation
  helper. The native Windows context menu also refuses WSL paths explicitly
  and rejects impractically large symbolic selections before materializing
  them.

- **Get Info on Windows now includes approved Shell metadata and shortcut
  details.** Explicit Get Info reads a small allow-list from `IPropertyStore`
  on a COM worker, keeps results in a bounded identity/revision cache, and
  appends owned text to the normal cross-platform sections. `.lnk` targets,
  arguments and working directories use the same revision-aware shortcut
  cache. GPS coordinates, arbitrary property blobs, COM interfaces and PIDLs
  never enter the UI model, logs or persistent storage.
  Property handlers now run in a disposable eight-second broker as well, so a
  crashing or stalled third-party metadata extension cannot take down Ferail
  or occupy one of its background workers forever.

- **Windows file clipboard cut/copy semantics now interoperate with
  Explorer.** Ferail publishes and reads the Shell's `Preferred DropEffect`
  alongside `CF_HDROP`, so files cut in either application paste as a move and
  ordinary copies remain copies. Ferail's cut-row marker is cleared only after
  a fully successful move. Native outbound drags expose both filesystem paths
  and the identity-preserving Shell ID-list format; Ctrl, Shift, Ctrl+Shift and
  Alt continue to negotiate Copy, Move and Create Shortcut with the drop
  target.

- **Windows Open, Reveal and native verbs now report and refresh correctly.**
  Failed default-app launches and Explorer reveals produce an actionable
  notification instead of disappearing silently. After the isolated Windows
  context-menu broker closes, Ferail refreshes only tabs showing the selected
  items' parent folders, so rename/delete/provider verbs become visible
  without a process-wide rescan.

- **Opening a Windows shortcut now preserves Shell semantics.** `.lnk`
  resolution runs in a dedicated COM STA through the process-wide bounded
  provider lane and a revision-aware memory-only cache. A shortcut to a real
  directory navigates inside Ferail; files and applications are invoked via
  the original shortcut so arguments and working-directory behavior are not
  lost. Broken and missing targets fail visibly, and stale results cannot land
  in a changed tab or row.

- **Thumbnail work is now bounded across the whole process.** List and grid
  viewports share one priority scheduler instead of starting independent
  batches in every window. Visible and selected images outrank overscan,
  duplicate requests share one provider call, Windows Shell thumbnail work is
  capped at four concurrent calls, and image construction/result application
  is spread across frames. A late result is applied only when its original
  surface generation and row identity still match.

- **Windows Shell icon loading shares the same process budget.** Native type
  icons, custom folder icons, the sidebar and the icon grid no longer create
  independent worker waves or fetch icons while updating GPUI state. Duplicate
  type/path requests coalesce, failures converge to the normal fallback icon,
  and completed images land under the same per-frame upload/apply limits as
  thumbnails.

- **Tool results can open their location directly in a new tab.** Right-click
  one Disk Usage square, recursive Search result, or Duplicate member and
  choose Open in New Tab. A folder opens directly; a file opens its containing
  folder and is selected there, while the result remains in place for
  comparison.

- **Large folders, Flat View, recursive search, and Disk Usage enumerate much
  faster on macOS.** Ferail now reads a directory's names, types, sizes, dates,
  flags and identities in native batches instead of issuing one metadata call
  per item. Recursive scans use bounded parallel directory reads on local APFS
  volumes while removable, network and unknown disks keep the conservative
  serial path.

- **Closing Disk Usage releases its scan index.** Its file identities and
  parent links now belong to the result surface instead of the process-wide
  navigation map, so scanning millions of files no longer permanently retains
  every discovered path. Explicit actions reconstruct only the paths they use.

- **Incomplete Disk Usage scans are visible.** The summary counts skipped
  folders instead of presenting a partial total as complete. On macOS, a scan
  that reaches protected folders offers Full Disk Access, also available in
  Settings › Performance; the faster reader itself does not require that
  permission.

## 0.6.8 - 2026-08-25 (Windows-only release)

This release publishes Windows x64 portable and symbols archives only. macOS
and Linux remain on 0.6.5; Ferail's updater selects the newest release that
actually has an asset for the current platform.

- **Dragging and rubber-band selection stay responsive in large folders.**
  Ferail no longer rebuilds or deep-clones the whole selected path set for
  every visible item while a drag is active. Edge autoscroll is time-paced
  rather than mouse-poll-rate-paced, unchanged grid viewports no longer
  resubmit thumbnail work, path icon/thumbnail cache reads no longer allocate
  temporary keys, and grid marquee selection visits only the cells its
  rectangle intersects before synchronizing the list once on release. Windows
  OLE drag-over events are also capped to a display-relevant cadence instead
  of forcing full app renders at the mouse report rate.

- **A native Windows drag no longer leaves two icon stacks in Ferail.** Once a
  drag first exits the source window, the Windows Shell drag image remains the
  sole visual for the rest of that gesture, including if the pointer comes
  back over Ferail. Ferail still restores the typed payload invisibly so its
  own folders remain valid drop destinations.

## 0.6.7 - 2026-08-25 (Windows-only release)

This release publishes Windows x64 portable and symbols archives only. macOS
and Linux remain on 0.6.5; Ferail's updater selects the newest release that
actually has an asset for the current platform.

- **The real Windows file context menu is available on explicit demand.**
  Ferail's fast right-click menu remains the default and now ends with “More
  options from Windows…”. Shift+right-click and Shift+F10 open the native
  Shell menu directly, including third-party verbs and owner-drawn submenus.
  Shell extensions run only in a disposable broker process: a crash cannot
  take Ferail down, and an extension that stalls while building the menu is
  terminated after a bounded preparation period. Once visible, the menu has
  no timeout and remains open as long as the user needs it. Ordinary commands
  are invoked synchronously; the special Properties verb is routed through
  Windows' dedicated property-sheet APIs so its interface survives the broker
  dispatch. Shift+right-click also suppresses Ferail's menu completely rather
  than stacking both menus.

- **See which programs are locking a file, and close them, from the
  right-click menu (Windows).** "What's Locking This?" on any file or folder,
  and "What's Blocking Eject?" on a removable volume in the sidebar, open a
  dialog naming every process that has the item open (via the Windows Restart
  Manager), each with a Close button plus Close All: programs are asked to
  quit politely first and only force-closed if they refuse, with a warning
  that unsaved changes can be lost. Before, this diagnosis only appeared
  after a copy or move had already failed. Folders and volumes are checked
  through a capped scan, and the dialog says so when a very large tree was
  sampled rather than fully checked.

- **A failed USB eject on Windows now names the apps blocking it.** The
  "couldn't eject" message used to show only a raw error; it now lists the
  programs with files open on the volume, and clicking one brings that
  program's window forward so you can close the offending files: the same
  behavior macOS already had.

- **The Delete Immediately confirmation now opens with its Delete button
  focused.** Confirming a permanent delete (Shift+Delete) no longer needs a
  Tab press or a mouse click first: Enter or Space activates the focused
  button right away, and pressing Enter while the dialog itself held focus no
  longer dismissed it without doing anything. The button's focus ring also
  drew with its bottom edge cut off by the dialog body; it now fits.

- **Shift-clicking "Copy File List" now includes subfolder contents
  recursively.** A plain click still copies the visible rows' paths; holding
  Shift follows every folder in the list and copies its entire subtree too,
  each folder's line followed by its name-sorted contents. The walk respects
  the hidden-files toggle, never follows symlinks, and runs in the background,
  finishing with a toast that says how many paths were copied. The menu item's
  tooltip points out the Shift option.

- **Files can now be dragged from Ferail into Explorer and other Windows
  applications.** Rows, grid items, and sidebar folders become a native Shell
  file drag with normal copy/move/link modifier behavior. The OLE session is
  started only after GPUI releases its input-state borrow, preventing the
  `RefCell already borrowed` crash seen during the first implementation;
  leaving and re-entering Ferail also hands the drag image cleanly between
  GPUI and Windows instead of drawing two icon stacks. Drop, cancellation, and
  failure all clear the platform drag state.

- **Software Update on Windows downloaded the debug-symbols archive instead of
  the app.** Since releases started shipping a `…-win-x64-symbols.zip` PDB
  bundle alongside the app zip (0.6.6), the updater grabbed whichever Windows
  zip GitHub listed first: the symbols one. The updater now skips symbols
  bundles, and the bundle was renamed to `…-x64-symbols.zip` (on the published
  0.6.6 release too) so already-shipped builds also pick the right download.

## 0.6.6 - 2026-08-24 (Windows-only release)

This release publishes Windows x64 portable and symbols archives only. macOS
and Linux remain on 0.6.5; Ferail's updater selects the newest release that
actually has an asset for the current platform.

- **A corrupt PDF can no longer occupy a thumbnail worker forever.** Every
  Windows PDF open, parse, render and stream-read operation now shares one
  five-second deadline and is explicitly cancelled on expiry; unreadable
  documents fall back to their icon.

- **Rapid scrolling and selection no longer leave trains of obsolete Windows
  preview work behind.** Each file surface runs one thumbnail batch and keeps
  only the newest pending viewport, releasing cancelled reservations for a
  clean retry. Selecting a new document also terminates the previous preview
  broker immediately instead of waiting for its six-second deadline.

- **Windows crash reports now say what Ferail was doing.** The breadcrumb ring
  records path-free navigation generations, selection changes, preview
  cancellation/completion and thumbnail batch state. Native crash handling is
  re-armed after window/GPU initialization, and release PDBs now retain source
  line tables.

- **Developer screenshot runs with a second window now exit cleanly.** The
  deterministic `--screenshot --properties` repro retained dozens of
  gpui-component input callbacks and ended with an `InputState` leak
  assertion. Ferail drains those callbacks while their window is alive and
  removes harness windows before quitting; the same repro now exits zero.

- **Windows release packages must be reproducible by default.** Packaging now
  refuses a dirty source tree, with an explicit `-AllowDirty` escape hatch for
  local-only smoke packages, and records the release debug-information policy
  in the symbol manifest.

- **Quitting can no longer crash with a "leaked handles" report in
  released builds.** Beyond the reference cycles fixed above, a UI-library
  behavior still leaks an input handle whenever a second window (such as
  Get Info) was open, and the library's leak *detector*, a developer
  diagnostic, was compiled into the builds users download, turning that
  leak into an exit-101 crash with a scary report. Packaged builds no
  longer carry the detector; developer/test teardown now also drains the
  retained callbacks and closes harness windows before the assertion, while
  the upstream strong-capture design remains documented.

- **Get Info now shows photo metadata.** For images, the properties window
  gains an Image section: pixel dimensions, camera (make and model,
  deduplicated), lens, date taken, a one-line exposure summary (shutter,
  aperture, ISO, focal length), and the stored rotation when there is one.
  If the photo embeds GPS coordinates, a Location row says so: the
  coordinates themselves are deliberately not read, displayed, or stored.
  Works the same on every platform; unreadable or EXIF-less files simply
  show no section.

- **Settings › Diagnostics has a new "Bug reports" section listing the
  folders that matter when filing an issue.** The crash-reports folder
  (crash and freeze reports, native minidumps, saved report bundles) and
  the settings folder (settings file, metadata database, language packs)
  are shown with their paths and an *Open folder* button that browses
  them in a Ferail tab, creating the folder first if nothing was ever
  written to it.

- **A crash in a terminal no longer floods the console.** The crash
  output used to dump every breadcrumb and a long backtrace on stderr;
  now the console gets a short digest: the last few breadcrumbs, the
  most relevant stack frames, and the path of the full report, while
  the report file under the config folder's `reports/` gets more than
  before: every breadcrumb plus the complete raw backtrace, with no
  environment variable needed. Native faults on Windows, previously
  silent on the console, now print one line naming the exception and
  the minidump path. Also fixed: a second crash in the same run used to
  overwrite the first report file (reports now append), and on Windows the
  "relevant frames" digest always said no Ferail frames were found: the
  filter only recognized Unix-style paths.

- **`C:\Windows\Fonts` (and folders like it) no longer shows blank icons.**
  The Fonts folder is a special Explorer location: Windows refuses the
  ordinary way of looking up icons and thumbnails for files inside it, so
  every font file showed an empty placeholder. Ferail now retries those
  lookups the way Explorer itself does, and font files get their proper
  icons, and their "Abg" preview cards where Windows provides them.

- **Font previews are no longer upside down on Windows.** The Windows
  component that renders "Abg" preview cards for font files hands its
  image back stored bottom-to-top, unlike every image thumbnail, with
  nothing in the data saying so. Those cards rendered rotated 180°;
  font files are now flipped correctly.

- **Shortcuts (`.lnk`) show the shortcut arrow on their icon, like
  Explorer.** A shortcut's icon used to be indistinguishable from the
  real file or app it points to; the arrow badge Explorer draws is now
  composed onto shortcut icons in the list, grid and sidebar.

- **Windows thumbnails no longer look like screenshots of another app.**
  For files with no thumbnail of their own (Word and Excel documents, PDFs
  on machines without a PDF thumbnailer, …), the icon grid and list used to
  show a capture of the file's *preview* component: the same live viewer
  Explorer's preview pane hosts: complete with its scrollbars, toolbars
  and window chrome. Thumbnails now show what Explorer shows: the file's
  real thumbnail when one exists, otherwise its type icon. Only the preview
  pane still falls back to that capture, where a document rendering with
  chrome beats a bare icon.

- **PDFs get real thumbnails and previews on Windows, rendered by Windows
  itself.** The first page is now drawn with the PDF renderer built into
  Windows (the one the Photos and Reader apps use), in the grid, the list,
  the preview pane and the viewer, instead of depending on whichever
  third-party PDF component is installed. It runs without any window or
  helper process, so it is also immune to the PDF preview crash that
  motivated the helper-process change below. Password-protected PDFs still
  show their icon.

- **Preview components run inside a disposable Ferail helper, never the UI
  process.** Loading the component directly into that short-lived process is
  intentional: the parent can terminate the exact process that owns a hung or
  faulting DLL. Windows' `prevhost.exe` surrogate remains a compatibility
  fallback for components that expose no in-process class. Components that
  only accept a stream or a shell item (Outlook `.msg` files, for instance)
  are initialized too, where before they were skipped.

- **The preview pane no longer shows a big type icon for images Windows
  declines to thumbnail at preview size.** For some files: OneDrive
  images, notably: Windows produces the small grid thumbnail but refuses
  the larger preview-pane extraction, and Ferail then showed the file
  type's icon even though the grid was showing the picture itself. The
  type icon used to be baked into the Windows fetch as its own fallback,
  which cut off every decoder queued behind it; it is now the very last
  resort across all platforms, after Ferail's own image decoding, cover
  art, and video poster tiers have had their turn. This also un-blocks
  poster frames for videos Windows can't thumbnail (MKV and friends with
  the mpv provider configured), which the early icon used to shadow.

- **`ferail thumb` accepts relative paths and a `--preview` flag.** The
  command-line thumbnail extractor used to fail silently on Windows unless
  given an absolute path; `--preview` asks for what the preview pane would
  show rather than the grid thumbnail.

- **Quitting no longer ends in a "leaked handles" crash after using a
  context menu or the filter box.** On Windows, closing Ferail after a
  normal session could exit with an error report about leaked `PopupMenu`
  and `InputState` handles: the very "crash" files testers sent for 0.6.5.
  The right-click menu and three text inputs (the filter box, the address
  editor, the shortcuts search) each kept a hidden reference to themselves,
  so they could never be released at shutdown. Those reference cycles are
  gone, and a dismissed context menu now frees its contents immediately
  instead of lingering until the next right-click.

- **A broken PDF or Office preview can no longer crash Ferail on Windows.**
  Third-party preview components (the ones other apps install so Explorer can
  draw document previews) used to run inside Ferail itself, so a faulty one
  (like the PDF previewer access violation reported against 0.6.5) could take
  the whole app down or hang it. Each preview now renders in a short-lived
  helper process: if the component crashes or stalls, only the helper dies,
  the file shows its icon instead, and a component that fails once is
  skipped for the rest of the session.

- **Windows crashes in native code now leave a minidump next to the crash
  report.** A fault inside a driver, shell extension, or preview component
  used to vanish without a trace: the text report only covered Rust-side
  panics. Ferail (and its preview helper) now write
  `%APPDATA%\Ferail\reports\ferail-<role>-<pid>.dmp` and note the exception
  code in `ferail-crash-<pid>.txt`; with the published symbols bundle that
  is enough to name the faulting module and line.

- **The status bar's progress bar now sits beside the task it measures.**
  It used to float at the far right of the status bar, over by the app
  statistics, visually orphaned from the "Scanning…"/"Copying…" text at the
  left. The strip now follows the task label directly.

- **The Windows download now starts on a fresh PC, no Visual C++ install
  required.** The 0.6.5 build could fail before showing a window with a
  `VCRUNTIME140.dll was not found` error, because it expected Microsoft's
  C++ runtime to already be present. Ferail now carries that runtime inside
  the executable, and packaging refuses to produce a build that quietly
  depends on any DLL Windows itself does not ship. Each Windows release now
  also comes with a matching debug-symbols bundle so crash reports from
  testers can be decoded against the exact published build. The Windows
  build remains unsigned, so SmartScreen still warns on first launch.

- **Windows CPU usage now matches Task Manager, and redraw activity says what
  it measures.** The status bar previously counted CPU as a percentage of one
  core, so a busy process could show 700% while Task Manager showed a much
  smaller whole-machine share. Windows now uses Task Manager's normalization,
  and the ambiguous `rps` label is now an explicit, localized `redraws/s`.

- **Windows opens and reveals difficult paths through the Shell instead of a
  command line.** Double-click no longer routes files through `cmd /C start`,
  which could select the wrong verb, and Reveal in Explorer now uses PIDL
  identity instead of an `/select,` string that failed on valid names with
  spaces or characters such as `#`, `é`, and `!`.

- **File details now follow the viewport instead of scanning an entire
  folder.** Format, Description, and quarantine information still appears as
  rows enter view, but opening a 10,000-file or multi-million-file location no
  longer opens every file in the background. Rapid scrolling coalesces work,
  navigation cancels stale reads, and the UI applies each result directly to
  its handful of rows rather than walking the whole listing again.

- **Include Subfolders' keyboard shortcut now installs on every platform.**
  The command and action both existed, but the keymap bridge omitted their
  final connection, so startup warned about `view.toggle_flat` and skipped
  Ctrl/Cmd+Shift+L. The shortcut now reaches the existing action normally.

- **Rapid selection changes no longer accumulate preview providers.** Image
  and text previews now keep one active request and only the newest request
  waiting behind it. Holding an arrow key across a large media folder therefore
  uses constant queue space and cannot start one native preview handler per
  crossed row; active preview work is also visible in diagnostic task snapshots.

- **Leaving Include Subfolders now releases its large row buffers.** Clearing
  a multi-million-file Flat listing destroyed its rows but retained the
  backing vector's full capacity, which could leave hundreds of megabytes
  resident until Ferail quit. Flat-only row and filtered-row storage is now
  returned to the allocator while ordinary directory reloads still reuse
  their much smaller buffers.

## 0.6.5 - 2026-08-23

- **Include Subfolders: every file under this folder, in one list.** A third
  view button (⌘⇧L, or the View menu) turns the current location into a
  files-only listing of everything below it, subfolder contents included, in
  the same table you already know. Rows appear as Ferail walks the tree rather
  than after it finishes, the breadcrumb counts the files and folders it has
  been through, and you can cancel or refresh the scan at any point. A new
  sortable **Path** column shows where each file actually lives, and typing in
  the filter narrows the finished list without touching the disk again. There
  is no cap on how many files it will show, and everything it learned about
  your folder structure is dropped when you close the view. Multi-million-row
  Select All no longer expands into millions of in-memory selection records,
  Copy File List stays responsive while preparing very large lists, long names
  preserve their beginning, middle, and end, and large counts use grouped
  digits such as `4.138.016`.

- **Generate and verify a file's SHA-256 without leaving Ferail.** The File
  menu, row context menu, and command palette open a cancellable streaming
  calculation with progress and a Copy button. If the clipboard contains one
  SHA-256, Ferail trims surrounding whitespace, accepts common checksum-file
  formats, and shows an explicit match or mismatch. The expected value is
  editable, and Clear affects only the dialog, never the system clipboard.

- **The status bar now shrinks to fit instead of running off the window.** In a
  narrow window, or a language with longer words, where French turned "up 3m"
  into "en service depuis 3m": the bar's right-hand end used to be pushed past
  the edge, taking the Show Hidden switch with it. It now steps down as room
  runs out: fuller phrases first give way to short ones ("126.3 GB free on
  Macintosh HD" → "126.3 GB free" → "126.3 GB", "up 3m" → "UP 3m"), then the
  text drops a size, and only then do readouts start dropping: the app's own
  CPU/memory figures first, the folder's own numbers last. The item count and
  the Show Hidden switch are always there, whatever the width; at the narrowest
  the switch keeps its label as a tooltip.

- **Sort a folder by how often you go there.** The toolbar's sort menu used to
  offer only Name, Size, Kind and Date Modified: the same four you get by
  clicking a column header. It now has a fifth, Ant Trail, which ranks
  subfolders by their visit heat so the places you actually browse rise to the
  top; picking it again flips to coldest-first. Files and folders you've never
  opened stay below, in name order. Include Subfolders doesn't offer it: those
  rows carry no heat.

- **The filter field has a ✕ to clear it.** Typing a filter left you with no
  way back but selecting the text and deleting it; the button appears as soon
  as there's something to clear and restores the unfiltered folder.

- **Fixed: sorting by Ant Trail could do nothing until you changed folder.**
  After closing Include Subfolders, the list kept an empty Path column and
  still counted itself a subfolder listing, so an Ant Trail pick fell back to
  sorting by name: the warm folders stayed where they were. Leaving Include
  Subfolders now clears that state properly, and the Ant Trail order is decided
  from the rows themselves, so it can't be silently ignored again.

- **Favorites are marked in search and duplicate results too.** A file you had
  starred showed its star in an ordinary folder listing but not in the results
  of a search or a duplicate scan, exactly where knowing "this one I care
  about" matters most before you delete something.

- **Large folders take noticeably less memory to hold open.** Every row carries
  its name plus its size, type and description text, and each row used to own
  a private copy of all of it. Rows that say the same thing, and in a big
  folder most of them do, now share one copy, taking a row from 264 bytes to
  160. On a folder with a million entries that is roughly 100 MB Ferail no
  longer keeps. Viewport-scoped work and bounded refreshes also avoid spending
  time on rows that are not visible. These improvements apply to every listing,
  not only Include Subfolders, so ordinary large folders are faster too; at
  multi-million scale, the remaining cost now follows the actual data volume.

- **A freeze in a terminal launch now prints a short summary instead of
  thousands of lines.** The hang report used to be echoed to stderr in full
  (every loaded system library included) which pushed the one useful line, the
  path to the saved report, far out of view. The console now gets a digest:
  where the UI thread is stuck (innermost frames), the longest-running
  background task, your last action, and the report path; the complete report,
  with all thread stacks, still goes to the file you attach to an issue. Set
  `FERAIL_FULL_HANG_REPORT=1` to get everything on stderr again.

## 0.6.0 - 2026-08-23

- **The viewer is better suited to visual comparison and overlays.** Its
  filename now appears only in the native title bar, a live opacity control
  fades the whole window from 100% down to 20%, and Stay on top can accompany
  another app's native full-screen Space on macOS. The mpv backend also
  disables the four remaining built-in Lua scripts (`select`, `positioning`,
  `commands`, and `context-menu`), closing the second LuaJIT hardened-runtime
  crash path left in 0.5.2.

- **The toolbar now folds into its ⋯ menu instead of running off the edge of
  a narrow window.** As the window narrows the bar sheds clusters in order:
  the icon-size bar first, then Dock and Show Desktop, then the icon-size
  buttons, then Sort, and last of all New Folder and Refresh, and everything
  it sheds turns up in the ⋯ menu in the same order it had in the bar. The
  list/icon view switcher never folds, and the filter field gives up width
  only once there is nothing left to fold. Previously a window under about
  880 px wide pushed the ⋯ button itself off the edge, taking Get Info, Disk
  Usage, Find Duplicates and Empty Trash out of reach along with it.

- **Icon view has a size bar, and it reaches much smaller and much larger
  sizes than before.** Click or drag anywhere along it to pick any size you
  like instead of stepping between five fixed ones, and watch the grid
  re-lay-out as you drag. The
  range now runs from 32 px, small enough to skim a big folder, up to
  512 px, where a photo is genuinely previewable. The − and ＋ buttons stay,
  and now jump to the next stop past the size you are on rather than snapping
  to the nearest one, so ＋ always makes icons bigger. A new reset button
  beside them returns to the default 128 px in one click. The slider hides
  itself on windows too narrow to hold it, leaving the buttons in charge.

- **You can choose how a photo fills its icon.** Settings › Layout › **Icon
  fit** offers *Best fit* (the whole image with bars beside it: what icon
  view has always done, and still the default), *Fill frame* (crop the edges
  so the image fills the square completely), *Fit width* and *Fit height*
  (match one edge and let the other letterbox or crop), and *Stretch*. The
  filling modes pull a slightly larger preview than Best fit does, since they
  magnify the image further; at the very largest icon sizes they can still
  look a little softer than Best fit.

- **Find Similar Images extends the duplicate finder without exposing your
  photo library.** It groups resized and recompressed versions using dual
  perceptual hashes and a chain-resistant similarity pass, then reuses the
  virtualized duplicate cards with private in-memory thumbnails, dimensions,
  distance, and a best-copy keeper. Reclaim totals follow the chosen keeper;
  unsafe bulk keep-newest and clone replacement are unavailable. Paths,
  pixels, thumbnails, and perceptual hashes are neither persisted nor sent
  over the network. Use Cmd+Shift+S to start a scan; in its results, Up/Down
  moves through candidates and Space or a double-click opens the current group
  in the full-size viewer. Structure and detail controls can now tighten or
  relax the live grouping without rereading photos, and the panel explains
  whether it is enumerating folders, analyzing a known image total, or grouping
  the results.

- **The update check no longer hides an update you can actually install.**
  When a release ships for some platforms before others, Ferail now offers the
  newest release that has a download for your operating system and processor,
  and mentions the still-newer one as a separate note naming the platforms it
  exists for, instead of reporting the global latest and leaving you with no
  Download button. "What's new" stops at the version you can install, so the
  notes describe what you are about to get. Automatic notifications stay
  limited to updates this machine can install.

## 0.5.2 - 2026-08-22

Fixes a macOS crash while browsing folders containing videos when the mpv
backend is selected. The signed release now disables libmpv's unused built-in
Lua scripts before initialization, preventing Homebrew's LuaJIT from creating
an executable page that the hardened runtime rejects. Video decoding, live
filters, audio, seeking, and poster thumbnails remain enabled.

## 0.5.1 - 2026-08-22

Windows fixes for 0.5.0's archive work, which was only exercised on macOS and
Linux before release. Adding files to a ZIP, by dropping them on it, or by
saving the workbench: failed with *"the process cannot access the file because
it is being used by another process"*: the rewritten archive was still open when
Ferail tried to swap it in, which Windows refuses and Unix allows. Converting an
archive failed with *"Access is denied"* for a related reason, a flush issued
through a read-only handle. macOS and Linux were unaffected, so 0.5.0 remains
current there.

## 0.5.0 - 2026-08-22

Archives become a place you work, not just a list you look at. ZIPs have a
transactional editing workbench, members drag out to Finder and to Ferail's own
windows as native file promises, and files dragged the other way are added to a
ZIP in place: every write staged and swapped in atomically, so a cancelled or
failed operation leaves the original untouched. Ferail also speaks French and
German now, with importable language packs for anything else, and it can check
GitHub for a newer version when you ask it to (daily checks are opt-in; with
them off it makes no network requests on its own). Crash and freeze reports
survive the failures they describe.

- **Crash and freeze reports are harder to lose or deadlock.** Rust panics now
  persist an essential crash report before collecting the full backtrace, and
  the independent freeze watchdog never waits on a diagnostic mutex that the
  stalled UI thread might own. Its heartbeat state machine is covered through
  suspect, one-shot report, recovery/re-arm, and system-sleep cases; as before,
  hard process death still belongs to the operating system's crash reporter.

- **Dropping files or folders onto a ZIP adds them to it.** Drag anything onto
  an archive in the list or grid and it is added in place, no need to open the
  archive first; a dropped folder brings its contents, and a name already in
  the archive is reported instead of quietly shadowing the original. Archives
  that cannot be modified in place (7-Zip, TAR and its compressed variants,
  single-file GZ/BZ2/XZ, LHA) show the forbidden cursor and refuse the drop
  rather than letting the files land in the surrounding folder by accident.
  Members dragged out of another archive can be dropped on a ZIP too. Adding is
  transactional: the archive is staged and swapped in atomically, so a
  cancelled or failed add leaves the original untouched instead of possibly
  leaving a half-written entry inside it. If some dropped items were already in
  the archive, Ferail now names them instead of reporting a clean success.

- **ZIP archives now have a transactional editing workbench.** Drag files or
  folders into the archive (including onto an inner folder), rename entries,
  and stage removals, then review the projected result and choose **Save
  Changes** or **Revert**. Saving writes and validates a sibling archive before
  atomically replacing the original; cancellation, write failure, or an
  archive changed by another app leaves the original untouched. Other archive
  formats and ZIP-based packages such as DOCX/JAR/APK stay browse-only and show
  a lock badge, forbidden cursor, and explanation when a drop cannot be
  accepted.
  Archive rows now also expose packed size, compression method, checksum,
  permissions, encryption, and comments when their format records them. They
  use the same deceptive-filename highlighting as normal files. **Convert
  Archive…** creates a separately named ZIP, 7-Zip, TAR, TAR.GZ, TAR.BZ2, or
  TAR.XZ, validates it, and preserves the source. Extraction now also refuses
  pre-existing destination symlinks/reparse points and codec-specific link or
  special-file entries, in addition to the existing absolute/`..` zip-slip
  checks.
  Archive members can now be dragged from a popped-out workbench to Finder on
  macOS using native file promises: decoding starts only after the external
  drop and runs off the GUI thread. The same gesture now crosses into another
  Ferail window, where dropping on the list extracts into its current folder
  and dropping on a folder row extracts there: previously the drop silently
  did nothing in Ferail (while working in Finder) because the window never
  registered itself as a destination for promised files. No placeholder file
  is created, and a workbench overlapping its parent can drop straight into
  the underlying main window. Dropped members land in the destination under
  their own name, the same way the identical drag onto Finder behaves: a
  file arrives as `alpha.txt`, a dragged folder brings its subtree, and
  neither is wrapped in an extra folder named after the archive; a name
  already in use is kept as `alpha (2).txt` rather than overwritten.
  List and grid folder targets plus Browse-tree, volume, available favorite
  rows, sidebar Locations, and breadcrumb segments highlight and extract into
  the selected folder: sidebar Locations such as Downloads and Desktop, and
  breadcrumb segments, previously ignored dropped archive members entirely,
  and every target keeps its highlight during the cross-window part of the
  drag, where nothing used to light up. Dragging members out now also works
  from a docked archive view, not only a popped-out one. Crossing a Ferail
  window edge no longer
  makes GPUI discard the archive payload, so leaving and re-entering restores
  the red forbidden feedback and the eventual drop still commits. Dropping a
  member back on its own archive is a cancelled drag and no longer bubbles into
  the current folder. Renamed compressed TAR downloads such as
  `backup.tar (1).gz` are detected from the decoded TAR header and show their
  real inner tree.

- **Check for Updates now always gives a visible, recoverable result.** The
  native menu command raises an already-open Software Update dialog instead
  of appearing to do nothing, and recovers if its original window vanished.
  Offline, timeout, missing-page, rate-limit, server, and malformed-response
  failures now explain what happened and offer Retry; checks stop after 20
  seconds, and interrupted downloads no longer leave `.part` files behind.

- **Ferail can now speak your language.** Settings › Appearance has a new
  Language group: follow the system language, or pick a language pack:
  French and German ship built in, and anyone can add another without a
  code change. *New language…* writes a template file with every string
  and a set of translation instructions; give that file to a translator or
  to an AI assistant (Claude, ChatGPT, a local model: Ferail itself never
  calls one and needs no API key), then *Import…* the result. *Export…*
  saves a language to finish or share it, and the dropdown shows how much
  of the UI each pack covers. Switching takes effect immediately, menus
  included; anything a pack doesn't cover stays in English.

- **Every failure toast with real error text can now be expanded and
  copied.** The "Show details" / "Copy" controls that the structured
  failure reports already had now also cover the trash, de-duplicate,
  save-report, move-to-trash and disk-full failures, which used to show a
  clipped one-liner you couldn't read in full or paste into a bug report.

- **Opening a window no longer freezes when your last folder was on a
  sleeping drive.** Ferail reopens at the folder you left off in; if that
  folder lives on an external disk that has spun down (or a network share
  that's slow to answer), every new window, including Cmd+N, used to sit
  frozen for seconds while the disk woke up. The window now opens at once,
  showing the folder's breadcrumbs with an "Opening …" note in the status
  bar, and fills in when the disk answers. A folder that no longer exists
  falls back to Home, as before. Folder icons in the icon grid and the
  sidebar are fetched the same way now, one custom-icon folder on a slow
  volume can't stall scrolling anymore.

- **Ferail can now tell you when a newer version exists: if you ask it
  to.** A new **Ferail → Check for Updates…** menu item asks GitHub
  Releases for the latest version and shows the result in a Software
  Update dialog: when something newer exists the dialog shows **what's
  new**: the release notes from GitHub, rendered right there, and if you
  skipped a few versions, the notes of every release you missed, so you
  decide with the changes in front of you; then download the right file
  for your machine (macOS disk image,
  Windows zip, Debian/Ubuntu package for your CPU) straight into
  Downloads with live progress, then open it or show it in the folder,
  installing stays your step, Ferail never replaces itself. A separate
  **Settings → About → Updates** switch turns on a *daily automatic*
  check that posts a notification when a new version appears; it is
  **off by default**, and with it off Ferail makes no network requests
  on its own. Nothing is ever downloaded without you choosing to.

## 0.4.0 - 2026-08-19

The Linux build becomes actually usable (a font-lookup/backtrace stack-up
had it at seconds per frame), and a batch of features lands: a filter box
that understands metadata expressions and suggests its syntax, freeze
diagnostics (automatic hang reports, `--safe-mode`), a viewer that fits
media to the window, a folder context menu on empty space, an honest
status bar under a filter, and eject that first releases Ferail's own
holds. Linux `.deb`s and the Windows zip are built by CI; Windows remains
unsigned (SHA-256 on the release page).

- **Linux: the app no longer crawls at seconds per frame.** On a stock
  Linux install the interface was barely usable: every repaint took
  seconds and input lagged far behind. Two causes stacked: the UI font the
  toolkit asks for by default ("IBM Plex Sans", which Zed bundles and
  Ferail does not) isn't installed, so every piece of text on screen walked
  a nine-deep fallback list on every frame; and Ferail's own
  crash-diagnostics default (`RUST_BACKTRACE=1`) made each of those misses
  capture a full stack trace. Ferail now picks an installed system font once
  at startup (Adwaita Sans / Ubuntu / Cantarell / Noto Sans / DejaVu Sans,
  in that order; Noto Sans first under KDE) and a real monospace face for
  code previews (which used to fall through to a proportional font), and it
  no longer lets the crash-diagnostics backtrace setting leak into ordinary
  error handling on any platform: panics still get full backtraces. Set
  `RUST_LIB_BACKTRACE=1` yourself if you want library errors to carry one.

- **The viewer now fits media to the window by default, and you can pick a
  different default.** Small images and videos used to open as a tiny
  postage stamp in the middle of the window (the viewer fitted large media
  down but never enlarged anything); now everything opens filling the
  window. If you preferred the old pixel-true behaviour, a new **Settings →
  Layout → Viewer → Default zoom** dropdown offers "Fit, never enlarge" and
  "Actual size (100%)" alongside the new "Fit to window" default; the
  choice also sets what zoom reset (⌘0) and the double-click fit toggle
  return to, and applies from the next viewer window you open.

- **A frozen app now explains itself.** If the interface stops responding
  for about ten seconds, Ferail automatically writes a hang report: what
  background work was running, the recent action history (path-redacted,
  honoring the privacy toggle), and on macOS full stack traces of every
  thread, to the same `reports/` folder as the issue bundle, and keeps
  running in case it recovers. Launched from a terminal, pressing
  `Ctrl+\` (macOS/Linux) or `Ctrl+Break` (Windows) writes the same report
  on demand and exits; `Ctrl+C` on a frozen app writes one on the way
  out too. On Linux, stack traces are included when `elfutils` or `gdb`
  is installed. Attach the report when filing a freeze issue.

- **New `--safe-mode` launch flag (or `FERAIL_SAFE_MODE=1`) for freeze
  hunting.** It starts the app with every optional background subsystem
  off: file watching, folder sizes, thumbnails, format detection, the
  metadata database, free-space lookups, and the volume/power/stats
  watchers, so one relaunch answers whether a freeze comes from the
  background work. Settings are untouched; a normal restart brings
  everything back. Favorites, Recents, and Ant Trail stay empty for the
  safe-mode session by design.

- **The filter box now filters by metadata values, not just names.** Typing
  `size:>10mb`, `mod:week`, `created:2026-01-01..2026-06-30`, `kind:folder`,
  `ext:pdf`, or `locked:yes` narrows the listing by the file's actual size,
  dates, kind, extension, or locked flag: alone or combined with ordinary
  text (`report ext:pdf mod:month`). Plain words keep working exactly as
  before, and anything unrecognized is treated as literal text rather than
  guessed at. The same tokens work when Enter escalates the filter into a
  recursive search, whichever engine runs it (Spotlight or the built-in
  walker). Dates accept `today`, `yesterday`, `week`, `month`, `year`,
  comparisons (`mod:>2026-01-01`), and ranges; sizes accept the units the
  Size column shows. Wrap a phrase in quotes to match it with its spaces.

- **The filter box suggests its own syntax as you type.** Start typing a
  token key and a completion menu offers the supported filters: type `lo`
  and it offers `locked:` with a one-line description; accept it and the
  menu chains into the valid values (`yes` / `no`, or ready-made examples
  like `size:>1mb` and `mod:today`). Clearing the field shows the full
  token list once, so the syntax is discoverable without reading docs.
  Arrow keys and Enter pick from the menu while it's open; Esc dismisses
  it, and plain typing is never interrupted: the menu only appears when
  what you're typing looks like a token.

- **A (?) button beside the filter field opens a filter-syntax cheat sheet.**
  One click lists every supported token with its accepted values and
  examples, plus the date and size rules, so you never have to remember the
  syntax or hunt for it in the docs. Esc or a click outside closes it.

- **The mpv video player now works in release downloads, no rebuild needed.**
  The optional mpv backend (plays virtually any video format, and powers live
  grading and the transparent chroma-key windows) used to require compiling
  Ferail from source with a special flag. Release builds now ship with the
  provider compiled in; it stays dormant until you install mpv's library
  (macOS: `brew install mpv`; Windows: a libmpv build; Linux: `libmpv2`,
  which the .deb now pulls in automatically on current distributions) and
  pick **mpv** under **Settings → Plugins**. Ferail still does not bundle
  libmpv itself, without it the built-in player works as before.

- **Right-clicking the file list's empty space now opens a folder menu.**
  Right-clicking below the last row, or anywhere in an empty folder:
  used to do nothing. It now opens a context menu of commands that act on
  the folder you're browsing: New Folder, Paste, Select All, Get Info,
  Reveal in Finder, Copy Path, Open Terminal Here, Add Folder to
  Favorites, and Refresh. The menu targets the folder itself, never the
  current selection, matching what Finder does on its window background.

- **The status bar now says how much the filter field is hiding.** Typing in
  the filter narrowed the listing, but the item count and total size beside
  it silently narrowed too, "11 items · 214 KB" looked like the whole
  folder when it was only the matches. The count now carries a companion
  figure for what the filter is holding back, "11 items · 214.3 KB" now
  sits beside "16 filtered out · 281.1 KB", so the rest of the folder,
  and its size, is never a mystery. When the filter matches nothing at
  all, the status bar says "All 60 items filtered out · 15.0 MB" and the
  empty listing says
  "All 60 items filtered out." instead of claiming the folder is empty,
  which used to send you looking for files that were only being filtered.

- **Ejecting a volume you're still browsing now works.** Eject used to fail
  with "Ferail has files open on it" when the app itself was the culprit:
  a tab open on the volume, an archive being browsed, or a viewer window
  playing a file from it was enough. Eject now first releases Ferail's own
  holds: tabs on the volume (in every window) go back to your home folder,
  archive and disk-usage views rooted on it close, and viewer windows
  playing from it close; then the eject proceeds, waiting a moment and
  retrying once so the freed files are really closed. And when another app
  still blocks the eject, the failure message shows each blocking app as a
  **clickable button**: clicking one brings that app to the front so you
  can close whatever it has open on the volume and try again. (Buttons for
  processes without windows, like background daemons, do nothing; on Linux
  and Windows the names are shown but aren't clickable yet.)

## 0.3.0 - 2026-08-08

First release with **Linux downloads** (Ubuntu/Debian `.deb`, Intel and ARM),
and the first where dragging files out of Ferail to other apps works on macOS.
macOS remains signed + notarized; Windows remains unsigned (SHA-256 on the
release page).

- **Dragging files out of Ferail into other apps now works, with full
  Finder semantics.** Dragging rows, grid cells, or sidebar folders to
  Finder, the Desktop, an editor, or any other app never actually worked:
  the drag ghost just stopped at the edge of the window and nothing was
  handed to the system, because the UI framework's drag was in-window only
  and the code wrongly believed otherwise. Ferail now promotes a drag to a
  native macOS drag session the moment the pointer leaves the window, and
  the drop behaves exactly like a drag started in Finder: a drop on the
  same volume moves the files, a drop on another volume copies them (with
  the system's green “+” badge on the cursor), holding ⌥ forces a copy,
  ⌘ forces a move, ⌃ drops an alias, and pressing Esc cancels the drag,
  inside or outside the window, with the items animating back to where
  they came from. Entries inside an archive are the one exception: they
  have no on-disk files until extracted, so they still only drag within
  the window.
- **The status bar shows what Ferail itself costs.** A quiet readout on the
  right, "up 3d 4h · CPU 0.2% · MEM 184.0 MB · 0 rps", reports how long the
  app has been running, its CPU share, its memory footprint, and how many
  times per second the window redrew. All figures are about Ferail, not the
  machine: the last figure is deliberately labelled *rps* (redraws per
  second), not "fps": the app only draws when something changes, so an idle
  window honestly reads 0, and any number it shows is a plain count of real
  redraws rather than a claim about animation smoothness. A nonzero value
  while you aren't doing anything means something is wastefully repainting.
  CPU is counted the way Activity Monitor does, so it can exceed 100% on
  multi-core work. Each number sits in a fixed-width slot, so the bar
  doesn't shift around as values update. The readout appears a few seconds
  after launch, once the first reliable sample exists.
- **Ferail can be packaged as an Ubuntu/Debian `.deb`.** The repo now carries
  a freedesktop desktop entry, the app icon in the standard hicolor location,
  and `cargo deb` packaging metadata; every window identifies itself to the
  desktop environment as `ferail`, so docks and taskbars show the right icon
  and group Ferail windows together. Verified end to end on Ubuntu 24.04
  (arm64): the package builds, installs, and the installed app launches and
  browses folders. CI builds both the Intel (amd64) and ARM (arm64)
  packages against Ubuntu 22.04, so they run on 22.04 and later, and they
  are attached to the GitHub release as public downloads, starting with this
  release. Opening a *specific folder* from the
  desktop ("Open with Ferail") isn't wired yet: the binary doesn't take a
  directory argument.
- **Fixed a crash-on-build for ARM Linux.** Owner/group name lookup used a
  buffer type that only compiles where C's `char` is signed: fine on
  Intel/macOS, a build failure on ARM Linux (Raspberry Pi class machines,
  ARM servers, Apple-Silicon VMs).
- **Choose your terminal.** Settings → Files → Terminal picks which terminal
  "Open Terminal Here" launches: an app name or `.app` bundle on macOS, a
  program path, or a command on `PATH`: with your own launch arguments
  (`{dir}` expands to the folder) and a Standard/Administrator mode. Blank
  keeps the platform default: Terminal.app, Windows Terminal, or the usual
  Linux emulator hunt. Administrator means a UAC prompt on Windows and a root
  shell in the terminal on macOS and Linux.
- **Paste, move, rename, and the result is selected for you.** When an
  operation finishes in the folder you're still looking at, what it produced is
  selected and scrolled into view: pasted and moved files, a renamed item, a
  new folder, a duplicate, an alias. Previously a paste into a long listing
  gave no sign of where the file landed.
- **Hidden files are easier to reason about.** With *Show hidden* on, hidden
  entries render dimmed so they read as distinct from your real files. With it
  off, the status bar quietly reports what's out of sight, "3 hidden ·
  12.1 KB", so you know hidden content exists, and how much space it takes,
  without unhiding it.
- **Diagnostics can take you to the files it talks about.** Settings →
  Diagnostics now shows the full path of the running app, and every row about a
  location on disk: the app itself, the config folder, the settings file, the
  metadata database, mpv: has a Reveal button that opens that spot in Ferail
  with the item selected.
- **Read-only volumes are detected on Windows and Linux.** The status bar says
  "{volume} is read-only" instead of reporting "0 B free", which is true on a
  CD but buries the actual story.
- **Text previews handle legacy single-byte files.** Old README and `.nfo`
  files written in ISO-8859-1 (Amiga/DOS-era exports) now preview as text
  instead of being rejected as binary.
- **Folder sizes stop re-measuring themselves.** Returning to the app used to
  re-walk every visible folder tree from scratch, so on a big folder the Size
  column could never settle. It now answers from cache and only recomputes what
  actually went stale; Refresh is still the gesture that forces a fresh measure.
- **Fixed: folders labelled as archives.** Some folders showed a file format
  such as "ZIP archive · 3 files" in the Format and Description columns,
  inherited from a stale cache entry for a path that used to be a file. Folders
  are now guarded from format detection at every layer, and existing bad
  entries clear themselves on next launch.
- **Delete Immediately is findable.** The permanent-delete command (Shift+Delete,
  or Option+Cmd+Delete on macOS) was missing from the Cmd+K command palette and
  the keyboard-shortcut sheet, so it could only be reached from a menu.

## 0.2.2 - 2026-07-31

First release with a **Windows download**. Also the release that unbroke the
Windows build, which had been failing to compile on `main` for two weeks
without anyone noticing, because nothing in CI ever built it.

- **Windows builds are back, and now ship.** A single `IMFMediaEngine::SetMuted`
  call written from a Mac on 2026-07-14 could not compile under the `windows`
  crate's type inference, and took the whole Windows app down with it. Fixed,
  and with it the app builds, runs, passes its tests, and screenshots on
  Windows again.
- **Windows packaging**: `scripts/package-win.ps1` produces a portable ZIP
  (`Ferail.exe`, the `ferail` CLI, and the licence notices), plus an installer
  with a Start Menu entry and an uninstaller when Inno Setup is present.
  Authenticode signing is wired but this release is **unsigned**, so Windows
  shows a SmartScreen warning: verify the download instead:
  `Ferail-0.2.2-win-x64.zip` is SHA-256
  `9993AF0EF53DE617C255BF9EBBA7FF53DFB3EDDC80866FF5D969715A78F30E6B`.
- **Fixed: the viewer's "Stay on Top" did nothing on Windows.** The toggle never
  reached the OS. It now really does keep the window above other apps.
- Note that nothing automatically builds Windows yet, so the class of breakage
  above can still recur: it is caught only by someone building on a Windows
  machine.
- **Fixed: a fresh `git clone` could not build on any platform.** The workspace
  manifest referenced checkouts that only exist on one developer's machine, so
  `cargo` failed before it compiled anything.
- **The shipped binary carries no GPL-3.0 code.** A single non-optional
  dependency edge (`gpui → sum_tree → ztracing`) pulled GPL-3.0-or-later crates
  into every build, which is incompatible with distributing a binary under
  MIT/Apache-2.0. Ferail now vendors that one Apache-2.0 crate with the edge
  removed. Related correction: THIRD-PARTY-NOTICES.md previously said the edge
  was already gone upstream: it was not; the lockfile that suggested so had
  been generated with an unrelated local override active.
- No user-visible macOS changes.

## 0.2.1 - 2026-07-31

- **Fixed: the 0.2.0 app would not launch on a Mac without Homebrew.** It quit
  immediately with a dyld *"Library not loaded:
  /opt/homebrew/opt/xz/lib/liblzma.5.dylib"* error. The xz/LZMA support added
  in 0.2.0 linked against whatever liblzma the build machine happened to have
  instead of bundling its own, so the app only ran on machines that already had
  Homebrew's `xz` installed. liblzma is now compiled into the binary, and the
  macOS packaging script refuses to build a release that references any library
  outside `/usr/lib` and `/System`.
- The macOS app bundle now carries `LICENSE-MIT`, `LICENSE-APACHE` and
  `THIRD-PARTY-NOTICES.md` in `Contents/Resources/licenses`.

## 0.2.0 - 2026-07-30

- **Archive support (compress & extract)**: right-click **Extract** on any
  `.zip`, `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`, `.tar.xz`, `.gz`, `.bz2`, `.xz`,
  or `.7z`, then **Extract Here** (into the current folder) or **Extract To…**
  (pick a destination). Extraction lands in place when the archive holds a
  single top-level folder, or in a new folder named after the archive
  otherwise, and it's safe against malicious archive paths (no writing outside
  the destination). **Compress** is now a submenu offering **ZIP**, **7-Zip**,
  and **TAR** (Gzip / Bzip2 / XZ / uncompressed), powered by a new built-in
  archive engine (no more shelling out to `ditto`), so it works the same on
  every platform.
- **New Archive dialog**: "New Archive…" in the Compress menu opens a dialog to
  pick the format (ZIP / 7-Zip / TAR.GZ / TAR.BZ2 / TAR.XZ / TAR), the
  compression level (Store / Fast / Normal / Maximum), and an optional password,
  instead of taking the one-click defaults.
- **Add files to a zip by dropping them in**: drag files from Finder or the file
  list onto an open archive to add them in place (ZIP only; formats that can't be
  edited show no drop target). Names already in the archive are reported rather
  than silently duplicated.
- **Browse inside archives**: right-click a file → **Open as Archive** to open
  its contents (like Disk Usage): a real, sortable file list with the usual
  columns, expandable folders, and a filter box, so a 5000-file archive opens
  as one folder to drill into, not 5000 rows. Then **Extract Selected**
  (a selected folder brings its whole subtree) or **Extract All**. It works on
  anything that *is* an archive underneath, even without the extension
  (`.docx`, `.xlsx`, `.pptx`, `.jar`, `.apk`) and says so plainly when a file
  isn't one. Formats that can't be edited in place (tar, 7z) are marked
  read-only. The workbench can also be **popped out into its own window** (and
  docked back), like Disk Usage: handy for dragging files into an archive with
  Finder open beside it. You can also **drag entries out of an archive** onto a
  folder row or another Ferail window to extract them there (dragging to
  Finder itself is still to come).
- **Richer 7-Zip descriptions**: `.7z` files now show their file count, root
  folder, and whether they're encrypted in the Description column, the same way
  ZIPs already did.

## 0.1.0 - 2026-07-24

First signed & notarized macOS build.

- **Folder contents at a glance**: folders now show their recursive item counts
  ("1,204 files · 88 folders") in the Description column.
- **Easier favorites**: "Add to Favorites" is now in the File menu, and you can
  drag a folder straight onto an empty Favorites list (it's a proper drop zone
  now).
- **Filenames truncate in the middle**: long names in the list keep their start
  and their extension visible ("Annual Board Meeting…approved).pdf"), Finder-style,
  instead of losing the end.
- **Tidier viewer**: the viewer toolbar folds into a "…" menu when the window is
  too narrow to fit every button.
- **Fixed:** typing a name into the New Folder / Rename dialogs now works: those
  fields were silently ignoring keystrokes.
- **Fixed:** resizing a column while a folder is still loading now sticks: the
  width no longer snapped back while background work was running.

## 2026-07 - More platforms, steadier on slow drives

- **Runs on AROS**: Ferail now boots on AROS (aarch64) with menus, previews,
  and disk usage.
- **Windows & Linux caught up**: resilient file operations with clear errors
  when a file is busy, OneDrive/Trash/Open-With integration, native video, and
  Finder-style "Eject All" everywhere.
- **Image previews without macOS**: a built-in thumbnail renderer means previews
  work off macOS too.
- **Calmer on slow media**: spun-down drives and network mounts no longer freeze
  the window.

## 2026-06 - Viewer, video, search & disk usage

- **Media viewer**: images and video with zoom/pan, rotation, slideshow,
  In/Out cues, one-click enhance, and transparent stacking windows.
- **Icon (grid) view** and a Finder-style drag with real thumbnails and
  spring-loaded folders.
- **Find things**: recursive + Spotlight search and a duplicate finder, each in
  its own tab.
- **Disk Usage**: treemap with a Top-N panel and HTML export.
- **Richer previews**: inline text/code with syntax highlighting and formatted
  markdown.
- **Command palette** (Cmd+K) and a keyboard-shortcuts overlay.

## 2026-05 - The core explorer

- Rebuilt on a new rendering foundation, then filled in the essentials:
  **multi-window tabs, a curated Favorites sidebar, sortable columns, copy /
  move / trash with progress and undo, Get Info, and background folder sizes.**
- Started as a native macOS file explorer with real icons, magic-byte file
  detection, quarantine badges, and drag-out to Finder.
