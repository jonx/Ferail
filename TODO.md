# Ferail TODO

← [Project README](README.md) · [Documentation map](docs/README.md) ·
[Architecture](docs/ARCHITECTURE.md) · [Status](docs/STATUS.md)

The single list of unfinished work, grouped by area and ordered by priority.
What is *done* is in [docs/STATUS.md](docs/STATUS.md); how a feature is built
is in [docs/features/](docs/features/README.md); the program rules are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). When an item ships, delete it
here and let [CHANGELOG.md](CHANGELOG.md) and git history carry the record.

<!-- toc depth=2 -->

- [Known bugs](#known-bugs)
- [Highest priority](#highest-priority)
- [High-value features](#high-value-features)
- [File list, sidebar and navigation](#file-list-sidebar-and-navigation)
- [File ops, Trash and drag](#file-ops-trash-and-drag)
- [Search](#search)
- [Preview, Get Info and viewer](#preview-get-info-and-viewer)
- [Metadata and intelligence](#metadata-and-intelligence)
- [Responsiveness and data architecture](#responsiveness-and-data-architecture)
- [Settings, commands and accessibility](#settings-commands-and-accessibility)
- [CLI and automation](#cli-and-automation)
- [Packaging and polish](#packaging-and-polish)
- [Cross-platform](#cross-platform)
- [Cleanup](#cleanup)

<!-- /toc -->

## Known bugs

- **The command palette cannot be scrolled to the end.** Cmd+/ lists 108
  commands; the wheel stops around row 62 and will not go further. Measured:
  60 and 200 wheel notches land on the identical row, shrinking the card's
  `max_h` from 460 to 300 makes the reachable end *earlier*, and neither a
  definite card height nor an active filter changes it. So the clamp tracks
  the viewport, not the content length, inside gpui-component's `Command`
  scroll handle. Arrow keys reach every row, which is the workaround shipped
  in the 0.7.7 notes.
- **Rows overlap while scrolling the list in Flat View.** Painted rows smear
  over each other during a fast scroll, yet clicking selects the correct row,
  so this is paint, not layout or hit-testing. Not reproduced headlessly yet;
  needs a capture of a real scroll on the reporter's machine.
- **36 catalogue commands have no palette action.** `action_for_command` in
  `keyboard_help.rs` maps 68 of 104 `CommandId`s; the rest render disabled, so
  the palette shows commands it cannot run. Most are context-only (tag colours,
  Open With slots) and correctly disabled, but the list has not been audited
  since the catalogue grew.

## Highest priority

- **Windows reliability and compatibility campaign.** Every item WIN-001 to
  WIN-017 is implemented; what is open is qualification, which by the ledger's
  own rule is never claimed from macOS. Work the remaining Windows-only gates
  in [WINDOWS_COMPATIBILITY_PLAN.md](docs/features/WINDOWS_COMPATIBILITY_PLAN.md)
  against the
  [reliability test plan](docs/testing/WINDOWS_RELIABILITY_TEST_PLAN.md),
  including its 4.194.304-row regression gate, resuming from the
  [handover](docs/testing/WINDOWS_HANDOVER.md). Open in particular: independent
  administrator-equipped qualification of the Fast NTFS helper across the VHDX
  and large-volume matrix, the adversarial cases (MTP disconnect, hostile
  providers, long soaks, multi-DPI), and WTEST-130 to 139 for the WSL
  locations.
- **Editors must open a file of any size, the same way listings do.** The
  built-in editors *refuse* what they cannot handle comfortably: text past
  2 MiB or 100.000 lines, images past 64 MP
  ([TEXT_EDITOR.md](docs/features/TEXT_EDITOR.md),
  [IMAGE_EDITOR.md](docs/features/IMAGE_EDITOR.md)). A refusal with a hand-off
  to the system editor is honest, but it is the one place in Ferail that gives
  up on scale, and the million-row case is the whole premise. In order:
  - **Text: stop loading the whole file.** `read_for_edit` reads the file into
    one `String`, so a 2 GiB log is out of reach by construction. The shape
    that scales is the one the file list already uses: a windowed view over
    the file (memory-mapped or paged), rendering only the visible span, edits
    accumulated as a piece table rather than a mutated buffer.
    gpui-component's `EditorState` is documented for ~50K lines, so the huge
    file path likely needs its own read-mostly viewer with an edit overlay.
  - **Text: save without materialising.** With a piece table the save is a
    streamed copy through `safe_write`'s sibling-then-in-place dance, applying
    spans in order, never holding the file in RAM.
  - **Images: tile the canvas.** Strokes are already in full-image coordinates
    and composited off-thread, so the model survives; what breaks past 64 MP is
    the single full-res RGBA buffer in the save worker (4 B/px). Compositing
    and encoding in horizontal strips lifts the cap to whatever the codec can
    stream.
  - Until then the refusal states stay, and they must keep naming the reason
    and offering the system editor.
- **A `--no-default-features` CI leg.** `bundle-mac.sh` builds without the
  `screenshot-harness` feature and no CI job builds that configuration, so a
  release build broke on a feature-gated symbol that every green check had
  compiled. One cheap `cargo check -p ferail-gpui --no-default-features`
  closes it.

## High-value features

Net-new, but each sits on plumbing that already exists.

- **Smart Folders / Saved Searches.** Wire the reserved
  `FavoriteTarget::SavedSearch` (favorites.rs) into a real feature: pin a
  search as a favorite that re-runs live on click, Spotlight-backed where
  available, with the search glyph already rendering. Mostly wiring (a
  favorite type plus a persistent search identity; search mode is ephemeral
  per tab today), not new architecture.
- **Clipboard history stack.** A bounded ring of recent copies and cuts plus a
  paste picker (Cmd+Shift+V) to choose an older entry. The clipboard plumbing
  in `shell/file_ops.rs` and `CF_HDROP` on Win32 is already ours: this is a
  buffer and a picker on top.
- **File-level frecency feeding search ranking.** Extend the Ant Trail, which
  already logs folder visits in SQLite with a decay concept, to file opens, and
  feed frequency × recency × relevance into result ordering. Shares the
  file-open signal that recently-opened files needs, and pairs with the Ant
  Trail decay item under Metadata & Intelligence.
- **Open With: custom tools, "Other…", and multi-selection**
  ([OPEN_WITH.md](docs/features/OPEN_WITH.md)). The gaps: the submenu is
  `SingleOnly` even though `open_with_slot` already resolves the whole
  selection and calls `open_with_app_many`; an empty candidate set hides the
  submenu entirely, exactly when an unknown extension most wants a specific
  tool; there is no "Other…" chooser (`gpui::PathPromptOptions` has no type
  filter and a macOS `.app` is a directory, so this needs a platform
  `NSOpenPanel` entry point); and no user-defined tools. The tool model should
  be a sibling of `ferail_core::terminal::TerminalSpec`: program plus
  pre-split argv tokens plus per-token placeholder substitution, argv never a
  shell string, persisted as a shareable `tools.json` rather than in the
  recreate-on-version-mismatch metadata DB, matched only against cached
  `FileEntry` fields, and dispatched through closure-backed menu items. Phase 1
  is the three cheap gaps; custom tools follow.
- **Target panel ("Pick as Target") and batched transfers.** A pinned, frozen
  second listing on the right: right-click a folder anywhere, pick it as the
  target, and it stays put as a source or destination until replaced or
  closed. No navigation, no spring-load, drops onto its folder rows allowed.
  It reuses the existing table and `FileListDelegate` (already multi-surface
  via `asset_scope`); the coupling to break is that context-menu actions
  resolve against the active tab through `Shell::resolve_targets` (27
  `context_row` sites, three real functions). Phase 2 stages transfers into a
  reviewable queue applied on commit: a batch, explicitly not a transaction,
  re-validated at commit time.
  Design: [TARGET_PANEL.md](docs/features/TARGET_PANEL.md). Supersedes a
  dual-pane split for the reorganize-across-folders case; the split stays
  optional and composes with it later.

## File list, sidebar and navigation

- Finish the hover/focus/selected-state consistency audit. The app carries a
  `ferail_design` token set that is dead for colour while every surface pulls
  ad hoc from the gpui-component theme plus `selection_colors`, giving five
  hover treatments and three selection systems. Wire one semantic token layer
  (or standardize every surface onto the existing `*_hover` / `*_active` /
  `ring` tokens) so tabs, breadcrumbs, rows, grid and sidebar read as one.
- Add sidebar collapse-to-icons and narrow-window behaviour; give the sidebar a
  keyboard focus region (this also unblocks Favorites arrow-key navigation).
- Add a "Reveal in Browse" file-list context action.
- More Finder-style sidebar roots: a dedicated Network browse root, and
  splitting Volumes into Internal / External / Network sections.
- Breadcrumb completion polish: inline segment-mode filtering.
- Toolbar **grouping by kind or date**: a shared list and grid sort/render
  model with group headers and members beneath. Deferred from the density
  pass.
- Persist per-tab sort/filter/scroll state where it is not already stable.
- Mirror the hidden-file summary into folder Get Info via the Calculate walk.
- Flat View at the million-plus scale: build sort and filter indexes
  off-thread, add segmented scroll geometry, and a page-backed spill path for
  result sets larger than RAM. A dedicated sub-100-byte Flat row is deferred
  unless later measurement justifies the complexity.
- Context-menu follow-ups: a compact Finder-style tag swatch row, async Open
  With prewarm if cold-cache stutter appears, and per-target enable/disable
  rules for read-only volumes, missing files and permission-denied targets.
- Tags checkmarks over a multi-selection read only the clicked row's `tags`
  while the toggle applies to the whole resolved selection. Make them a true
  group state (✓ applied to all targets, mixed for partial) by projecting
  per-target tag sets into `TargetCap` and reading them through
  `MenuTargets::all`, mirroring the bulk/anchor model Clear Quarantine uses.

## File ops, Trash and drag

- **Put Back for items Ferail did not trash.** Ferail records where it moved
  each item from, so Put Back works for its own trashing. An item trashed by
  Finder reports an unknown original location, because macOS keeps Finder's
  put-back record in a private store. Verified empirically: a file trashed by
  Finder on a scratch APFS volume carries only `com.apple.provenance`, and
  `.Trashes/<uid>` holds no sidecar. Any fix needs a supported API that does
  not exist today, so this may stay session-scoped by design.
- **Reach Restore from the keyboard and the toolbar.** `file.restore_from_trash`
  exists in the menu inventory but not in `ferail-core`'s command catalogue, so
  it has no shortcut and no palette row. Add it to the catalogue and bind it.
- Permanent delete reports failures as raw `format!("{}: {e}")` strings instead
  of the structured `FileOpError` / `file_op_failure_report` path that
  `on_move_to_trash` and `on_empty_trash` use. Align it so permanent-delete
  failures get the same per-item classified report and coping actions.
- Windows pasteboard **volume-identity parity** for CF_HDROP.
- Recents follow-ups: recently-opened **files** (needs a file-open signal; only
  folder visits are logged today), and optionally a recents store decoupled
  from the heat map, since Clear and Remove also clear that folder's heat.
- **Catch external changes with a filesystem watcher.** Deletes, adds and edits
  made outside Ferail (another file manager, a terminal `rm`, an installer)
  cannot be self-reported, so only FSEvents / `ReadDirectoryChangesW` / inotify
  closes the gap. It should refresh the *listing* (rows appearing and
  disappearing) and the folder *sizes* together, since both go stale the same
  way. Pairs with the Favorites missing-transitions item and the NodeStore
  identity item.
- **Write per-directory subtotals through the folder-size walk.**
  `recursive_totals` sums a subtree into one grand total and keeps no
  per-directory breakdown, so sizing `Downloads` walks every descendant but
  caches only the top-level rows; navigating *into* a subfolder re-walks it
  from scratch. Cache a `folder_sizes` row (size plus counts) for **every**
  directory the walk descends, keyed by each subdir's own path and mtime, so a
  later drill-down is a pure cache hit. This turns the pre-order stack sum into
  a post-order accumulation and multiplies DB writes from a handful per listing
  to thousands per top-level folder: batch them in one transaction and weigh
  the write amplification against the drill-down win first. Only worth doing if
  drill-down re-walks prove noticeable on a slow disk.

## Search

- Filter chips (kind / date / size), query operators, and glob or regex
  queries.
- Saved smart folders: see **Smart Folders / Saved Searches** above.
- Windows NTFS MFT + USN and Linux Tracker/Baloo engines behind the same
  `SearchEngine` selection. Until one exists, Settings offers a "Spotlight"
  engine on Windows where `resolve_spotlight` is macOS-only and can never
  engage: hide the option off macOS or implement a real backend.

## Preview, Get Info and viewer

- Get Info follow-ups: **inline rename inside the popup** (the name is
  read-only there; F2 still renames); **undo coverage** for attribute,
  permission and tag edits; combined **multi-item Get Info**; real Windows and
  Linux gather (unix `stat_info` yields perms and dates; NSURL and
  volume-format reads are macOS-only). For timestamps, the remaining parity is
  a native macOS creation-time writer and an explicit policy for symlinks and
  reparse points.
- **Embedded metadata editing** (distinct from filesystem dates). Start with a
  privacy-first **Remove location data** action for JPEG and TIFF, deleting
  location from EXIF, XMP and IPTC through an atomic same-directory rewrite
  that never recompresses pixels; extend to HEIC and video location atoms only
  with container-specific validation. Separately, expose common writable audio
  tags (title, artist, album, genre, year, track, disc) through `lofty`, and a
  conservative allow-list of Windows document properties only where the
  handler's `IPropertyStore` is writable. Do not add arbitrary GPS, camera,
  lens, exposure, orientation or video-metadata editing until lossless
  round-trip behaviour is proven.
- Preview-pane providers: audio **waveform** and a **video thumbnail strip**
  beyond the Quick Look poster, **archive and package summaries**, and
  per-provider cancellation tokens (stale results are dropped at apply today,
  not cancelled mid-read). Add an explicit cloud-placeholder state before reads
  that may fault remote content in.
  - **Audio waveform**: a peak view styled to the app's look (theme tokens,
    house stroke) in the preview stage. lofty reads tags but not samples, so
    decode peak buckets off-thread with `symphonia`, cache them like previews,
    and paint bars through the existing preview-cache and staleness machinery.
    Design: [MEDIA-TAGS.md](docs/features/MEDIA-TAGS.md#deferred-waveform-preview).
- **Windows preview pane: host `IPreviewHandler` live, not as a capture.** The
  pane's last resort for Office, RTF and text files is a `PrintWindow`
  screenshot taken in the broker (`preview_handler.rs`): chrome included, with
  no reliable finished-painting signal. ShellBat wrote the same code and
  disabled it for those reasons. Explorer's shape is right: a native child
  window (`WS_POPUP | WS_EX_NOACTIVATE`, parented to the app window) positioned
  over the pane's rect, `SetWindow` once, `SetRect` on every scroll and resize,
  `IObjectWithSite` plus `IPreviewHandlerFrame` provided, activated
  `CLSCTX_LOCAL_SERVER` so it runs in `prevhost.exe`, kill-and-retry on
  `RPC_E_SERVERCALL_RETRYLATER`. Needs the GPUI window's HWND, a rect feed from
  the pane's layout, and z-order and occlusion handling. PDFs are already off
  this path (`pdf_render.rs`).
- **Windows: decode HEIC/AVIF/RAW through WIC** where the bundled `image`
  crate has no codec: `IWICImagingFactory` → `CreateDecoderFromFilename` →
  frame → `32bppPBGRA` converter → `IWICBitmapScaler`, on the pool. Requires
  the `Win32_Graphics_Imaging` feature; ShellBat's `Entry.GetImage` is the
  reference shape.
- Viewer follow-ups ([VIEWER.md](docs/features/VIEWER.md)): swap the `qlmanage`
  shell-out for `QLThumbnailGenerator`; pinch to zoom; live playlist sync via
  the watcher, skipping deleted entries; a watchdog for eligible-but-unplayable
  videos stalling auto-advance; slideshow transitions once the animation-budget
  review lands; an audible-output pass for in-viewer audio on a real run;
  per-frame copy on a `CVDisplayLink` background pull if 4K60 shows cost;
  precise scrubbing seek (`seekToTime:` tolerance zero); volume control.
- Windows viewer parity: Ctrl and F11 chords, an `IShellItemImageFactory`
  fallback, and a Media Foundation video frame source feeding the shared
  `RenderImage` path. Confirm `video_mf.rs` still integrates after the viewer
  refactor and that the optional mpv plugin loads via `LoadLibraryW`.
- **Windows transparent viewer windows.** Chroma-keyed see-through windows are
  the highest-risk Windows-specific gap in the video stack: macOS uses
  `NSWindow` background appearance, Windows needs the DWM or layered-window
  equivalent. Top Phase 1 investigation for the port.
- Similar Images comparison: turn the group-scoped viewer into an A/B
  workspace. Pin one reference, compare the current candidate beside it with
  synchronized zoom and pan, then add optional opacity overlay, draggable wipe,
  and press-and-hold flicker. Keep side by side as the safe default for cropped
  or shifted images; evaluate automatic alignment separately. Every comparison
  path and decoded pixel stays window-scoped and ephemeral.

## Metadata and intelligence

- **Magic detection**: 111 format types with structured parsers for
  executables, archives, images, audio and video is solid. Expand the long tail
  and add the CLI modes below.
- **Quarantine and provenance UI**: add Gatekeeper assessment, code-signature
  identity, and in-list provenance display. Where-from is cached but shown only
  in the preview pane.
- **Ant Trail**: add prediction and prewarming, and time decay. Heat is
  cumulative today with no recency weighting.
- **Mouse predictor** ([MOUSE_PREDICTOR.md](docs/features/MOUSE_PREDICTOR.md)):
  the pointer-prediction module, Ant Trail blend, task-scheduler integration,
  debug overlay and pointer-path performance tests. Nothing is implemented.
- **APFS clone-aware disk-usage sizing**: clones share extents without
  `nlink > 1`, so they still count at full size. Detecting them needs a
  per-file clone-id query: weigh the extra syscall per file before adding it.
- Disk Usage: richer iCloud download-state handling once the existing
  path-prefix cloud glyph is not enough.
- **ThumbHash placeholders for real thumbnails.** Private Mode already
  synthesises and decodes ThumbHash blurs (`private_thumb.rs`). The real
  feature is storing a ~25-byte hash per image so a grid paints a recognisable
  blur before the thumbnail exists. Measured on a real SQLite build: 28,1 MB
  for 200.000 images as its own table (147 B/row, path dominates) against
  about 5 MB as a column on the existing `files` table, which is the shape to
  build. Privacy: a stored hash is a 6x6 impression of the image, so it belongs
  under the same reset scopes and diagnostics exclusions as thumbnails.

## Responsiveness and data architecture

- Finish the stable **NodeStore identity** model for rename, move, mount
  changes, Ant Trail, selection, watcher events and metadata cache keys.
- **NodeId intern-map lifecycle.** `NativeFs`'s identity maps are add-only for
  the life of the process, so every path the app resolves stays pinned: a
  browsing session grows them one entry per file *seen*, and one recursive tool
  run over a large tree pins the whole tree. Observed on a released 0.7.6 after
  nine hours of normal use: 1,38 GB RSS, with a sample showing
  `getattrlistbulk` and `open` still walking at idle. Both directions now share
  one `Arc<Path>` instead of a `PathBuf` each (241 → 144 bytes per path), which
  halves the slope without stopping the growth, and `NativeFs::intern_stats`
  plus the stats sampler put the footprint in an issue report. The remaining
  work is the lifecycle. Disk Usage and Flat View already use
  drop-with-surface scan-local arenas (`file_list.rs`'s flat path arena is the
  template); duplicate finding, recursive search and *ordinary listings* still
  mint global ids. Careful: scan-minted ids interleave with ids that live tabs,
  selections and history hold, because the path-keyed map returns the same id
  to both, so range- or ownership-based forgetting can misdirect a later trash
  or rename through a stale `path_for`. Either refcount ids per holding
  surface, or give each surface an arena namespace that drops with it and keep
  the global map for navigation identity only.
- **Cache freshness follow-ups** ([FRESHNESS.md](docs/features/FRESHNESS.md)):
  invalidate **both** parents' ancestor chains on a cross-directory move
  (`spawn_file_op` reloads a single `reload_path` today), and reuse the same
  model for the next recursive aggregates (item counts, clone-aware sizing)
  rather than building a parallel one.
- **Qualify the cancellation and backpressure baseline.** Every recursive lane
  cancels cooperatively or retires by generation, and the streaming channels
  are finite. What is missing is proof: hostile slow-provider, network and
  cloud tests, aggregate lane counts and cancellations exposed in diagnostics,
  and the rule that any new recursive tool adopts these primitives instead of
  adding a private queue.
- **Two bounded-work gaps from the 2026-08-29 audit.** Similar Images still
  hands its complete scan-local image and signature index to one foreground
  conversion pass at scan completion: slice that before qualifying libraries
  with hundreds of thousands of images. The viewer's next-slide
  `preview::warm` bypasses the one-active plus one-latest Shell preview queue:
  route it through a small process-owned speculative lane so rapid slideshow
  navigation cannot accumulate provider calls.
- Move remaining expensive metadata reads off synchronous UI paths (preview
  generation, large-folder bookkeeping), and audit render paths for accidental
  `PathBuf` resolution or filesystem calls.
- **Prime Directive: the one surviving UI-thread I/O.** NSPasteboard reads and
  writes in the copy, cut and paste handlers run on the main thread. They are
  fast (no per-path stat: the handlers pre-collect cached `is_dir`), and this is
  listed for strict-compliance completeness.
- Add slow-path tests or fixtures for slow folders, network volumes, cloud
  placeholders, permission failures and stale worker results.
- Streaming-enumeration tests: delayed and full-channel cancellation, stale
  generation delivery, UI-slice limits, and partial-error delivery for ordinary
  listing and search (Flat already covers the bounded million-row shape).
  Surface partial enumeration errors in the task and notification UI instead of
  logging them only.
- Evaluate Linux `statx` and `io_uring` for the shared fast walk, and have
  duplicate finding adopt the shared reader without weakening the clone and
  cloud rules.

## Settings, commands and accessibility

- **Polish plurals need their `few` form.** The bundled pack has `one` and
  `other` for all 105 plural entries, with `other` holding the genitive
  (`many`) form, so counts of 2 to 4 fall back to it and read `3 elementów`
  where Polish wants `3 elementy`. `i18n/plural.rs` already computes
  `one`/`few`/`many` correctly, so only the pack is missing forms. Ask the
  pack's author rather than search-and-replacing: a phrase whose head is itself
  genitive does not take the nominative plural the bare rule suggests. See
  [LOCALIZATION.md](docs/features/LOCALIZATION.md#known-gap-polish-plurals-need-a-few-form).
- **Localization follow-ups**: translate backend error text
  (`ferail-fs-native`, `ferail-archive`) and the failure-report bodies, once
  bug reports can carry the English alongside; locale-aware numbers, sizes and
  dates; RTL mirroring (blocked on gpui layout support); contribute Ferail's
  languages to gpui-component's own `ui.yml` so the widgets' OK and Cancel
  follow; and optionally an in-app translation provider on the same file
  format, left out of v1 to avoid API-key handling.
- Diagnostics follow-ups ([DIAGNOSTICS.md](docs/features/DIAGNOSTICS.md)):
  (a) the **in-app redaction modal**, drag-to-black-box over the screenshot
  before bundling. Reuse `image_edit`'s Redact mode rather than building a
  second canvas; it is unverifiable headlessly, so build it with visual
  testing. (b) An **OS-level window capture** so the bundle's screenshot works
  on a clean Windows build; today it uses `render_to_image`, which needs the
  gpui_windows patch and is omitted gracefully otherwise. (c) Move
  `run_checks()` off the UI thread if a slow or network config dir makes the
  one-time probe in `SettingsView::new` noticeable.
- Settings "Saved" feedback pill or toast: changes persist silently today.
- **Themes and colour customization** ([THEMES.md](docs/features/THEMES.md)):
  bundled themes plus a theme picker (Phase 1), a drop-in user themes folder
  with hot reload via `ThemeRegistry::watch_dir` (Phase 2), and a generalized
  accent-override layer (Phase 3).
- Split the Cmd+/ overlay's dual role into distinct "Commands" and "Keyboard
  Shortcuts" modes if it proves confusing.
- User-overridable key bindings: installed from the catalogue today, with no
  UI.
- Ensure every icon-only button has a tooltip with its shortcut, every
  truncated string has a tooltip, and menu shortcuts render via `Kbd`.
- Keyboard accessibility: tab order, focus rings, arrow navigation, Escape
  behaviour, and Settings-from-anywhere.
- Accessibility announcements for file operations and long-running tasks.
- IME and composition support for text input and rename flows.

## CLI and automation

- Extend `ferail magic` with `--json`, `--csv`, `--mismatch-only` and
  `--limit`.
- Extend `ferail du` with structured output and filters, reaching parity with
  the Disk Usage window's largest-file model.
- Add non-GUI commands for automation: metadata reset, duplicate finding, cache
  inspection, command-catalogue listing.
- Accept a bare directory argument so `Exec=ferail %U` and
  `MimeType=inode/directory;` work in the desktop entry. A bare path exits as
  an unknown subcommand today.
- Add a plugin or scripting story only after the command and permission model
  is explicit.

## Packaging and polish

- Rework the app icon to macOS conventions and generate the iconset. The bundle
  script already builds `.icns` from a PNG source; the icon *art* is the gap.
- **Bundle an LGPL libmpv inside the `.app`** so mpv playback works out of the
  box. Every release build compiles `--features mpv`, and the provider dlopens
  a user-installed libmpv, falling back to the native player without one. What
  is missing is the library itself. Homebrew's libmpv chain is **GPL-3.0**
  because its ffmpeg is built with x264 and x265, so it cannot ship inside an
  MIT/Apache DMG without making the whole binary GPL. The viable path: build
  ffmpeg `--disable-gpl --disable-nonfree` (decoders and demuxers only; the
  *encoders* are GPL, while the H.264/HEVC/AV1/VP9 **decoders** are LGPL and
  decoding is all the viewer needs) as static libs linked into an LGPL libmpv,
  yielding one self-contained `libmpv.dylib` to place in
  `Contents/Frameworks/` and sign, instead of relocating Homebrew's ~47-dylib
  closure with `install_name_tool`. Sign it *before* the outer bundle
  (`bundle-mac.sh` signs only the app today, so notarization would fail), probe
  the bundle ahead of Homebrew in `default_mpv_path()`, and add the LGPL
  notices plus a corresponding-source offer: pinning the ffmpeg and mpv sources
  and build script in-repo is an ongoing obligation on every rebuild. **Verify
  early:** the viewer's live vf chain must survive an LGPL ffmpeg, whose `eq`
  filter is GPL-gated, so the grade path may need `colorlevels` or `hue`
  instead. Test the exact chain with
  `cargo run -p ferail-video-mpv --example probe`. Windows: the same recipe
  yields `libmpv-2.dll` beside the exe in the ZIP. Linux: nothing to bundle.
- **Obtain an Authenticode certificate.** `scripts/package-win.ps1` wires
  signing end to end but a stock run is unsigned, and SmartScreen warns on
  every download of an unsigned binary. Reputation accrues *to the
  certificate*, so signing from the first public release matters more than
  signing later.
- **A macOS release job in CI.** It needs the signing certificate (.p12) and a
  notary API key as encrypted repo secrets. Until then a Mac release is one
  local `scripts/package-mac.sh` run plus `gh release upload`. Linux .debs
  already release from CI on `v*` tags; the Windows ZIP is still a local
  `scripts/package-win.ps1` run.
- **`cargo-deny` for licence and advisory drift.** There is no `deny.toml`, so
  a future `gpui` rev bump that changes the transitive licence surface would
  not be caught mechanically. The GPL edge is severed today through
  [`vendor/ztracing`](vendor/ztracing/README.md); the lesson that made it
  necessary is worth keeping: **do not audit the licence surface from the
  lockfile alone**, resolve the graph. A lockfile generated with the AROS
  `[patch]` active dropped `ztracing` entirely and made THIRD-PARTY-NOTICES.md
  record the edge as fixed upstream when it never was.
- **One published zed fork** referenced by `git =` URL, carrying both the GPL
  severance and the `gpui_windows::render_to_image` patch. It would retire
  `vendor/sum-tree` and the local screenshot path override. Upstream fix
  tracked at <https://github.com/zed-industries/zed/issues/55470>, acknowledged
  but stuck in legal: do **not** assume it lands on a timeline.
- Visual polish still missing from the GPUI shell: vibrancy and materials,
  titlebar hit testing, sharper row density, empty and error illustrations, and
  an animation-budget review.
- Rebuild deterministic screenshot fixtures for the shell, settings pages, disk
  usage, task popover and panel, errors, empty folders and narrow layouts.
- Screenshot CLI deferred flags: either implement deterministic `--splitter`,
  `--scroll`, `--ui-scale` and `--mac-chrome` behaviour, or remove them and
  warn clearly where the harness cannot honour them.
- Add debug overlays for frame time, task queue, cached and missing metadata,
  layout bounds, hit regions, and injected slow I/O.

## Cross-platform

- **Filename display-convention parity.** macOS landed: an on-disk `:` shows as
  `/` and a typed `/` stores `:`, matching Finder, through
  `ferail_fs_native::paths::{display_leaf,on_disk_leaf}`. Remaining on the same
  seam: macOS HFS NFD normalization is cosmetic and renders fine today, so
  revisit only if a normalization-sensitive comparison surfaces; and
  optionally an informational note in Get Info when a name contains a
  `/`-shown-as-`:`, so the on-disk reality is discoverable.
- **Windows Shell namespace breadth** (WIN-013). This PC, the Recycle Bin and
  connected portable devices enumerate through the provider. Still missing: a
  **Desktop folder** Location, OneDrive and other provider roots, and treating
  removable or disconnected provider identity as ephemeral with a clear
  reconnect state.
- **A Recycle Bin sidebar row on Windows.** macOS has a Trash location; Windows
  has none. It is a shell *virtual* folder (`FOLDERID_RecycleBinFolder`, CLSID
  `{645FF040-…}`), not a filesystem path, so it needs shell-namespace browsing
  rather than a `well_known_locations_for` entry: navigating to the raw
  `C:\$Recycle.Bin\<SID>` would show the `$R*` and `$I*` internals, which is
  worse than nothing.
- **Port window docking to Windows** ([DOCK.md](docs/features/DOCK.md)).
  `dock.rs` computes frames in macOS global screen space (origin bottom-left,
  y-up), so a Windows arm has to map that onto Win32's top-left, y-down monitor
  rects: `MonitorFromWindow` and `GetMonitorInfoW` for
  `screen_visible_frame_for_window`, `GetCursorPos` for
  `current_mouse_location`, `SetWindowPos` for the frame. A docked drawer with
  edge-slam reveal and auto-hide cannot be verified headlessly, so it needs
  interactive testing on a real desktop. `Shell::window_ns_view` must stay
  AppKit-only until then: returning `Some(hwnd)` would let `set_dock` run
  against no-op primitives and show a docked state for a window that never
  moved. The toolbar control is already `cfg!(target_os = "macos")`-gated, so
  there is no dead UI meanwhile.
- **A ~50 px empty band under the Windows title bar** that macOS does not have
  (compare `screenshots/win-baseline.png` against `docs/images/tour-shell.png`).
  Probably `TitleBar::title_bar_options()` reserving the macOS traffic-light
  strip on top of the Windows caption area, exactly as windows-port.md §5
  predicted. Cosmetic, but it is the first thing a Windows user sees.
- **Windows power follow-ups** ([POWER.md](docs/features/POWER.md)): display
  on/off events (`PBT_POWERSETTINGCHANGE` plus
  `RegisterPowerSettingNotification` for `GUID_CONSOLE_DISPLAY_STATE`), and
  switching the idle-sleep guard from per-thread `SetThreadExecutionState` to
  the process-wide Power Request API if a transfer ever asserts from a
  thread-pool worker.
- **Linux shell stubs** ([linux-port.md](docs/features/linux-port.md)): the
  file-URL clipboard (`text/uri-list`), and the dark, volume and power
  observers (D-Bus, udisks2, logind). These need a real desktop with mounts and
  session events to verify meaningfully. Also open: `pkexec` re-exec for
  `run_elevated_self` and a `/proc/*/fd` scan for `processes_using`, the Linux
  half of the resilient file-op coping that ships on Windows; a taskbar-identity
  check on a real desktop session; and video and PDF thumbnails through totem,
  evince or Tumbler beyond the image support that ships.
- **Linux headless screenshots**: implement `render_to_image` in `gpui_wgpu`
  (offscreen render target plus `copy_texture_to_buffer` readback, BGRA/RGBA)
  and wire it through both `gpui_linux` window backends, Wayland and X11,
  mirroring the `gpui_windows` D3D11 patch. This unlocks `--screenshot` on
  Linux so the GUI can be verified the way macOS and Windows are.

## Cleanup

- Keep `cargo clippy --workspace --all-targets` at zero warnings. `multi_table/`
  carries a module-level `#![allow]` for style lints by policy (pinned
  gpui-component fork); do not extend those allows elsewhere.
- Remove stale references to old specs or deleted migration ledgers as code and
  docs settle.
