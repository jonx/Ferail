# Ferail Status

← [Documentation map](README.md) · [Project README](../README.md) ·
[Open work](../TODO.md) · [Changelog](../CHANGELOG.md)

Where the project actually is. This is the only file that states status: the
README shows the four platform rows and links here, feature notes describe
the finished design and link here, and [TODO.md](../TODO.md) lists what is
not done. If you are about to write "currently" anywhere else, write it here
instead.

## Where we are

| | |
| --- | --- |
| Release | **0.7.7**, published on macOS, Windows and Linux |
| Daily driver | macOS (Apple silicon), signed and notarized |
| Toolchain | Rust `1.97.1`; MSRV `1.97` |
| UI stack | gpui-component `e8f54eb`, Zed/GPUI `f66ed39` |
| Open work | [TODO.md](../TODO.md) |
| Active campaign | [Windows reliability and compatibility](features/WINDOWS_COMPATIBILITY_PLAN.md) |

## Platforms

| Platform | State | What that means |
| --- | --- | --- |
| **macOS** | Primary | Feature-complete for everyday use. The DMG is Developer ID signed and notarized, so it opens without a warning. |
| **Windows** | Active port | Broad native parity: clipboard, Recycle Bin with Restore, thumbnails, Open With, Media Foundation video, UAC elevation, Restart Manager lock diagnostics, and native context-menu verbs on explicit demand. Builds, runs and screenshots on real hardware. Unsigned, so SmartScreen warns on first launch. Indexed search and window docking are planned. ([notes](features/windows-port.md)) |
| **Linux** | Early port | Builds and runs; volumes, Trash and Open With are real; clipboard, thumbnails and video are stubbed. Ubuntu 22.04-compatible amd64 and arm64 `.deb` packages ship. ([notes](features/linux-port.md)) |
| **AROS** | Research port | Boots and runs as a browsable, themed file manager through a from-scratch GPUI platform backend. Not at parity: several features are gated pending native shell integration. Distributed inside the **Macaros** releases as the `C:Ferail` command of the AROS image, not as a Ferail download. ([notes](features/aros-port.md)) |

## Windows campaign

The ledger is [WINDOWS_COMPATIBILITY_PLAN.md](features/WINDOWS_COMPATIBILITY_PLAN.md);
the acceptance matrix is
[WINDOWS_RELIABILITY_TEST_PLAN.md](testing/WINDOWS_RELIABILITY_TEST_PLAN.md);
the operational resume point is
[WINDOWS_HANDOVER.md](testing/WINDOWS_HANDOVER.md).

Open at 0.7.7: independent administrator-equipped qualification of the Fast
NTFS helper across the VHDX and large-volume matrix, the adversarial and
hardware-dependent cases (MTP disconnect, hostile providers, long soaks,
multi-DPI comparison), and real-Windows qualification of the WSL locations
(WIN-017). No Windows-only exit gate is marked complete from macOS or
cross-compilation alone.

## Features

Audited against the sources on 2026-06-20 and corrected since as features
shipped. "Shipped with follow-ups" means the user-facing feature exists and
the rest is tracked in [TODO.md](../TODO.md).

