# Ferail 0.7.9 - Media-only slideshows and a quieter filter

Slideshows can skip everything that is not media, the filter stops getting in
the way, and it can search subfolders as you type.

## Slideshows of pictures, videos and music only

**A *Media only* checkbox in the viewer** leaves documents and other files out
of the playlist, so a slideshow of a mixed folder shows only its pictures,
videos and songs. The choice is remembered for the next viewer. In a narrow
window it moves into the **…** menu with the other slideshow controls.

## A filter that stays out of the way

**The suggestion menu under the filter field no longer pops up while you
type ordinary text.** It used to open on an empty field, after clicking × to
clear it, and after any plain word. It now opens only when what you type looks
like a filter token, such as `si` for `size:`, after a short pause, and it has
its own close button. Press **Down** in the field to see every token, or click
**(?)** for the full cheat sheet.

**Search subfolders as you type.** Turn on **View > Search Subfolders While
Typing** and the filter field searches the current folder and everything below
it after a short pause, instead of filtering only what is on screen. With the
option off, Enter still runs that search.

## Clearer empty folders

**A folder whose files are all hidden or filtered out says so**, with the
count and how to show them, instead of *This folder is empty*. The status bar
no longer says *Empty folder* beside the hidden count either.

## Also in this release

- **The status bar's right side holds still** while a folder's sizes are being
  measured; background tasks no longer make it shift back and forth.
- **The viewer toolbar folds properly in French and German** instead of
  pushing controls off the edge of a narrow window.
- **The update window shows the release name once**, and its buttons have
  room for longer labels.

## Worth knowing

The command palette's list still stops short of the end with the scroll
wheel. **Use the arrow keys** to reach the last commands.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
