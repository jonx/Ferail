# Magic Description Column

Rich content-derived facts about a file, rendered as a single string in a
new **Description** column to the right of Format. Inspired by the magic
column in the `bfe-explorer` Windows predecessor (a private prior codebase),
adapted to ferail's nonblocking contract and current single-`display_magic`
shape.

## Status

**Shipped (2026-05-15).**

**Sidecar formats (2026-08-26).** Content-first recognition now distinguishes
scene/Kodi/MsInfo NFO files, SFV manifests and GNU/BSD checksum lists before
the generic text path. Their descriptions include the decoded text encoding
and checksum algorithm where available. `MAGIC_REVISION = 3` invalidates stale
cached generic-text labels after this detector expansion.

**Cache correctness (2026-08-07).** Two fixes for stale labels shadowing
better answers:

- *Sniffer-revision healing.* Cached rows only invalidate on file mtime,
  so labels written by an older build (e.g. "Binary" for an Amiga hunk
  executable, before `magic/amiga.rs` existed) survived detector upgrades
  forever. `ferail_fs_native::MAGIC_REVISION` now stamps the DB; a bump
  wipes cached labels/descriptions at startup so rows re-sniff lazily.
  Bump it with every detection improvement (see MAGIC_SNIFFING.md).
- *mtime validation on read.* The prefetch cache read served a row
  without comparing its stored mtime to the live file's, so an in-place
  edit kept the old label/description/quarantine state until a manual
  Refresh. Mismatched rows are now treated as misses.

**OOXML central directory read + OLE2/CFBF family (2026-08-04).** Two
fixes for Office files misdescribed as "ZIP archive" / "Binary data":

- *Targeted CD read.* The ZIP refine pass could only see central-directory
  entries inside the first/last 4 KB windows. A routine `.pptx` has a
  10–25 KB CD that starts *before* the tail window, so its entry names
  were invisible and the file stayed a plain ZIP (small decks happened to
  fit and did refine — hence the inconsistency). `parse_central_directory`
  now takes a `read_at` closure and performs **one bounded targeted read**
  of the CD itself (capped at 128 KB) when it lies outside both windows.
  ZIP64 marker values (`0xFFFF` / `0xFFFFFFFF`) are resolved through the
  ZIP64 EOCD record while we're there.
- *OLE2/CFBF detection.* The `D0 CF 11 E0` compound-file container —
  legacy `.doc`/`.xls`/`.ppt`, MSI, and **password-protected OOXML**
  (an encrypted `.pptx` is a CFBF wrapping an `EncryptedPackage` stream,
  not a ZIP) — previously fell through to "Binary data". New
  `magic::ole` sniffs the container and refines the app from the first
  directory sector (one more targeted read): `WordDocument` /
  `Workbook` / `PowerPoint Document` → `DocWordOle` / `DocExcelOle` /
  `DocPowerPointOle` ("… · legacy format"), `EncryptedPackage` →
  `OleCompound` + encrypted ("Office document · encrypted"), VBA
  storages → macro flag. The `*Ole` types are deliberately distinct
  from the ZIP-based `Doc*` types so `archive::probe_format` never
  offers them for ZIP browsing.

The read budget for ZIP/CFBF is now up to three bounded reads: 4 KB
header, 4 KB tail, and one targeted CD / directory-sector read. Because
descriptions are cached by `(path, mtime, size)`, rows sniffed before
this fix keep their stale "ZIP archive" text until Refresh (F5) forces a
re-sniff or `--reset-db magic` clears the cache. A dev probe exists for
checking a file by hand:
`cargo run -p ferail-fs-native --example magic_probe -- <files…>`.

**Folder counts share the column (2026-07-22).** The column is content
facts for *files*; for *folders* it now shows recursive item counts —
"N files · M folders" (singular-aware; "Empty" for an empty tree). These
come for free from the background folder-size walk (the same pass that
sums a directory's recursive bytes now also counts its files and
sub-folders), cached alongside the size in the `folder_sizes` row and
formatted by `ferail_fs_native::folder_contents_summary`. The two never
collide: a directory has no magic facts, so the prefetch worker leaves its
description empty (and its apply only overwrites a *non-empty* value), while
the folder-size worker owns folder descriptions. See
[FRESHNESS.md](FRESHNESS.md) for the cache contract.

