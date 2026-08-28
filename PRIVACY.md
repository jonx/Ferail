# Ferail Privacy Policy

Effective date: 28 August 2026

Ferail is a desktop file manager maintained by John Knipper. This policy
applies to the official Ferail desktop application and command-line tools.

## The short version

- Ferail has no account system, advertising, analytics, telemetry, or remote
  file-processing service.
- File browsing, previews, metadata inspection, checksums, duplicate detection,
  similar-image search, archive operations, and Disk Usage run on your device.
- Ferail does not upload your files, filenames, thumbnails, metadata, hashes,
  search results, or usage history.
- Automatic update checks are **off by default**. A network request is made
  only when you check manually or explicitly enable automatic checks.
- Bug-report path redaction is **on by default**, and reports remain on your
  device until you choose to share them.

## Information processed on your device

Ferail reads the files and locations you choose to browse or analyse. Depending
on the feature, this can include filenames, paths, sizes, timestamps,
permissions, filesystem metadata, file signatures, media metadata, thumbnails,
checksums, archive listings, and image pixels. This processing is local.

Some optional features need broader operating-system access:

- macOS Full Disk Access allows Ferail to read locations protected by macOS;
- the Windows Fast NTFS helper uses a narrow, elevated process to read local
  NTFS metadata for a scan;
- opening network, cloud-backed, removable, or WSL locations can involve the
  operating system or the provider responsible for that location.

Granting those permissions does not cause Ferail to send the accessed data to
the Ferail developer.

## Information stored locally

Ferail stores settings and application state in the normal per-user application
data directories. Local state can include:

- preferences, window layout, tabs, Favorites, and Ant Trail visit history;
- absolute paths needed for those features;
- derived file metadata, folder-size results, and exact-duplicate hashes cached
  against a path, size, and modification time;
- locally generated crash, freeze, and issue-report files.

Ferail's own thumbnails and preview pixels are held in bounded process memory,
not written to Ferail's persistent database. The operating system or an
installed file provider may maintain its own caches independently.

Similar-image search has a stricter boundary: perceptual hashes, decoded pixels,
candidate paths, and result thumbnails are kept only by the active scan/result
surface and are discarded when that surface closes. They are not added to the
persistent metadata database. NFO content, SFV entries, and checksum
verification results are likewise not persisted by Ferail.

## Network access and updates

Ferail contains no telemetry channel and does not contact a Ferail-operated
server.

Automatic update checking is opt-in and disabled on a fresh installation. If
you choose **Ferail > Check for Updates...**, or enable **Settings > About >
Updates > Check for updates automatically**, Ferail requests the public release
list for `jonx/Ferail` from the GitHub API. GitHub receives ordinary connection
information such as your IP address and request headers under
[GitHub's Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).
No file information or usage history is included. A release asset is downloaded
only after you choose to download or install it.

With automatic update checking disabled, Ferail makes no update request on its
own. User-directed actions involving a website, network filesystem, cloud
provider, or external application may still cause that third party to use the
network under its own policy.

## Diagnostics and bug reports

Ferail does not automatically upload crash reports, freeze reports, minidumps,
screenshots, logs, or issue bundles. They are saved locally; you decide whether
to share them.

On a fresh installation, **Settings > Diagnostics > Redact file names & paths**
is enabled. **Copy report** and **Create report bundle...** then replace each
user file or folder name with `…`, retaining only structural information such
as path depth and a final file extension. The home-directory account name is
scrubbed regardless of that setting. You can see the sanitized activity trail
in Settings before sharing it. If you deliberately disable redaction, reports
may contain real paths.

Automatic text crash and freeze reports are designed to use path-free
breadcrumbs and the same sanitized activity trail, but unexpected operating-
system or library error text can contain details Ferail did not generate.
Native minidumps are memory diagnostics and can contain incidental filenames,
paths, or other process memory. Review every report and screenshot before
sending it. You never need to send personal files or an unredacted image to
report a Ferail bug.

## Retention and deletion

Local state remains until Ferail replaces it, its bounded cache evicts it, or
you delete it. Settings > Diagnostics provides shortcuts to Ferail's settings
and reports folders. The principal locations are:

- macOS: `~/Library/Application Support/Ferail`
- Windows: `%APPDATA%\Ferail`
- Linux settings: `$XDG_CONFIG_HOME/ferail` (normally `~/.config/ferail`)
- Linux metadata: `$XDG_DATA_HOME/ferail` (normally
  `~/.local/share/ferail`)

Closing a similar-image result releases its scan-local data. Deleting Ferail's
application-data folders removes its persisted settings, metadata, cached
hashes, history, and reports; it does not delete the files you browsed.

## Sharing with third parties

Ferail does not sell or share personal information. If you voluntarily publish
a report through GitHub, email, or another service, that service's terms and
privacy policy apply to what you submit.

## Changes and contact

Material changes to this policy will be recorded in Ferail's changelog and in
this file's version history. For privacy questions, use the
[Ferail repository](https://github.com/jonx/Ferail) without posting personal or
unredacted diagnostic data in a public issue.
