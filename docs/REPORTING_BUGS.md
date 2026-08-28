# Reporting a Ferail bug

A useful report says what Ferail was doing, which exact build was running, and
includes the diagnostic file produced for that failure. You never need to send
your files, thumbnails, directory listing, or an unredacted screenshot.

**File and folder path redaction is enabled by default** for Copy report and
Create report bundle. Ferail replaces user path components before building the
shareable output, always removes the home-directory account name, saves the
result locally, and never uploads it automatically. You can inspect the exact
sanitized activity trail under Settings → Diagnostics. Native minidumps can
contain incidental process memory, so review them before sharing. See the
[Privacy Policy](../PRIVACY.md) for the complete boundary.

## What to include

1. Record the Ferail version shown in the title bar, operating system and CPU
   architecture. On Windows, say whether you used the setup or portable ZIP.
2. Give the shortest repeatable steps, what you expected, and what happened.
   Mention the selected tool and important options, plus whether the location
   was local, removable, network, cloud-backed, WSL, or an archive.
3. Add approximate scale and resource observations: item count, elapsed time,
   Task Manager/Activity Monitor CPU and memory, and whether the UI recovered.
   Approximate figures are enough; do not include file names.
4. Attach the newest matching report from Ferail's `reports` folder. Settings
   → Diagnostics shows and opens the exact folder.

Default report locations are:

- Windows: `%APPDATA%\Ferail\reports`
- macOS: `~/Library/Application Support/Ferail/reports`
- Linux: the `reports` folder under Ferail's XDG config directory, normally
  `~/.config/ferail/reports`

Private Mode (`Cmd/Ctrl+Shift+K`) keeps Ferail's layout visible while replacing
personal text and content. Use it before screenshots or video. Native dialogs
and other applications are outside Ferail's window, so check those yourself.

## Freeze or very slow operation

Leave the frozen window open for at least 15 seconds. The watchdog writes
`ferail-hang-<pid>-<sequence>.txt`. On Windows it also starts a clean broker
which writes a same-stem `.dmp`; attach **both** files because the dump contains
the thread stacks which explain where the UI stopped.

If Windows did not produce a `.dmp`, open Task Manager → Details, right-click
`Ferail.exe`, choose **Create dump file**, and attach that dump with the text
report. For macOS/Linux, a run started from a terminal can also be sampled with
`Ctrl+\` while it is stalled. Say whether restarting with `--safe-mode` changes
the result.

The maintainer must use the PDB bundle from the exact same Windows release;
PDBs from another version cannot reliably symbolize a dump. The release's
`Ferail-<version>-x64-symbols.zip` contains the matching identity manifest.

## Crash

Attach the newest `ferail-crash-*.txt` and any same-stem `.dmp` from the reports
folder. Also include the final console lines if Ferail was launched from a
terminal. Do not paste only a screenshot of an exception: the text report and
dump contain the actionable module, backtrace, and thread state.

## Testing the Fast NTFS helper on Windows

Fast NTFS reads the raw NTFS metadata of the containing volume. Windows
therefore requires an elevated helper even though the scan is read-only. The
Ferail GUI itself remains unprivileged and falls back to Portable scanning if
UAC is declined or the helper fails.

Use the helper from the same package as the Ferail build being tested. It is
beside `Ferail.exe` in both the installed application and portable folder.
Open **PowerShell as Administrator**, change to that folder, then run:

```powershell
.\ferail-ntfs-helper.exe --help
.\ferail-ntfs-helper.exe --diagnose "C:\path\to\scan" *> "$env:USERPROFILE\Desktop\fast-ntfs-diagnostic.txt"
```

The path may be a whole local NTFS volume (`C:\`) or a subdirectory. The
helper builds the volume index once and reports the requested subtree. Its
shareable output contains aggregate geometry, phases, record rates, counts,
bytes and timings—not the requested path or any file name.

Expected success ends with a `--- report ---` block containing `mft`, `subtree`,
`skipped`, and `timing` lines. When reporting a problem, attach the redirected
text and also say:

- whether PowerShell was elevated and whether UAC appeared;
- filesystem and drive type (NTFS local SSD/HDD or mounted VHDX);
- whole volume or subdirectory;
- whether Ferail's Fast scan completed, fell back to Portable, or was cancelled;
- Ferail/helper version, elapsed time, peak GUI/helper memory, and any hang or
  crash report created at the same time.

Do not use this diagnostic on FAT, exFAT, ReFS, network paths, WSL paths, or
cloud placeholders whose data is not on a local NTFS volume; those are expected
to use the portable filesystem walker.