**ELF OS-ABI + relocatable (2026-06-24).** The ELF parser now reads
`e_ident[EI_OSABI]` (byte 7) into `MagicInfo::os: ElfOs` and flags
`e_type == ET_REL` as `is_relocatable`. Named OSes (AROS, the BSDs,
Solaris, explicit GNU/Linux) get an OS suffix in the description; the
System V default (used by ordinary Linux toolchains) adds none, so the
pre-existing description shape is preserved. AROS ships its libraries as
aarch64 relocatables — `exec.library` now reads as
`ELF · 64-bit · relocatable · ARM64 · AROS`. The Format column label is
unchanged (`ELF executable`), keeping the icon classifier's `executable`
tint.

**Refresh forces a re-sniff (2026-06-24).** Because the cache is keyed by
`(path, mtime, size)`, a row whose *derived* data went stale without the
file changing — e.g. after the sniffer's own logic changed — would keep
serving the old label/description. The Refresh command (F5 / toolbar)
now arms `Tab::force_resniff`, which the prefetch worker honors by
ignoring the magic/description read cache and re-sniffing every visible
row from disk. The fresh result writes through, so the cache self-heals
and the next ordinary load is a hit again. Quarantine state stays
cache-first; the broad `--reset-db magic` CLI path remains for clearing
the whole cache at once. (Edge case: if a forced re-sniff now yields an
*empty* description for a row that previously had one, the in-memory cell
updates but `upsert_file`'s COALESCE keeps the old DB value — harmless
for the enrichment case that motivated this.)

- `ferail-fs-native::magic` module ported in full from bfe-explorer
  (8 files, ~1500 lines): structured `MagicInfo` / `MagicType` /
  `CpuArch` / `PeSubsystem`, ` · `-joined `description()` formatter,
  per-family parsers (PE / ELF / Mach-O / ZIP / Office / JAR / APK /
  PNG / JPEG / GIF / BMP / WebP / ICO / TIFF / MP3 / FLAC / WAV / Ogg /
  MP4 / MOV / AVI / MKV / shebang / UTF-16 / INI / .reg / .url / .lnk /
  SQLite / fonts).
- `HEADER_BYTES`: 512 → 4096.
- `FileEntry::display_description` (`ferail-core`).
- `ferail-meta` schema v3: `files.description` column, idempotent
  `ALTER TABLE` migration, COALESCE upsert, `ResetScope::Magic` clears
  description alongside `magic_label`.
- Prefetch worker: one `detect_magic_info` call per file (one 4 KB
  read), derives label + description, write-through to DB.
- File-list UI: `description` column added at 320px, not sortable.
- Tests: 80 unit tests across `ferail-core` / `ferail-fs-native` /
  `ferail-meta` — all pass.
- Live smoke test: GUI navigates `target/debug`, prefetch processes
  28 rows and applies the batch with no panic.

### Visual verification

Screenshot at [magic-description.png](../images/magic-description.png)
shows real values populated by content sniffing on a folder of mixed files:

- `screenshot.png` → `PNG image`
- `photo.jpg` → `JPEG image`
- `project-bundle.zip` → `ZIP archive`
- `cargo-lock.gz` → `Gzip archive`
- `run.sh` → `Shell script`
- `report.txt` → `PNG image` with the mismatch triangle (a PNG wearing a
  `.txt` extension — magic wins, the ⚠ flags the disagreement).

The headless `--screenshot` path now works on Windows via a
PrintWindow capture in `ferail-shell-win32` (gpui_windows doesn't
implement `render_to_image`; see
[windows-port.md](windows-port.md)).

The unified [Format column](../../crates/ferail-gpui/src/file_list.rs)
(`Kind ⊎ Magic` with the mismatch triangle) stays. Description is **added on
top** — it never replaces Format and never displaces the mismatch indicator.

## Goal

Turn rows like

