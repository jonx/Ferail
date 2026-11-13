# Ferail 0.8.0 - Compare folders, see what grew, Git at a glance

Six new tools, a much faster Disk Usage, and a sweep that removed the places
where the window could still stall on a slow disk.

## Compare two folders

- In Split View, **Compare Folders** (Cmd+Option+K, or the toolbar button)
  tints every row in both panes: blue exists on one side only, amber differs,
  green is identical. Files are compared by content, and folders of the same
  name are compared all the way down.
- Two arrow buttons in the toolbar **copy what is missing or newer** to one
  side or the other. Nothing is ever deleted, and Ferail asks first before
  replacing older files, because that cannot be undone.
- Hover the Compare button for the count of each kind.

## Disk Usage

- **Large folders scan far faster and the window stays fluid.** A folder of
  1,8 million items took 130 seconds in the window and now takes 11.
- **See what grew.** The calendar button recolours the map against the
  previous scan of the same folder: orange grew, blue shrank, purple is new.
- **See duplicates in place.** A new button hatches every file that has an
  identical copy in the scanned folder and totals the space you could reclaim.
  Click a hatched file to light up its copies.

## Git status

- In a folder that belongs to a Git repository, a **Git column** marks each
  file (M modified, S staged, R renamed, D deleted, C in conflict, ? untracked,
  I ignored), a folder carries a dot for the most serious state inside it, and
  the status bar shows the branch with its ahead and behind counts.
- It is read-only and uses the `git` already on your machine. With a saved
  column layout the column appears at the far right; Reset Columns puts it
  next to the name.

## Safer copies

- **Verify copies** (Settings > Files, off by default) reads every copied
  file back and compares it with the original. A copy that does not match is
  removed and reported, and a move keeps its original. Copying is slower with
  it on.
- **Verify Integrity** (File menu, context menu, palette) finds files whose
  content changed while their size and date did not. It only knows files
  Ferail has hashed before, and records the others for next time.

## Viewer

- **N** opens the next file and **P** or Shift+N the previous one; **F**
  toggles full screen.
- On macOS the pointer hides after resting five seconds over the viewer.

## Also in this release

- **Every notification has a Copy button** for its text.
- **The window no longer stalls on a sleeping or network disk** in six places,
  among them the Recents section of the sidebar, New Archive, and the start
  and end of every copy on Linux.
- **Counts are grouped everywhere**, and several English fragments inside
  translated messages are now translated. Polish gains six missing entries.
- **Windows: search can use the NTFS index**, and Disk Usage reuses it instead
  of rereading the drive's file table on every scan.

## Worth knowing

**The new tools were checked with automated tests and screenshots, not by
hand.** The sync buttons of Compare Folders, clicks in the Disk Usage map and
the live refresh of the Git column have not been exercised in a running app.

**The Windows NTFS index has been tested on recorded data only, never on a
Windows machine.** It is used only when you pick **NTFS index (administrator)**
in Settings > Search, or once Disk Usage has started the administrator helper.
Prefer the built-in walker if a search looks wrong, and please report it.

**Verify Integrity can mislabel two cases.** A file rewritten at the same size
within the same second may be reported as corrupted, and a file you edited
normally may show as newly recorded instead of modified.

On Windows the duplicate finder does not recognise hard links, so reclaimable
space is overstated when they are involved. The Git column and the comparison
tints do not appear in the icon grid.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
