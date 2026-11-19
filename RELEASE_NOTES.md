# Ferail 0.8.1 - Paste from History, Open With for many files, clearer errors

A release about everyday use: older copies you can paste again, Open With that
works on a whole selection, error messages you can read at a glance, and a
round of fixes to keys that misbehaved while typing.

## Paste from History

- **Cmd+Shift+V** lists your last ten copies and cuts and pastes the one you
  pick into the current folder, not just the latest. Each entry shows what was
  copied, from which folder, and how long ago. Enter pastes the newest.
- Only the files' paths are kept, in memory, never on disk. Settings >
  Privacy > **Keep Paste from History for** decides how long: 30 minutes by
  default, from Off to until Ferail quits. Private Mode records nothing.

## Open With

- **Works on several files.** Open With lists the clicked file's applications
  and opens the whole selection with the one you choose.
- **Other…** at the bottom lets you pick any application, including for a file
  type the system knows no application for.
- A launch that fails now tells you why instead of doing nothing.

## Clearer errors

- **Every error reads the same way:** the problem with its plain reason, the
  file it concerns in italics with its folder, what to do, and the system's own
  error text set apart in small type. The copy icon copies all of it.
  Notifications are a little wider so a path fits.
- **Delete Immediately** lists each item it could not delete, with the reason
  and an administrator retry, like Empty Trash.

## Small things

- **Cmd+Backspace in the Trash puts items back**, as in Finder. Put Back is
  also in the command palette and the File menu.
- **`ferail ~/Downloads`** opens a window there from the terminal.
- **Settings show a short "Saved" badge** after each change.
- **Task names are in italics** instead of between quotes, and quotation marks
  are consistent within each language.
- **The Git column appears only inside a Git repository**, and a new switch,
  Settings > Files > Git > Show Git status, turns Git off entirely.

## Fixes

- **Space types a space in every text field.** In the filter box and inline
  rename it opened Quick Look instead.
- **F2, F5 and Option+Enter no longer act on the file list while you type.**
  F2 in the filter box started a rename; F5 during a rename reloaded the list.
- **Verify Integrity reports an edited file as modified.** Browsing a folder
  after editing a file erased its recorded checksum, so the edit was recorded
  as a new baseline. A file rewritten within the same second is no longer
  mistaken for corruption where the disk keeps sub-second times.
- **Folder sizes stay right after an operation that fails partway.**
- **Some errors were explained with the wrong reason:** a busy or existing
  file could be reported as a permission problem.
- **The administrator terminal works in the signed Mac build.** macOS was
  silently refusing Ferail's request to Terminal; it now asks once.

## Worth knowing

**The new features were checked with automated tests and screenshots, not by
hand.** The administrator-terminal permission prompt in particular has only
been checked as configuration; it shows for the first time in the notarized
build.

**Other… shows every file, not only applications.** On a Mac, picking
something that is not an application is refused with a note. Paste from
History holds what you copied in Ferail, not what other apps put on the
clipboard: plain Cmd+V covers that.

**The metadata database upgrades itself on first launch.** An older Ferail
opened afterwards sets that database aside and starts a new one, losing cached
file details and the put-back records of trashed items.

On Windows the duplicate finder does not recognise hard links, so reclaimable
space is overstated when they are involved, and the NTFS index has been tested
on recorded data only.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