```
mytool.exe        4.2 MB   PE / DOS executable   Mar 4
song.mp3          5.1 MB   MP3 audio (ID3)       Feb 1
archive.zip       12 MB    ZIP archive           Yesterday
```

into

```
mytool.exe   4.2 MB   PE / DOS executable   Mar 4       Windows PE · 64-bit · x86-64 · GUI · .NET
song.mp3     5.1 MB   MP3 audio (ID3)       Feb 1       MP3 · stereo · 44 kHz · 03:24
archive.zip  12 MB    ZIP archive           Yesterday   ZIP archive · 14 files · root: project-v1.0
```

## Prime-Directive Constraints

The Description column is **derived data** like `display_magic`. It is:

- Computed off the UI thread. Never read from paint.
- Populated lazily after the row exists (empty string until the worker fills it).
- Cancelable / droppable — a result that arrives after the directory changed
  is discarded by `apply_batch`'s bounds check, same as magic today.
- Cached in the metadata DB by `(path, mtime_unix, size)`. On cache hit, no
  re-read.
- Bounded I/O. Cheap pass reads the same ≤ 4KB header magic already reads.
  Expensive pass (ZIP central directory, MP3 frame scan) only runs in the
  second tier and stops at fixed byte budgets.

## Single-Pass Strategy

**Verified against bfe-explorer source (Nov 2026 read of
`crates/ferail-ui/src/magic/types.rs`):** the reference implementation does
everything in **one synchronous read of 4 KB**. The parsers are byte-twiddling
on a fixed buffer — microseconds per file, dominated by the I/O. A two-pass
split would add complexity for no measurable win.

### Read budget

Bump `HEADER_BYTES` from **512 → 4096**. Required for:

- ZIP macro detection (`vbaProject.bin` is rarely in the first local file
  header; `find_in_zip_entries` walks up to 20 entries via
  `compressed_size` jumps and needs the buffer to reach them).
- JPEG `SOF` marker placed after large EXIF metadata.
- PE optional header + CLR data dir when `pe_offset` is non-trivial
  (`pe_offset + 24 + 112 + 14*8 ≈ 0x1A8` is normal).
- MP3 with sizable ID3 tags (tag size is 7-bit-syncsafe; large tags push
  the first frame well past 512 bytes).
- MP4 `moov` atoms placed near the start.

Cost: 3.5 KB extra per file read. Negligible — the buffer is stack-allocated,
the read is one disk block either way on most filesystems.

### Dispatch order

Per bfe-explorer's `sniff_bytes_info`:

1. **Executables**: PE / ELF / Mach-O — full structured parse.
2. **AmigaOS family** (`magic/amiga.rs`): hunk binaries, Workbench icons,
   IFF containers, tracker modules, disk images. See below.
3. **ZIP-based**: Office (.docx/.xlsm/.pptx + macro variants), JAR, APK,
   generic / encrypted ZIP.
4. **Images**: PNG / JPEG / GIF / BMP / WebP / ICO / TIFF — width × height +
   alpha where extractable.
5. **Audio**: MP3 (ID3 or raw frame) / FLAC / WAV / Ogg — channels, sample
   rate, bitrate, duration.
6. **Video**: MP4 / MOV / AVI / MKV / WebM — `has_video` / `has_audio` via
   box / chunk scan.
7. **Signature table fast path**: PDF, RAR, 7z, Gzip, LHA, SQLite, Lnk, etc.
   — no metadata beyond the type.
8. **Text heuristic**: shebang → script subtype, UTF-16 BOM, XML / HTML /
   .reg / INI / .url / JSON detection via prefix sniffing on `text.trim_start()`.
9. **Binary fallback**: < 85% printable → `Binary`; else `Unknown`.

### AmigaOS formats

Cross-platform, not an AROS feature: an Aminet download sitting on a Mac is
still a hunk binary full of `.info` icons and ProTracker modules, and all of
these are pure header reads. Everything is big-endian — the formats were
designed on a 68000.

