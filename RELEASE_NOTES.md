# Ferail 0.7.13 - Privacy settings and tab restore

Ferail reopens the tabs you had, and a new Privacy page in Settings decides
what it keeps about the folders you browse.

## Your tabs come back

- **Launching Ferail reopens the tabs of the last session**, in the same
  order, with the same one in front. Before, only the last folder came back.
- A folder that has since disappeared is skipped. Search, duplicate, archive
  and Disk Usage tabs are not reopened, and neither is a split.

## Settings > Privacy

- **Remember nothing** keeps visited folders, file and folder caches, the
  open tabs and put-back records in memory only, and bypasses the system
  thumbnail cache. What was stored before is left alone and returns when you
  turn it off. Favorites and settings are still saved.
- **Reopen the last session** can be turned off to always start in your home
  folder.
- **Record visited folders** can be turned off; Recents and the Ant Trail
  then stop growing.
- **Excluded locations**: folders, and optionally every other volume, that
  Ferail browses normally and records nothing about. Adding one also erases
  what was already stored under it.
- **Use the system thumbnail service** can be turned off. Ferail then decodes
  common image formats itself; PDFs, most videos and documents show their
  type icon.
- **Clear stored data** erases visited folders, caches, put-back records,
  the saved session or reports, one button each.
- History and caches are kept 7, 30 or 90 days or a year. Reports can be
  deleted after 7, 30 or 90 days.

## Also in this release

- **A Split View button in the toolbar**, next to New Folder. Click to split
  side by side, Shift-click to stack, click again to close the split.
- **The viewer's Adjustments panel fits its labels in every language.**
  Longer translations no longer wrap or push buttons past the edge.
- **A `--screenshot` run no longer changes the folder Ferail opens next.**

## Worth knowing

**Stored history now expires.** A folder not visited for 90 days leaves
Recents and loses its Ant Trail tint, unless you pick a longer period in
Settings > Privacy. Before, only file and folder caches aged out.

Tab restore and the Privacy page were exercised on macOS. Remember nothing is
covered by automated tests of the store, not by a full session by hand, and
none of this has been tried on Windows or Linux yet.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
