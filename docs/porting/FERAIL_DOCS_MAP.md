# Ferail Markdown Port Map

Every Markdown file in `../Ferail` was reviewed as source material. This file
records where each idea now lives in Feraille and how it changes for macOS.

## Root And Design Docs

| Ferail file | Feraille home | Port decision |
|---|---|---|
| `CLAUDE.md` | `CLAUDE.md`, `docs/UI_NONBLOCKING.md`, `docs/ARCHITECTURE.md` | Kept paint purity, NodeStore direction, and DPI lessons. Replaced Win32/D2D instructions with Mac renderer and shell boundaries. |
| `docs/design/UI_ARCHITECTURE.md` | `docs/ARCHITECTURE.md`, `specs/controls/00-overview.md` | Kept pure layout, atomic application, virtual tree, semantic visibility. Removed HWND-specific mechanics. |
| `docs/design/SPECS.md` | `docs/FEATURE_LEDGER.md`, `specs/ux`, `docs/ROADMAP.md` | Converted broad Windows master spec into Mac feature ledger and roadmap. |
| `docs/design/BEST_PRACTICES.md` | `CLAUDE.md`, `docs/UI_NONBLOCKING.md` | Collapsed into active engineering rules. |
| `docs/design/PROMPT.md` | `docs/ROADMAP.md` | Replaced prompt-style prose with actionable roadmap. |
| `docs/design/SUPER-PROMPT.md` | `docs/ROADMAP.md`, `docs/FEATURE_LEDGER.md` | Replaced with current Mac project docs; no prompt text retained. |

## Done Feature Notes

| Ferail file | Feraille home | Port decision |
|---|---|---|
| `docs/done/ANT_TRAIL.md` | `docs/features/ANT_TRAIL.md` | Kept heat model. Feraille has in-memory heat now; persistence/prediction Todo. |
| `docs/done/DISK_USAGE.md` | `docs/features/DISK_USAGE.md` | Kept scanner/treemap intent. Mac version must be APFS/mount-aware and async. |
| `docs/done/LAZY_TEXT_DISPLAY.md` | `docs/features/LAZY_METADATA.md`, `docs/ARCHITECTURE.md` | Kept NodeId/NodeStore/cache contract. Feraille currently has partial `FileEntry` cached display data. |
| `docs/done/MAGIC_SNIFFING.md` | `docs/features/MAGIC_SNIFFING.md` | Kept async/cached rule. Feraille has small async worker slice; full DB/persistent cache Todo. |
| `docs/done/STATUSBARPROGRESSCONTROL.md` | `docs/features/STATUS_PROGRESS.md` | Converted Win32 control spec into renderer/control-native Mac status task model. |
| `docs/done/WARMUP_RIGHT_CLICK.md` | `docs/features/CONTEXT_MENU.md` | Replaced Win32 `IContextMenu` details with NSMenu/Finder-action model. Kept prewarm principle. |
| `docs/done/nodeid_list_todo.md` | `docs/features/LAZY_METADATA.md` | Folded into NodeId-native target model. |
| `docs/done/stackoverflow-cursor-issue.md` | `docs/features/CONTEXT_MENU.md` | Windows wait-cursor fix is not applicable. Kept the general lesson: shell/menu work must never make the UI look wedged. |
| `docs/done/README.md` | `docs/FEATURE_LEDGER.md` | Replaced "done folder" convention with explicit status labels. |

## Notes

| Ferail file | Feraille home | Port decision |
|---|---|---|
| `docs/notes/todo.md` | `docs/FEATURE_LEDGER.md`, `docs/ROADMAP.md` | Converted phases into Mac-aware done/partial/todo ledger. |
| `docs/notes/MOUSE_PREDICTOR.md` | `docs/features/MOUSE_PREDICTOR.md` | Kept future prewarm concept. Must stay pure in pointer path. |
| `docs/notes/README.md` | `docs/FEATURE_LEDGER.md` | Empty source file; no content to port. |

## Testing

| Ferail file | Feraille home | Port decision |
|---|---|---|
| `docs/TESTING_OVERLAYS.md` | `docs/TESTING_OVERLAYS.md` | Reconstructed for Feraille screenshot CLI and Mac worker/debug needs. |

## What Was Intentionally Dropped

- Win32 API names as implementation requirements.
- Direct2D/GDI-specific rendering instructions.
- WSL-specific behavior.
- Windows shell extension details as Mac requirements.
- Conversational "generate next" sections.
- Duplicate copies of the same mouse-prediction note.

## What Was Preserved

- UI responsiveness as product identity.
- Paint purity.
- Stable identity and cached display metadata.
- Async/cancellable I/O.
- Ant Trail as a signature feature.
- Magic sniffing, previews, treemap, duplicate finder, and metadata DB as
  long-term parity goals.
