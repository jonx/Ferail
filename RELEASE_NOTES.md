# Ferail 0.7.12 - Split view

Two folders on screen at once. A window can show two of its tabs side by side
or stacked, and send files from one to the other, by keyboard or by dragging.

## Split view

- **Split Right** (Cmd+Option+S) and **Split Down** (Cmd+Option+Shift+S), in
  the View menu, show two tabs at once. The same command closes the split.
- **Each pane is a tab**, with its own folder, history, selection, sort and
  list or grid view. Pick another tab to change what the focused pane shows.
- **Click a pane to work in it**, or press Cmd+Option+P (Option+Tab on
  macOS). The toolbar, sidebar, preview and status bar follow it.
- **Copy to Other Pane** (Cmd+Option+C) and **Move to Other Pane**
  (Cmd+Option+M), in the Edit menu, send the selection to the folder on the
  other side, with the usual collision prompt, progress and undo.
- **Drag the divider** to resize the panes, double-click it to even them out.

On Windows and Linux, read Ctrl for Cmd and Alt for Option.

## Worth knowing

A split is not restored when Ferail restarts. Keyboard copy and move, pane
focus and both split directions were exercised on macOS; dragging files from
one pane to the other and dragging the divider are covered by the build but
have not been tried by hand yet.

The Windows build is not code-signed, so SmartScreen warns on first launch:
choose **More info ▸ Run anyway**. **Unblock the download before extracting or
installing it**: right-click the file, choose **Properties**, tick **Unblock**,
then **OK** (or run `Unblock-File` in PowerShell). A blocked copy cannot start
its helper processes, and Windows may refuse to run it at all. The macOS DMG is
signed and notarized.
