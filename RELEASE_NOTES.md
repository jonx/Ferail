# Ferail 0.7.11 - RAR, WinRAR-style extraction, and a Linux Trash

RAR archives open and extract, extraction follows WinRAR's three commands,
and names with numbers sort the way Explorer and Finder sort them. On Linux,
the Trash, removable drives and localized user folders join the sidebar, in
another batch from **ooonea**.

## Archives

- **RAR archives open and extract**, from RAR 1.5 to RAR5, including
  password-protected archives and multi-part sets opened from any part.
  Ferail cannot create RAR archives: the format is proprietary.
- **Extract Here puts the contents directly in the current folder**, with no
  extra folder named after the archive. It refuses rather than mixing files
  into an item of the same name.
- **Extract to "name" gives each archive its own folder.** With several
  archives selected it reads *Extract Each to Its Own Folder*. *Extract To…*
  keeps choosing in place or a new folder for you.

## Sorting and selection

- **Numbers in names sort by value**: `2_Intro.pdf` comes before
  `10_Forms.pdf`.
- **Sorting by size no longer scatters folders whose size is not known
  yet.** They sort by name until their size arrives.
- **Rubber-band selection keeps going past the edge** of the list and of the
  window, and scrolls the list when held above or below it.
- **Long names use the full width of the Name column** and re-fit live while
  you resize it.

## Linux

- **The Trash is in the sidebar**, with Restore and Empty Trash.
- **Removable drives appear before they are mounted.** Click to mount
  through UDisks2.
- **Desktop, Documents, Downloads, Music, Pictures and Videos follow your XDG
  user folders**, localized or redirected.
- **PDF thumbnails are kept** in the shared desktop thumbnail cache.
- **The arrow keys start at the first item** when nothing is selected.

## Also in this release

- **The preview pane stays open across launches** if you left it open.
- **Context menus fit inside the window**, scrolling when taller than it.
- **The Trash view has an Empty Trash… button** on a strip above the list.
- **A failed Move to Trash shows its message again** instead of an empty
  error.

## Worth knowing

RAR is not available on the AROS build. Rubber-band scrolling past the edge
is covered by the build and tests but has not been tried by hand yet.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
