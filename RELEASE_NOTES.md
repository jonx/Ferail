# Ferail 0.7.10 - Linux catches up, Italian, and a screen that stays on

The first outside code contributions land: a large Linux batch and an Italian
interface, both by **ooonea**. The viewer keeps the screen awake while it
plays, and the sidebar can be trimmed to what you use.

## The screen stays on while something plays

**A video or a slideshow keeps the display awake** for as long as it runs,
and the computer is free to idle again the moment you pause, stop or close
the viewer. A new switch under **Settings ▸ Layout ▸ Viewer** turns this off.

## Linux

Fourteen pull requests from ooonea, tested on a GNOME desktop under Wayland:

- **Copy, Cut and Paste of files work on Wayland.** Ferail exchanges file
  lists with Nautilus and other file managers, including the cut intent.
- **PDFs get a real preview** in the preview pane, the icon grid and the
  viewer when Poppler's `pdftoppm` is installed.
- **The system theme follows the desktop**, light or dark, through the
  portal, and switches live.
- **Folders open in Ferail from the desktop**: the launcher accepts a
  directory, and `--navigate` works for ordinary windows.
- **JSON and other text files open in their associated application**
  instead of the browser.
- **Build and download instructions** for Linux are current.

## Italian interface

**Ferail speaks Italian.** Pick *Italiano* under Language in Appearance
settings. Every one of the catalog's strings is translated, and **file
format labels and relative dates** (*Testo semplice*, *5 giorni fa*) now
follow the interface language in every pack.

## A sidebar that shows what you use

**Favorites and the Browse tree can be hidden independently** under
**Settings ▸ Appearance ▸ Sidebar**. Hiding Favorites keeps its saved
entries.

## Also in this release

- **Going up selects the folder you came from**, whichever way you go up,
  and leaving an archive selects the archive file.
- **Folder sizes are measured once you settle on a folder**, not the
  instant it opens, so clicking through folders no longer competes with
  reading them.
- **The viewer has a right-click menu**: Show in Ferail, Copy, Copy Path,
  Close.
- **Emptying a large Trash shows its progress**, and unlocks items locked
  in Finder instead of stopping on them.
- **Escape closes dialogs, menus and popups again**, and the command
  palette scrolls all the way.
- **Notifications and dialogs stay out of Private Mode** and gain a Copy
  button.
- **Double-clicking a grid item no longer starts a rename** after opening
  it.
- **Building Ferail no longer needs Xcode's Metal compiler.**

## Worth knowing

The Linux screen-awake guard relies on the GNOME session inhibitor, with the
logind idle lock as a fallback, and has not been exercised on a Linux desktop
yet.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
