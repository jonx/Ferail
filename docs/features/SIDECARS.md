# Sidecar files: NFO, SFV and checksum manifests

Status: shipped with follow-ups.

← Back to the [feature index](README.md) · [Checksums](CHECKSUMS.md) ·
[Magic sniffing](MAGIC_SNIFFING.md) · [Preview](PREVIEW.md) ·
[Tool results](TOOL_RESULTS.md)

## Product position

Ferail should understand files that describe or verify neighbouring files,
instead of treating every `.nfo` and `.sfv` as generic text.

The first complete experience is:

- scene NFO and `FILE_ID.DIZ` artwork previews with the intended CP437 glyphs
  and fixed-width layout;
- Kodi NFO files are identified as media metadata, including metadata-only,
  URL-only and combined forms;
- SFV and common checksum lists open a cancellable verification report with
  **OK**, **Mismatch**, **Missing**, **Unreadable**, **Unsafe path** and
  **Changed while reading** outcomes;
- Ferail can generate interoperable SFV and `SHA256SUMS` files;
- selecting a release folder exposes the useful sidecars inside it without
  making directory listing or painting more expensive.

This is one coherent "sidecar intelligence" feature, but it must be delivered
in independently testable phases. Detection and preview are useful without the
verification UI; verification must not depend on release-folder conveniences.

## Current implementation

The first complete vertical slice now ships in source:

- deterministic fixtures under `test-data/sidecars`, including encoded,
  malformed, hostile and optional million-entry cases;
- content-first magic types for scene/Kodi/MsInfo NFO and SFV/common checksum
  lists (`MAGIC_REVISION = 3`);
- one shared UTF-8/UTF-16/CP437/Latin-1 decoder and a bounded inert ANSI layout
  renderer used by the preview pane, including standard, bright, 256-colour
  and true-colour SGR styles;
- readable Kodi metadata summary followed by its inert local source;
- CRC32, MD5, SHA-1 and SHA-2 parsing/verification with cancellable bounded
  reads, changed-during-read detection and no-follow containment;
- a virtualized tab-local Verify surface plus File menu, context-menu and
  command-palette entry points;
- atomic no-clobber SFV and SHA256SUMS generation over a selection or current
  folder;
- a small memory-only folder-sidecar cache and preview card, with NFO Preview
  and manifest Verify actions.

Follow-ups remain: stream manifest parsing itself instead of retaining its raw
input during parse, compact the million-entry report store, add copy/retry
actions in the result surface, support SAUCE declarations, and add the optional
extras pass/name-only completeness summary. Existing problematic targets can
already be selected directly in Ferail from the report.

## Decisions that refine the original proposal

| Subject | Decision |
| --- | --- |
| Detection | Content is authoritative. An extension may select a bounded secondary check or increase confidence, but it never turns arbitrary text into an NFO or manifest. Renamed sidecars still work. |
| Kodi NFO | Recognize metadata XML, a scraping URL on its own, and the combined XML-plus-URL form. Do not require an XML declaration. Cover `movie`, `tvshow`, `episodedetails`, `musicvideo`, `artist`, `album` and movie-set roots. Never fetch a URL during detection or preview. |
| Scene-art encoding | Prefer BOM/valid UTF-8 first, then a SAUCE declaration when present, then a scored CP437 heuristic. One percentage threshold or repeated high byte is not enough. SAUCE support is still planned. |
| ANSI | Do not merely delete cursor movements: that destroys artwork layout. Render through a bounded, inert ANSI canvas. Support only the layout/SGR subset we need and discard OSC, DCS, hyperlinks, clipboard and device-control commands. |
| Verification algorithms | Verify CRC32, MD5, SHA-1, SHA-256 and the common SHA-2 list variants for compatibility. Clearly label CRC32, MD5 and SHA-1 as legacy integrity checks, not authenticity. Initially generate only SFV/CRC32 and SHA-256. |
| Manifest paths | Build a dedicated safe-relative-path resolver. `name_hazards` diagnoses suspicious display names; it is not a containment boundary. Windows backslashes in an SFV are separators, not automatically hostile. |
| Scale | Parse and verify as a stream. The result surface must remain virtualized and bounded for manifests with millions of entries; do not keep one `PathBuf`, task or progress event per row. |
| Privacy | Persist no NFO content, manifest entries, expected/actual hashes, verification report or absolute sidecar path. Do not include them in logs, diagnostics or crash reports. |
| Rename from Kodi metadata | Deferred. Pairing, collisions, multi-episode files, undo and naming policy need a separate design. Read-only metadata is safe and useful first. |
| CRC column | Rejected. Computing a column would require reading every visible file in full and violates the responsiveness contract. Verification is explicit and task-backed. |

