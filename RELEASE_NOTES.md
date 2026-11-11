# Ferail 0.7.14 - E-book covers and easier columns

E-books and comics show their cover, column borders are easier to grab, and
a handful of preview and listing annoyances are gone.

## Covers for e-books and comics

- **EPUB files and comic archives (`.cbz`, `.cbr`, `.cbt`) get a thumbnail**
  in the list, the icon grid and the preview pane: the cover an EPUB
  declares, or the first page of a comic.
- A comic archive saved under the wrong extension, a zip named `.cbr` for
  instance, still shows its cover.
- Password-protected archives stay on their type icon.

## Columns

- **Column separators are easier to grab.** The resize cursor appears a few
  pixels on either side of a column border in the header, instead of on a
  two-pixel strip that was hard to hit on a high-resolution display.
- **Double-clicking a column border fits the column to its content.** The fit
  left a wide empty margin, about a third of the column, because the text was
  measured at a larger size than the one the list draws it in.

## Also in this release

- **Refresh keeps the preview.** Pressing Refresh with a file shown in the
  preview pane emptied its content preview until you selected the file again,
  and the details kept the old size and dates. Both are read again.
- **Include Subfolders no longer fights itself on slow disks.** Ferail read
  the visible files for their format, description and thumbnail while the
  recursive listing was still running. On an external or network disk those
  reads slowed the listing. They now start when the listing is complete.
- **The sidebar and preview pane borders look like the Split View divider.**
  They tint on hover and turn the accent colour while dragged.
- **Choose whether folders open with their first item selected**, in
  Settings > Files, on every platform. It is on by default.

## Worth knowing

**Include Subfolders shows formats, descriptions and thumbnails only once the
listing is complete.** On a very large disk those columns stay empty until
the walk ends.

Covers were checked on macOS with EPUB and CBZ files. Reading from `.cbr`
archives is covered by automated tests, not by hand, and none of this
release has been tried on Windows or Linux yet.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
