# Ferail 0.7.7 - Menus you can shape, a Trash that works, and Polish

This release makes the right-click menus yours to arrange, gives the Trash the
commands it always needed, surfaces what the text editor could already do, and
ships a third bundled language.

## Make the menus yours

- **Turn off the entries you never use, and drag the rest into any order.**
  Settings ▸ Menus lists every entry the right-click menu shows on a file or
  folder, and the one on empty space, in the order they appear. Each has a
  switch. Rows drag into whatever order you like, and separators are rows too:
  drag one where you want a gap, remove the ones you do not, and **Add
  Separator** makes a new one. **Reset This Menu** puts a menu back the way it
  shipped.
- Hiding an entry never changes what Ferail can do: the command keeps its
  keyboard shortcut and stays in the command palette. Entries still appear only
  where they apply, so turning one back on does not make it show up on files it
  cannot act on. Open and Get Info cannot be hidden.
- A command added in a future version lands next to the entry it was designed
  to follow, rather than at the bottom of your arrangement.

## A Trash that behaves like one

- **The Trash has its own right-click menu.** Browsing it used to offer the
  ordinary file menu, which made no sense there: it proposed renaming,
  duplicating, compressing and tagging things you had thrown away, and *Move to
  Trash* on items already in it. The menu is now short and about deleted items:
  Open, **Put Back**, Get Info, Quick Look, Reveal in Finder, Copy Path, Delete
  Immediately and Empty Trash.
- **Put Back returns an item to where it came from**, recreating the folder if
  it has gone since, and never overwriting: if something else took the name, it
  says so instead of replacing it. One limit, stated plainly: Ferail can only
  put back what *it* trashed. macOS keeps the Finder's own put-back information
  in a private store that Ferail does not read, so an item trashed by Finder
  reports that its original location is unknown rather than guessing.
- **On Windows the Recycle Bin works again.** Right-click produced an empty
  popup, so the only thing you could do there was empty it. Every row now has
  the Shell menu plus a **Restore** entry that puts the selection back with its
  original name, dates and permissions, using Windows' own restore command, so
  it works on anything in the bin whatever put it there. Rows also show **where
  each item was deleted from**, the way Explorer does, and they have icons
  again.

## The text editor shows what it could always do

- **Find and replace are visible.** Cmd+F searches and Cmd+Shift+F replaces
  (Ctrl+F and Ctrl+H elsewhere), with previous/next, a match counter, a case
  toggle and Replace All. They have worked since the editor shipped; nothing in
  the window pointed at them. The toolbar now has a button for each, and Escape
  closes the find panel before it closes the window.
- **Reload from Disk** re-reads the file, keeping your place, and asks first if
  you have unsaved edits. A strip along the bottom shows the cursor's line and
  column, the line count, and the file's encoding and line endings, so a file
  that came in as CRLF or with a BOM no longer hides it. The toolbar's overflow
  menu turns line wrapping and line numbers on and off.
- **The Edit menu is no longer almost empty**: Cut, Copy, Paste and Move Items
  Here were on the keyboard and in the right-click menu but nowhere in the menu
  bar.

## Ferail speaks Polish

A complete Polish translation now ships inside the app, contributed by **Bohun**
and reviewed by a Polish speaker. Pick it in Settings, or let Ferail follow your
system language. One rough edge worth knowing: counts of two, three and four
items currently use the same wording as five and above, so a few labels read
slightly off until the pack gains its `few` forms.

## Fixes worth knowing

- **Closing Ferail now actually ends it.** Some closes left the process running
  with no window and no taskbar icon, so installing an update meant killing it
  by hand. Quitting now starts a watchdog: if the process is still alive a few
  seconds later it writes a report naming the windows that outlived the quit,
  then exits on its own.
- **Double-clicking a checksum file runs the check** instead of opening a
  column of hashes in a text editor. Open and Open With still open it as text.
- **Entering a folder previews the item it selected for you.** The pane used to
  show the file's details beside an empty thumbnail well until you clicked the
  row that was already selected.
- **Long text previews show a scrollbar**, so a `.nfo`, log or source file no
  longer looks like it ends at the bottom of the box.
- **Double-clicking a selected folder opens it** instead of renaming it. The
  click-to-rename gesture was firing on the first click of the double-click.
- **Ferail holds noticeably less memory over a long session.** The table that
  gives every file a stable identity stored each path twice; it now shares one
  copy, about 90 MB less per million files seen.
- **Private Mode paints blurred stand-in thumbnails** instead of identical grey
  boxes, so a screenshot of a real session still shows what the grid looks
  like. None of it comes from your files: every pixel is invented from a key
  created when Private Mode starts.

## Known issue

The command palette (Cmd+/) no longer lets the file list underneath steal your
hover, tooltips or scroll wheel. Scrolling the command list itself with the
wheel still stops short of the end, though: **use the arrow keys** to reach the
last commands until that is fixed.

## Still unsigned on Windows

The Windows build is not code-signed, so SmartScreen will warn on first launch.
Choose **More info ▸ Run anyway**. The macOS DMG is signed and notarized.