## Formats and semantics

### Scene NFO and FILE_ID.DIZ

These are text-art documents, commonly CP437 and sometimes containing ANSI
cursor and colour sequences. Detection should combine several independent
signals:

- valid UTF-8/BOM and SAUCE metadata take precedence over guessing;
- box-drawing and shade glyph frequency;
- repeated horizontal/vertical drawing runs and plausible adjacency;
- repeated, stable line widths, commonly near 80 columns;
- well-formed ANSI sequences and scene-style separator structure.

French or other Latin-1 prose containing accented capitals is an explicit
negative fixture. `FILE_ID.DIZ` can reuse the same decoder and renderer once
NFO preview is sound.

The decoded preview is monospaced, preserves whitespace, does not wrap, and
scrolls horizontally. A hard byte/line/canvas cap prevents a hostile document
from allocating an unbounded terminal grid. The renderer never executes links,
opens files, reads the clipboard or emits terminal/device queries.

**SAUCE** means *Standard Architecture for Universal Comment Extensions*. It
is an optional 128-byte record appended to the end of ANSI/ASCII art files. It
can declare metadata such as title, author/group, date, canvas dimensions and
the intended font/code page, which lets a viewer render old artwork without
guessing. It is data only (not executable content) and Ferail's planned support
will remain local and read-only. Files without SAUCE continue through the
content/encoding heuristics already implemented.

### Kodi NFO

Kodi documents have three relevant shapes:

1. XML metadata with a known root element;
2. a scraper URL only;
3. XML metadata followed by a scraper URL.

Recognition therefore cannot be limited to `<?xml` or to four media roots.
The parser remains local, bounded and read-only. URL text is displayed as data;
Ferail does not contact it. The first useful preview extracts a conservative
set of fields such as title, original title, year, plot, season/episode, rating
and artwork references without downloading remote art.

### Microsoft System Information NFO

`msinfo32` NFO is normally UTF-16 XML. The UTF-16 BOM path must inspect the
decoded root before returning generic text/XML. Identification is useful even
if the initial preview remains the normal structured/text preview.

### SFV

An SFV data line is conceptually:

```text
relative filename CRC32
```

The eight hexadecimal CRC is parsed from the end so filenames may contain
spaces. `;` comment lines are ignored. Real files may use OEM/CP437, an ANSI
code page or UTF-8; decoding must retain the original name bytes long enough to
avoid silently targeting the wrong file after a lossy conversion.

SFV has no universal escaping convention for every possible filename. Ferail
must report names it cannot represent rather than generate an ambiguous file.
In particular, filenames containing CR or LF cannot be emitted safely.

### GNU/BSD checksum lists

Support the common untagged GNU shape and tagged BSD shape. In GNU untagged
output, the digest is followed by a mode character and filename; `*` denotes
binary mode and a space denotes text mode. GNU escaping for backslash, CR and
LF, including the leading escape marker, must round-trip. Tagged forms such as
`SHA256 (name) = digest` need an unambiguous parser, not a permissive regex.

Digest length can propose an algorithm but is not sufficient validation by
itself. Mixed-algorithm manifests are either represented per entry or rejected
with a precise diagnostic; they are never partially verified in silence.

## Global implementation invariants

1. **No I/O during render.** Detection, parsing, directory enumeration and
   hashing run on background workers with generation guards.
2. **Bounded memory.** Reads use fixed buffers, parser/result batches are
   bounded, and progress is coalesced before reaching the UI.
3. **Cancellation is prompt.** Check between read blocks and between entries.
   Closing or replacing the result surface invalidates late messages.
4. **Cloud files stay cold.** A placeholder is reported as unavailable/skipped;
   Ferail does not hydrate it merely to verify a manifest. A user may explicitly
   retry with download if the platform offers that action.
5. **A checksum is not a signature.** Every result explains that matching bytes
   only prove equality with the supplied value. CRC32, MD5 and SHA-1 receive a
   visible legacy/weak label.
6. **No hidden traversal.** Verification cannot escape the chosen root through
   lexical components, symlinks, junctions, reparse points or a race between
   validation and opening.
7. **Live filesystems are acknowledged.** File identity, size and modification
   state are sampled before and after hashing. A change produces
   `ChangedWhileReading`, never a trustworthy match.
8. **Localization and docs ship together.** Every visible string uses `tr!`;
   English extraction, French/German packs, CHANGELOG and affected feature
   notes are updated in each user-visible phase.

## Phase 0 - pure recognition

Add sidecar recognition to the native magic text path before generic XML/plain
text outcomes. Candidate structured types are:

- `NfoScene`
- `NfoKodi`
- `NfoMsInfo`
- `ChecksumSfv`
- `ChecksumList { algorithm/form }`, if the current `MagicType` shape can carry
  that detail cleanly; otherwise keep the detail in `MagicInfo`.

`MAGIC_REVISION` must be bumped so persistent old results cannot mask the new
detector. The first 4 KiB is enough to classify many files, but not to promise
an exact entry count. A Description such as “42 entries” is allowed only if the
bounded parser observed EOF; otherwise describe the format/algorithm without a
count.

The generic UTF-16 and XML early returns need to delegate to the sidecar
classifier first. The CP437 classifier must run before the current printable
ASCII ratio turns OEM text into generic binary/text.

Tests use byte fixtures, not only Unicode strings: CP437 artwork, accented
Latin-1 prose, UTF-8 artwork, all Kodi forms, UTF-16 MsInfo,
SFV comments/spaced names, GNU escaped names, malformed/mixed manifests and
large manifests whose first chunk is inconclusive.

Also teach extension compatibility that `nfo`, `diz`, `sfv`, `md5`, `sha1`,
`sha224`, `sha256`, `sha384`, `sha512` and checksum-list names are textual containers, so honest files do not
raise a disguise alert. Content classification remains decisive.

## Phase 1 - shared decoding and faithful preview

Create one byte-to-text decision path shared by magic detection and preview.
Its order is:

1. BOM-declared UTF-8/UTF-16;
2. strict valid UTF-8;
3. declared SAUCE/code-page metadata when supported;
4. confident CP437 art;
5. the existing single-byte fallback.

The existing 128-KiB/500-line preview caps remain useful, with an additional
bounded terminal-canvas cap for ANSI art. The dedicated scene-text preview is
monospaced, whitespace-preserving, non-wrapping and horizontally scrollable.
It renders the safe SGR colour subset while keeping OSC/DCS, hyperlinks,
clipboard and device controls inert.

The visual corpus contains CP437 artwork with shades, single/double box lines,
cursor placement, accented prose, and a synthetic coloured Ferail release NFO.
Scene art uses a platform terminal font selected for connected box/block glyphs
(Monaco on macOS, Consolas on Windows, DejaVu Sans Mono on Linux).

## Phase 2 - manifest parser and verification engine

Place the pure parser/verifier in `ferail-fs-native`; the GUI only schedules it
and renders reports. Reuse/refactor the bounded byte loop behind the existing
single-file SHA-256 dialog so there is one cancellation/progress implementation.
Promote `crc32fast` to a direct dependency. Existing hash crates can cover
MD5/SHA-1/SHA-2 verification; dependency changes are documented normally.

The public model should distinguish at least:

```text
Ok
Mismatch { expected, actual }
Missing
Unreadable { reason }
UnsafePath { reason }
Unsupported { reason }
ChangedWhileReading
UnavailablePlaceholder
Cancelled
```

### Safe path resolution

Manifest names are untrusted data. The resolver accepts only paths relative to
the manifest/root directory and applies the originating format's separator and
escaping rules. It rejects absolute, UNC/device, drive-relative and parent
escapes. Containment is rechecked at open time using platform-appropriate
no-follow/file-identity primitives, because lexical normalization alone does
not stop a symlink or junction inside the root.

Do not route this through `name_hazards.rs`: that module can contribute warning
labels, but it cannot be the security boundary. On Windows, `dir\\file.bin` in
an SFV normally means a relative child and must work; `..\\file.bin`,
`C:\\file.bin`, UNC and device paths must not.

### Streaming and consistency

Parse entries incrementally and schedule bounded verification work. Avoid one
worker/task/future per file and avoid per-block UI messages. Coalesce progress
by time/byte threshold. A modest disk-aware concurrency limit may help separate
files, but never parallelize reads so aggressively that verification destroys
interactive I/O performance.

Sample stable identity, length and timestamp before and after each read. Where
available, verify that the opened handle resolves beneath the accepted root.
Report a changing/replaced file rather than comparing bytes from an unstable
object.

Finding extras requires a separate directory enumeration and can cost more than
checking the manifest. Make it opt-in (or a clearly separate second pass), and
state its recursion/symlink/package policy.

## Phase 3 - Verify result surface

Use the existing tab-local tool-result architecture. The header shows manifest,
root, algorithm/security label, aggregate outcomes and coalesced progress. The
virtualized table shows aligned, labelled columns for relative name, status,
expected value (including its algorithm) and actual value when relevant, with
filters for all/problem-only/status. Status is conveyed by text/icon as well as
colour.

Actions: cancel, retry unavailable/error rows, reveal a safely resolved file,
copy a bounded/exported failure report and rerun. For a huge report, “copy all”
must use the same honest, yielding/export-oriented behaviour as huge file lists;
it cannot assemble millions of paths and hashes on the UI thread.