| Format | Detection | Facts reported |
|---|---|---|
| Hunk binary | `0x03F3` header / `0x03E7` unit / `0x03FA` library | `68k`, hunk count |
| Workbench icon | `0xE310` (`DiskObject`) | kind (tool/drawer/project/disk/…), pixel size |
| IFF ILBM / PBM / ACBM | `FORM` + form type, `BMHD` chunk | width × height, planar depth |
| IFF 8SVX / 16SV | `FORM` + form type, `VHDR` chunk | mono, sample rate |
| IFF ANIM / SMUS | `FORM` + form type | type only |
| Tracker module | tag at offset **1080** (ProTracker), or `MMD0`–`MMD3` / `Extended Module:` / `IMPM` / `SCRM` | family name, channel count |
| ADF / DMS | `DOS` + filesystem flag ≤ 7 / `DMS!` | type only |
| LZX, AmigaGuide | `LZX` / `@database` | type only |

Two ordering constraints, both covered by tests:

- The sniffer runs **before** the generic signature table so `FORM`
  containers and the `DOS` bootblock are classified precisely.
- It deliberately **declines** `FORM….AIFF`/`AIFC`. Those are IFF too, but
  the audio parser reports channels and duration for them, which is strictly
  better than a bare type label.

`EM_68K` (4) also joins the ELF architecture table, so a 68k ELF — as opposed
to a hunk binary — no longer drops its architecture from the description.

### What v1 ports

**Faithfully**: PE / ELF / Mach-O / ZIP / PNG / JPEG / GIF / BMP / WebP / ICO
/ TIFF / MP3 (ID3 + raw) / FLAC / WAV / Ogg / MP4 / MOV / AVI / MKV / shebang
script subtypes / Office subtypes (.docx/.xlsm/.pptx + macros) / JAR / APK
/ UTF-16 / INI / .reg / .url / .lnk / SQLite / PDF / RAR / 7z / Gzip.

**Deferred (acceptable for v1)**:

- Universal Mach-O fat-binary architecture enumeration (currently just notes
  "Mach-O 64-bit" without per-slice arch).
- TIFF IFD-offset values that point outside the 4 KB buffer.
- MP4 with `moov` placed at end of file (very large videos exported by
  certain encoders).
- ZIP file count and `zip_layout` (single-root-folder name) — these need
  the central directory at EOF, which is genuinely a second I/O. Treat as
  Phase 2 if/when we want them.
- Image dimensions for JPEGs with > 4 KB of leading EXIF.

## Data Shape

### `ferail-fs-native::magic`

Replace the current single `magic.rs` (187 lines, flat byte-pattern table)
with a `magic/` submodule:

```
magic/
├── mod.rs          // detect_magic + detect_magic_info + dispatch
├── types.rs        // MagicType enum, MagicInfo struct, description()
├── exe.rs          // PE / ELF / Mach-O parsers
├── zip.rs          // ZIP + Office subtype + JAR/APK + find_in_zip_entries
├── image.rs        // PNG/JPEG/GIF/BMP/WebP/ICO/TIFF dimension extractors
├── audio.rs        // MP3 (ID3 + raw frame) / FLAC / WAV / Ogg
├── video.rs        // MP4/MOV/AVI/MKV box+chunk scans
└── text.rs         // shebang, UTF-16, XML/HTML/.reg/.url/INI/JSON heuristic
```

Per the Slow AI file-size discipline: parsers are pure byte-twiddling tables;
each file lands at 100-300 lines, which is fine. Splitting along format
families keeps each file mentally cohesive.