| Feature note | Status | Public-ready summary |
| --- | --- | --- |
| [ANT_TRAIL.md](features/ANT_TRAIL.md) | Shipped with follow-ups | Folder visit counts, sidebar heat, recents hydration, and DB persistence ship; prediction and decay remain open. |
| [ARCHIVES.md](features/ARCHIVES.md) | Shipped with follow-ups | Browse `.zip`/`.7z`/`.tar.*`/`.lha` in place, extract a selection, compress to ZIP/7z/TAR, and edit a plain ZIP as a reviewable transaction; the embedded workbench view remains open. |
| [aros-building.md](features/aros-building.md) | Reference | The AROS cross-build: toolchain, hosted target, and how to run the result. |
| [aros-port.md](features/aros-port.md) | Research port | Ferail boots and runs as a browsable, themed file manager on AROS through a from-scratch GPUI platform backend; native shell integration gates the rest. |
| [BULK_RENAME.md](features/BULK_RENAME.md) | Shipped with follow-ups | Pattern-rule bulk rename modal: literal/regex find-replace, case transforms, {name}/{ext}/{n}/{date} template, live preview, chain-aware apply, and batch undo ship; dimensions token, presets, and a keybinding remain open. |
| [CHECKSUMS.md](features/CHECKSUMS.md) | Shipped with follow-ups | Streaming single-file SHA-256 comparison plus safe multi-file SFV/checksum verification and atomic SFV/SHA256SUMS generation ship; compact million-entry reports remain open. |
| [CONTEXT_MENU.md](features/CONTEXT_MENU.md) | Shipped with follow-ups | Data-driven menu plans per surface, Open With, tags, Quick Look, a Trash-specific menu, and a Settings editor that hides, reorders and adds separators; a Services/Quick Actions submenu, a Share submenu and async Open With prewarm remain open. |
| [DIAGNOSTICS.md](features/DIAGNOSTICS.md) | Shipped | Privacy-reviewed diagnostic bundle: config, storage and dependency health, with user file and folder names excluded by default. |
| [DISK_USAGE.md](features/DISK_USAGE.md) | Shipped with follow-ups | Disk Usage, native batched APFS reads, bounded directory parallelism, scan-local memory, treemap/top list and partial-coverage reporting ship; APFS clone-aware sizing and the Windows MFT backend remain open. |
| [DOCK.md](features/DOCK.md) | Shipped with follow-ups (macOS) | Dock the whole window to the left or right screen edge as an auto-hiding, always-on-top drawer revealed by an edge-slam, with a thin grab handle; core slide/float/all-Spaces ship. Persistence/auto-restore, borderless drawer chrome, and multi-display polish remain open. |
| [DUPLICATES.md](features/DUPLICATES.md) | Shipped with follow-ups | Size/partial/full-hash duplicate finder, clone/hard-link awareness, card panel, virtualization, and cleanup actions ship; faster enumeration and more benchmarks remain open. |
| [FAVORITES.md](features/FAVORITES.md) | Shipped with follow-ups | Favorites persistence, sidebar, drag/drop, locate, rename, remove, keyboard actions, and cross-platform modal flow ship; tag favorites and file-watch missing transitions remain open. |
| [ferail-selection-dnd-spec.md](features/ferail-selection-dnd-spec.md) | Partial | Selection, row drag, external file drops, and many acceptance points ship; edge auto-scroll and favorite-row drops remain open. |
| [ferail-windows-instances-tabs-spec.md](features/ferail-windows-instances-tabs-spec.md) | Partial | Process/window state split, tabs, closed-tab undo, and shared caches are in place; full multi-window/tear-off completion remains open. |
| [FILE_OPS.md](features/FILE_OPS.md) | Shipped with follow-ups | Copy, cut, paste, move, trash, Put Back, collision policy, progress, cancellation, undo, and platform integration ship; mutation toast/undo gaps remain open. |
| [FLAT_VIEW.md](features/FLAT_VIEW.md) | Shipped with follow-ups | Uncapped, cancellable recursive file snapshot on the existing virtualized table, with scan-local identity, compact path arena, relative Path column, progress, Refresh, and in-memory filtering; compact rows, async indexes and page-backed scale remain. |
| [FREEZE_DIAGNOSTICS.md](features/FREEZE_DIAGNOSTICS.md) | Shipped | Freeze and stuck-shutdown reporting: the watchdog names what outlived a quit and writes a report instead of leaving a headless process. |
| [FRESHNESS.md](features/FRESHNESS.md) | Shipped with follow-ups | Keeps subtree-derived caches (folder sizes, Get Info "Calculate") honest via mtime + TTL validity, exact ancestor invalidation on in-app mutations, and a forced refresh when the window returns to the foreground; multi-parent moves and a live-watch upgrade path remain open. |
| [ICONS.md](features/ICONS.md) | Reference | Complete icon inventory: source (NSWorkspace / local Lucide bundle / upstream), attribution, command→icon map, and the rules for adding new icons. Flags missing/weak/reused glyphs. |
| [IMAGE_EDITOR.md](features/IMAGE_EDITOR.md) | Shipped | Built-in image redaction/annotation editor: rectangle + brush in Redact (opaque black) and Annotate (coloured) modes, undo, save-copy beside the original or confirmed in-place overwrite, off-thread compositing with a bounded preview buffer. |
| [LAZY_METADATA.md](features/LAZY_METADATA.md) | Shipped with follow-ups | Shared NodeStore, path guard, cached row metadata, background prefetch, metadata DB, and process-owned caches ship; rename/move identity completion remains open. |
| [linux-port.md](features/linux-port.md) | Partial port | The Linux shell crate compiles behind stubs; real clipboard/trash/open-with/volume/power/preview integrations remain open. |
| [LOCALIZATION.md](features/LOCALIZATION.md) | Shipped with follow-ups | English-as-key catalog, bundled French, German and Polish packs, live switching, and an export/translate/import workflow; Polish `few` plurals, backend error text, RTL and locale number formats remain open. |
| [mac_port.md](features/mac_port.md) | Shipped with follow-ups | macOS is the primary implementation path; remaining items are mostly packaging, polish, and verification. |
| [MAGIC_DESCRIPTION.md](features/MAGIC_DESCRIPTION.md) | Shipped | Format/Description columns, mismatch cues, quarantine badges, and structured descriptions ship. |
| [MAGIC_SNIFFING.md](features/MAGIC_SNIFFING.md) | Shipped with follow-ups | Structured magic detector, DB cache, async prefetch, and quarantine fusion ship; long-tail formats, cloud skip rules, and debug views remain open. |
| [MEDIA-TAGS.md](features/MEDIA-TAGS.md) | Shipped with follow-ups | lofty-backed audio tags/properties/cover: Get Info Media section, cross-platform cover art in preview/grid, and the rich audio Description line ship; in-viewer audio playback and a waveform preview remain open. |
| [METADATA_DB.md](features/METADATA_DB.md) | Shipped | SQLite metadata DB, schema versioning, favorites, file metadata, folder usage, and cache reset scopes ship. |
| [MOUSE_PREDICTOR.md](features/MOUSE_PREDICTOR.md) | Future | Design note only; no pointer prediction/prewarm scheduler is implemented yet. |
| [NON_NEGOTIABLE.md](features/NON_NEGOTIABLE.md) | Reference | Reusable method for making a project rule unbreakable for coding agents: canonical doctrine + named hazards + sanctioned path + lint wall + runtime tripwires + verification ritual + violation ledger. Uses the Prime Directive hardening as the worked example. |
| [OPEN_WITH.md](features/OPEN_WITH.md) | Partial + study | System handler enumeration (LaunchServices / `SHAssocEnumHandlers` / freedesktop MIME) ships behind a warm off-thread cache; user-defined custom tools, an "Other…" app chooser, "Always Open With", and multi-selection support are designed but unbuilt. |
| [POWER.md](features/POWER.md) | Shipped with follow-ups | macOS sleep/wake handling, transfer idle-sleep prevention, and Windows/Linux shell surfaces exist; Windows display events and Power Request API are still deferred. |
| [PREVIEW.md](features/PREVIEW.md) | Shipped with follow-ups | Info pane, Quick Look thumbnails, inline text/Markdown/code preview, caches, scroll chaining, and viewer handoff ship; audio/archive providers and true cancellation remain open. |
| [PRIVATE_MODE.md](features/PRIVATE_MODE.md) | Shipped with follow-ups | Session-only, process-global capture lock: the prepared UI remains intact while semantic names/paths/metadata are projected, content pixels are hidden, interactions are frozen, and screenshots enable it by default. |
| [SCREENSHOTS.md](features/SCREENSHOTS.md) | Shipped with follow-ups | Headless screenshot CLI and simulated UI states ship; deterministic fixture coverage and a few deferred flags remain open. |
| [SEARCH.md](features/SEARCH.md) | Shipped with follow-ups | In-folder filter, recursive walker, Spotlight/global search, streaming results, cancellation, and task integration ship; filters/operators and Linux/Windows indexers remain open. |
| [SIDEBAR_LAYOUT.md](features/SIDEBAR_LAYOUT.md) | Implemented; cross-platform visual qualification pending | Persistent section disclosure/order and normal/compact/icon density, with stable IDs for conditional platform sections. |
| [SIDECARS.md](features/SIDECARS.md) | Shipped with follow-ups | Content-first NFO/DIZ recognition, CP437/ANSI/Kodi preview, safe SFV/checksum verification and generation, and a memory-only folder sidecar card ship; compact million-entry parsing/results and richer report actions remain. |
| [STATUS_PROGRESS.md](features/STATUS_PROGRESS.md) | Shipped with follow-ups | Task registry, status strip, task popover, cancellation flags, recent history, and screenshot simulation ship; completion notifications and accessibility announcements remain open. |
| [STREAMING_ENUMERATION.md](features/STREAMING_ENUMERATION.md) | Shipped with follow-ups | Directory enumeration is worker-driven, batched, cancellable, and notification-based; slow-path/stale-result tests and partial-error UI remain open. |
| [SYSTEM_STATS.md](features/SYSTEM_STATS.md) | Shipped | CPU and redraw figures in the status bar, normalized the way the platform's own task manager reports them. |
| [TARGET_PANEL.md](features/TARGET_PANEL.md) | Future | Design note only: a pinned, frozen second listing ("Pick as Target") that acts as source or destination for file operations, and the batched-transfer queue it enables. Nothing implemented. |
| [TESTING_OVERLAYS.md](features/TESTING_OVERLAYS.md) | Future | Debug-overlay design remains unimplemented beyond screenshot simulation hooks. |
| [TEXT_EDITOR.md](features/TEXT_EDITOR.md) | Shipped | One window per file over gpui-component's Editor: find and replace, reload from disk, an encoding/line-ending/cursor strip, wrap and line-number toggles, UTF-8 with CRLF/BOM round-trip, atomic-sibling saves, size/binary refusal states. |
| [THEMES.md](features/THEMES.md) | Planned (Phase 0 shipped) | User-facing theming plan: the selection-accent override + color picker ship; bundled themes, a theme picker, a drop-in user themes folder, and a generalized override layer are scoped but unbuilt. |
| [TOOL_RESULTS.md](features/TOOL_RESULTS.md) | Shipped with follow-ups | Shared tab-local result surface for Search, Duplicate Finder, and docked Disk Usage ships; pop-out/state migration remains open. |
| [UPDATES.md](features/UPDATES.md) | Shipped with follow-ups | Opt-in update check, disabled by default, plus the Windows Install-and-Restart path; automatic background checks remain out of scope. |
| [VIDEO-MPV.md](features/VIDEO-MPV.md) | Shipped, library not bundled | The libmpv backend, live enhance filters, chroma key with an eyedropper, and transparent always-on-top viewer windows the window server composites on the GPU. Every release build (DMG, Windows ZIP, `.deb`) compiles the provider in, and it dlopens a user-installed libmpv at runtime, falling back to the platform-native player without one. **libmpv itself ships in none of them**: the `.deb` only recommends `libmpv2`, and bundling an LGPL build is open work. A zero-copy GL to IOSurface frame path is also open. |
| [VIEWER.md](features/VIEWER.md) | Shipped | Viewer window, playlist navigation, images, Quick Look fallback, mpv-backed video with live grading and chroma key, transparent stacking, slideshow, sticky zoom, and controls. |
| [windows-port.md](features/windows-port.md) | Shipped port | Clipboard, Recycle Bin with Restore and its original-location column, thumbnails, Open With, Media Foundation video, native context-menu verbs on demand, Shell namespace locations (This PC, Recycle Bin, portable devices), `.lnk` handling, native Properties, WSL Linux locations, UAC elevation, Restart Manager lock diagnostics, crash dumps and packaging all ship. Desktop and OneDrive namespace roots and MFT-indexed search are not built. |
| [WINDOWS_COMPATIBILITY_PLAN.md](features/WINDOWS_COMPATIBILITY_PLAN.md) | Shipped, qualification open | The ledger for the 0.6.5 tester report. Every item WIN-001 to WIN-017 is implemented: crash containment, viewport-bounded background work, Explorer and Shell compatibility, virtual locations, native menu on demand, WSL locations, and clean-machine packaging. What is open is Windows sign-off, which by the ledger's own rule is never claimed from macOS. |
| [WINDOWS_FAST_NTFS.md](features/WINDOWS_FAST_NTFS.md) | Shipped | The opt-in elevated Disk Usage backend: raw MFT parsing, compact subtree reconstruction, a private named-pipe helper protocol, automatic fallback and diagnostics. The helper binary is built, attested and packaged into the Windows artifacts. Independent large-volume and VHDX qualification is open. |
| [WINDOWS_HEADLESS_SCREENSHOTS.md](features/WINDOWS_HEADLESS_SCREENSHOTS.md) | Reference | How the screenshot CLI runs without a desktop session on Windows. |
