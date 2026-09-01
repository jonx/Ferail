# Ferail 0.7.7

The right-click menu is yours now, the Trash finally does something, and the
app speaks Polish.

## Make the menu yours

**Settings ▸ Menus lists every right-click entry, with a switch on each one.**
Turn off what you never use, drag the rest into the order you want, drop
separators where you want gaps, and reset a menu if you go too far. Hiding an
entry never removes the command: it keeps its shortcut and stays in the
palette.

## A Trash that behaves like one

**Right-clicking in the Trash used to offer to rename and compress things you
had thrown away.** It now offers what deleted items deserve, including **Put
Back**, which returns an item to where it came from and refuses to overwrite
anything standing there. Ferail can only put back what it trashed itself;
macOS keeps Finder's own record private.

**On Windows the Recycle Bin works again.** Every row has its Shell menu, a
**Restore** entry backed by Windows' own restore command, an icon, and a
column showing where the item was deleted from.

## Ferail speaks Polish

A complete Polish translation ships in the app, contributed by **Bohun**. Pick
it in Settings, or let Ferail follow your system language. Counts of two to
four items read slightly off until the pack gains its `few` forms.

## Also in this release

- **The text editor stops hiding its own features.** Find and replace, reload
  from disk, a line/column/encoding strip, and wrap and line-number toggles
  all have buttons now.
- **Closing Ferail actually ends it.** No more headless process to kill before
  installing an update.
- **Double-clicking a checksum file runs the check** instead of opening a
  column of hashes.
- **Entering a folder shows a real preview** of the row it selected for you,
  and double-clicking a selected folder opens it instead of renaming it.
- **Long text previews get a scrollbar.**
- **Private Mode paints blurred stand-ins** instead of grey boxes, so a
  screenshot of a real session still looks like Ferail. Every pixel is
  invented; none of it comes from your files.
- **Less memory over a long session**, about 90 MB per million files seen.

## Worth knowing

The command palette (Cmd+/) no longer lets the list underneath steal your
scroll wheel, but its own list still stops short of the end. **Use the arrow
keys** to reach the last commands.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. The macOS DMG is signed and notarized.