```rust
// types.rs — ~50 enum variants, structured info struct
pub enum MagicType {
    // Documents
    Pdf,
    // Office (ZIP-based, with macro variants)
    DocWord, DocWordMacro, DocExcel, DocExcelMacro,
    DocPowerPoint, DocPowerPointMacro,
    // Archives
    Zip, ZipEncrypted, Rar, SevenZip, Tar, Gzip,
    // App packages
    AppJar, AppApk,
    // Images
    Jpeg, Png, Gif, Bmp, Webp, Ico, Tiff,
    // Video / Audio
    Mp4, Mov, Avi, Mkv, Webm,
    Mp3, Wav, Flac, Ogg,
    // Data
    Json, Xml, Html, Sqlite,
    // Executables — cross-platform symmetric
    ExeWindows, DllWindows, ExeWindowsNet,
    ExeLinux, SoLinux,
    ExeMac, DylibMac,
    // Scripts
    ScriptBash, ScriptPython, ScriptPerl, ScriptRuby, ScriptNode, ScriptOther,
    // Text subtypes
    TextIni, TextReg, TextPlain,
    // Windows shortcuts
    Lnk, Url,
    // Generic
    Folder, Unknown, Binary,
}

pub struct MagicInfo {
    pub magic_type: MagicType,
    pub is_64bit: Option<bool>,
    pub arch: CpuArch,
    pub subsystem: PeSubsystem,
    pub is_dotnet: bool,
    pub has_macros: bool,
    pub is_encrypted: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_alpha: bool,
    pub channels: Option<u8>,
    pub sample_rate: Option<u32>,
    pub bitrate_kbps: Option<u16>,
    pub duration_secs: Option<u32>,
    pub has_video: bool,
    pub has_audio: bool,
    pub interpreter: Option<&'static str>,
}

impl MagicInfo {
    pub fn description(&self) -> String;  // " · "-joined facts
}

// mod.rs — read 4 KB, dispatch, return
pub fn detect_magic_info(path: &Path) -> Option<MagicInfo>;
pub fn sniff_bytes_info(buf: &[u8]) -> MagicInfo;  // unit-testable
```

The existing `detect_magic` stays as a thin wrapper so callers that only
want a label (`ferail-disk-usage::file_category`, `icons::classify_file`)
don't break:

```rust
pub fn detect_magic(path: &Path) -> Option<&'static str> {
    detect_magic_info(path).map(|i| i.magic_type.display_name())
}
```

`MagicType::display_name()` matches ferail's current label strings where
possible (`"PNG image"`, `"PE / DOS executable"`, `"ZIP archive"`) so the
Format column doesn't visibly change. Where bfe-explorer subdivides
(`ExeWindows` vs `DllWindows`), `display_name()` collapses back to ferail's
existing label by default; the richer distinction lives in `MagicType` for
the Description formatter.

### `ferail-core::FileEntry`

One new field:

```rust
pub display_description: String,
```

Defaults to `""`. Populated by the prefetch worker like `display_magic`.

### `ferail-meta` schema

Add one column to the `files` table:

```sql
files.description TEXT       -- new, NULL until computed
```

Bump `DB_VERSION`. `FileMetaRecord` gains:

```rust
pub description: Option<String>,
```

`upsert_file` follows the existing `COALESCE(excluded.X, files.X)` pattern so
Pass 2 writes don't blow away Pass 1's row.

Reset scope: extend `ResetScope::Magic` to also `NULL`-out
`files.description` (they're paired — re-sniffing magic without re-deriving
the description would leave stale rows).

## UI

### `ferail-gpui::file_list`

Add a fourth column, after Modified or before it — defer the order to
`get_design_feedback` since the user will move it anyway. Default order:

```rust
columns: vec![
    Column::new("name", "Name").width(360.0).sortable(),
    Column::new("size", "Size").width(100.0).sortable(),
    Column::new("format", "Format").width(220.0).sortable(),
    Column::new("modified", "Modified").width(160.0).sortable(),
    Column::new("description", "Description").width(320.0),  // not sortable in v1
],
```

Cell renderer for `"description"`:

```rust
"description" => div()
    .text_xs()
    .text_color(cx.theme().muted_foreground)
    .child(SharedString::from(entry.display_description.clone()))
    .into_any_element(),
```

Empty string renders as an empty cell — no skeleton shimmer in v1.

**Not sortable in v1.** Description strings sort lexicographically, which
groups MP3s near MP4s but separates 32-bit and 64-bit executables — both
are confusing. Wire sort after we see real columns in use.

### Format column

Untouched. Description is purely additive; the mismatch triangle still lives
on Format.

### Sort column ID lookup

