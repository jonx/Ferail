# Ferail 0.7.8 - Apps launch, archives open in place, and dead volumes stay harmless

Apps launch, archives open in place, screenshots paste, and a dead volume no
longer takes Ferail with it.

## Apps open like apps

**Double-clicking an application launches it** instead of walking into its
folder. The same goes for installers and document packages such as Keynote
files or the Photos library. To look inside, right-click and choose **Show
Contents**, or press **Option+Enter**.

## Archives open in Ferail

**Double-clicking a ZIP, 7-Zip or TAR file browses it in Ferail** instead of
letting the system extract it next to itself. Open With still hands it to
another app.

## Paste a picture as a file

**Copy a screenshot or an image, press Cmd+V in a folder**, and Ferail saves
it there as *Pasted Image* with the date and time, selected and ready to
rename. Undo takes it back.

## Volumes you can trust

**A network share, USB disk or FUSE filesystem that stops answering no
longer freezes Ferail.** It stays in the sidebar, dimmed and marked *not
responding*, with Eject offered, and every other volume keeps working.

**The Volumes list now matches Finder on macOS**: no hidden *- Data*
volumes, and *Macintosh HD* once.

## Also in this release

- **The right-click menu fits what you clicked.** Edit no longer appears on
  pictures, videos, archives or programs.
- **List rows are centered vertically**, so sizes and dates line up with the
  names beside them.

## Worth knowing

The command palette's list still stops short of the end with the scroll
wheel. **Use the arrow keys** to reach the last commands.

A volume that has stopped answering can still leave a background thread
waiting on it until the volume comes back or is unmounted; Ferail stays
responsive, but ejecting the volume is the way to release it.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. The macOS DMG is signed and notarized.
