# Checksums and file verification

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Ferail can calculate a SHA-256 checksum for one file without loading the whole
file into memory. Select a file, then choose **Generate SHA-256…** from its
context menu, the File menu, or the command palette.

The dialog opens before disk work starts and shows byte-based progress. The
worker reads the file in bounded chunks, supports cancellation from both the
dialog and task panel, and retains neither file contents nor paths after the
operation. Only the final hexadecimal checksum remains in the dialog.

## Clipboard comparison

When the dialog opens, Ferail looks for exactly one SHA-256 checksum in the
text clipboard. It recognizes a bare checksum as well as common checksum-file
forms such as:

```text
<hash>  filename.dmg
SHA256(filename.dmg) = <hash>
sha256:<hash>
```

Leading and trailing spaces, tabs, and line breaks are ignored, and uppercase
hexadecimal digits are normalized. A malformed value or text containing two
possible checksums is not imported automatically.

The expected checksum stays editable. **Clear** removes it only from this
dialog; it never clears or rewrites the system clipboard. **Copy** is the only
action that deliberately replaces clipboard text, using the generated
checksum. A match and a mismatch use distinct labels and colors so the result
does not depend on color alone.

## Safety boundary

SHA-256 verifies that the selected bytes match an expected checksum. It does
not establish who published that checksum: users should obtain the expected
value from a trusted source independent of the downloaded file.

## Multi-file manifests

Ferail also recognizes SFV, GNU-style checksum lists and BSD tagged lists by
content. **Verify Checksums…** opens a tab-local, cancellable and virtualized
report. **Double-clicking a manifest runs the check** instead of handing the
file to the system text editor, which answers no question the double-click was
asking; Open and Open With in the context menu still open it as text. One
predicate decides what counts as a manifest for both the menu entry and the
double-click (`verify::is_manifest_file_name` on the name, then the sniffed
description), so the two can never disagree. CRC32, MD5 and SHA-1 are supported for compatibility and presented as
legacy integrity checks; SHA-224/256/384/512 use the same streaming engine.

Manifest filenames are untrusted. Unix verification opens each component
relative to an already-open root with no-follow semantics; other platforms
reject symlink/reparse components and re-check canonical containment. A file
that changes while being read is reported separately rather than trusted.

**Create Checksum File…** generates a no-clobber SFV or SHA256SUMS from the
selection or current folder. Publication is atomic and cancellation removes
the private temporary file. See [SIDECARS.md](SIDECARS.md) for the complete
format, scale and privacy contract.