`SortColumn::from_id` in `file_list.rs:795` gets no new arm — Description
isn't sortable. If we add sortability later it would be a separate
`SortColumn::Description` (probably falling back to magic-then-name).

## Prefetch Worker Changes

[`prefetch.rs`](../../crates/ferail-gpui/src/prefetch.rs) is the only
worker we touch:

1. `PrefetchSeed` gains `has_description: bool`.
2. `PrefetchRow` gains `description: String`.
3. `run_worker`:
   - DB cache lookup pulls `description` alongside `magic_label`.
   - On miss, call `detect_magic_info(&path)` (one 4 KB read, replaces the
     current `detect_magic` call).
   - Derive `(label, description) = (info.magic_type.display_name(),
     info.description())`.
   - Upsert `FileMetaRecord { description: Some(...), .. }`.
4. `apply_batch` writes `e.display_description = row.description` when
   non-empty (same staleness guards as `display_magic`).

The task registry's single `MagicPrefetch` entry stays — description is
part of the same indexing pass, not a separate workload.

## Performance Budget

Per-row cost is **dominated by the 4 KB read** (one disk block on most
filesystems — essentially free). The parsing additions are pure
byte-twiddling on the in-memory buffer:

- PE / ELF / Mach-O header: ~50 lines of slice indexing, no allocation.
- Image dimensions: 4 specific byte ranges per format, no allocation.
- Shebang: one `split_once`, one interpreter constant lookup.
- ZIP entry scan: up to 20 local file headers walked via `compressed_size`
  jumps; bounded by buffer length.

The description formatter allocates one `String` per file (the
` · `-joined output, typically 20-60 bytes). For a 50 000-row folder that
is 50 000 strings averaging ~40 bytes — negligible against the existing
`display_magic` allocation per row.

The metadata DB column adds one `TEXT` per file. Existing `files` table
already has 11 columns; one more is invisible.

**The Format column never blocks on this work.** Detection runs in the
existing `cx.background_executor().spawn` call. UI thread reads
`display_description` purely for paint — same contract as `display_magic`
has always had.

## Test Plan

Unit:
- `MagicInfo::description()` produces the expected string for every variant
  combination (snapshots in `ferail-fs-native`).
- `detect_magic_info` on fixture binaries (PNG, MP3, .docx, PE, ELF, Mach-O)
  populates the expected fields. Use the same fixture pattern
  `crates/ferail-fs-native/tests` already has.
- `formats_compatible` in `ferail-core` is unchanged.

Integration:
- Open the repo's `target/` directory; verify executables show
  `… · 64-bit · x86-64 · console` (or similar).
- Open `~/Pictures`; verify images show `… · 1920×1080`.
- Open a folder of MP3s twice; verify the second open hydrates Description
  from the DB without re-reading.
- Reset DB with `--reset-db magic`; verify Description columns clear.

UI:
- Screenshot a populated Description column at default width and at 800px
  width (truncate vs full).
- Confirm Format column still shows the mismatch triangle when applicable.

## Open Items

- **Sortability**: deferred until we see real usage.
- **Pass 2 scheduling**: viewport-aware vs idle-only — try idle-only first,
  upgrade if users complain about "MP3" sitting around without metadata.
- **Cancellation between passes**: Pass 1's worker doesn't currently get
  cancelled when the user navigates away. Pass 2 should respect a generation
  counter (or the same `shell_weak.upgrade()` check) so it doesn't keep
  reading bytes for a folder the user left 3 seconds ago.
- **Cloud placeholders**: Mac note from
  [MAGIC_SNIFFING.md](MAGIC_SNIFFING.md) applies double for Pass 2 (ZIP CD
  is at the *end* of the file → forces a full download on cloud-only
  files). Pass 2 must short-circuit on cloud-placeholder xattr.

## Reference

bfe-explorer's `MagicInfo::description()` is the canonical formatter. Port
the match arms verbatim where types overlap; deviate only where ferail's
type names differ (we have `"ZIP archive"` where bfe has `Zip` /
`ZipEncrypted` split — the formatter handles both since it reads structured
fields, not the label string).