The compact report store should intern directories/names and represent status
and algorithm with small enums. It must not insert every manifest path into
process-lifetime file identity caches. Acceptance includes a synthetic
1,000,000-entry manifest with bounded UI memory growth, responsive scrolling,
prompt cancellation and no millions of queued callbacks.

Entry points: File menu, command palette and context menu when the selected file
is a recognized manifest. No default shortcut is necessary initially. The
feature gets its own icon and `ICONS.md` entry.

## Phase 4 - generation

Offer **Create checksum file…** with format, selection/directory scope,
recursive policy and output name. Initial outputs:

- SFV: `filename CRC32`, conventionally CRLF;
- GNU-compatible `SHA256SUMS`: digest, mode character, encoded filename.

Generation reuses the verification byte engine and writes to a temporary file
in the destination directory, flushes it, then atomically renames it. Cancel or
failure removes only that known temporary file and never overwrites a prior
manifest without explicit confirmation. Exclude the output/temp file itself.

Default recursion does not cross filesystem boundaries, follow symlinks, enter
packages, or hydrate cloud placeholders. Each option is visible rather than an
implicit platform difference. Report unrepresentable SFV names before starting
or as explicit skipped rows. Round-trip fixtures (generate then verify) cover
spaces, Unicode, backslashes, CR/LF restrictions and platform line endings.

## Phase 5 - release-folder awareness

Sidecar hints can be collected opportunistically while folder-size enumeration
already visits a directory. They are not literally free when that worker is
disabled or has no valid cache, so selecting a folder may schedule one bounded
`read_dir` to discover immediate sidecars. Store at most a compact sidecar-kind
bitmask with the existing folder cache, never names or contents.

Folder preview can then show:

- NFO available, with an explicit preview/chooser if several exist;
- checksum manifest available, with **Verify**;
- a quick name-only completeness hint such as “48 of 49 listed files present”,
  clearly separated from full byte verification;
- multiple manifests/NFOs without arbitrarily choosing the first filesystem
  entry.

Release archives, split sets and PAR2 presence can later enrich this summary.
Actual PAR2 repair is a separate, much larger feature. Archive-contained
manifests and authenticity/signature verification are also separate projects.

## Privacy and diagnostics

The feature processes personal filenames, paths, metadata descriptions and
hashes locally. The following are forbidden in the metadata database, normal
logs, breadcrumbs, hang/crash reports and analytics:

- NFO body text or extracted plot/title fields;
- manifest entry names or expected digests;
- actual digests and per-entry results;
- absolute source/root paths beyond Ferail's existing explicitly documented
  diagnostic path policy.

Logs may contain aggregate counts, timings, byte totals, algorithm, cancellation
and sanitized error categories. Tests should assert diagnostic formatting does
not include fixture names/digests. Kodi URLs are never requested automatically.
Verification reports are surface-owned and drop when their result surface
closes. Decoded NFO text follows the existing small process-memory-only text
preview LRU, and folder sidecar hints use their own 16-folder memory-only LRU;
both are evicted in memory and are never written to the metadata database.

## Delivery order and gates

| Phase | Depends on | Visible result | Principal gate |
| --- | --- | --- | --- |
| 0, recognition |, | Shipped | False-positive corpus and magic-cache revision |
| 1, decoding/preview | 0 | Shipped, including safe styled colour | Screenshot + bounded inert ANSI tests |
| 2: verifier engine | 0 | Shipped | Path containment, races, cancellation, format fixtures |
| 3: result surface | 2 | Shipped with scale/action follow-ups | Million-entry scale and responsive UI |
| 4: generation | 2 | Shipped | Atomicity and cross-tool round trips |
| 5: folder awareness | 0, 1, 3 | Initial preview card shipped | No listing/render regression |

The preferred sequence is 0 → 1 → 2 → 3 → 4 → 5. Phases 1 and 2 may proceed
independently after Phase 0. Each phase is a coherent commit with focused tests;
user-visible phases update localization, CHANGELOG and screenshots in the same
commit.

## Primary interoperability references

- [Kodi NFO files](https://kodi.wiki/view/NFO_files) and
  [parsing NFO files](https://kodi.wiki/view/NFO_files/Parsing)
- [GNU Coreutils checksum output modes](https://www.gnu.org/software/coreutils/manual/coreutils.html#cksum-output-modes)
- [cfv SFV/checksum-format manual](https://cfv.sourceforge.net/cfv.1.html)
- [SAUCE-aware Rust implementation (`icy_sauce`)](https://github.com/mkrueger/icy_sauce)
