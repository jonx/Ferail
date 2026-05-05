# Feraille — Open Risks & Backlog

Free-form list of things to look into later. Not a roadmap (see
[docs/ROADMAP.md](docs/ROADMAP.md)) and not a feature ledger (see
[docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)) — both of those are
structured. This file is for the unstructured "remember to revisit X"
notes that would otherwise crowd CLAUDE.md or rot in code comments.

When an item ships, delete it (the commit + NOTES.md entry is the record).

## Architectural risks

- **Eager filesystem enumeration.** Still synchronous in the `enumerate`
  path; the biggest known violation of the performance model for large
  or slow folders. Streaming + cancellation is a Stage-2 design pass.
- **TextInput IME / composition.** No full input-method-editor support
  yet. Will matter for non-Latin keyboards and Asian-language naming.

## Feature gaps

- **Delete-to-Trash.** Currently a fallback `~/.Trash` move. Replace with
  `NSWorkspace`'s real trash semantics (undo support, audible feedback).
- **Preview pane.** Metadata-only today. Real previews (text/image/PDF/
  Quick Look) need async cancellable workers.
- **Inline rename.** Modal dialog exists; in-row rename remains.
- **Drop targets.** Drag-out is done; drop-into-folder/tab/tree isn't.
- **Volume display name.** `crates/feraille-app/src/main.rs:1053` —
  fetch real macOS volume label rather than parsing the path.

## Diagnostics / tooling

- **Toast / ErrorState surface.** Multiple `log_error!` sites today should
  also surface to the user (rename failure, create_dir failure, trash
  failure). Currently visible only in stderr.

## Branding / packaging

- **Rework the app icon to macOS conventions.** The current icon
  ([crates/feraille-app/resources/feraille.png](crates/feraille-app/resources/feraille.png))
  is the Windows-shaped folder reused from Ferail. Macs expect the
  standard squircle silhouette — a square with rounded borders, full-bleed
  background, content centred — at 1024×1024 with the canonical macOS
  rounding radius. Generate `.iconset` folder and `iconutil` it into
  `feraille.icns` for eventual `.app` bundling.
- **Ship as a real .app bundle.** Without `Info.plist` + `Contents/
  Resources/AppIcon.icns`, the dock/About icon has to be set at runtime
  (currently via `feraille_shell_mac::set_app_icon_from_png_bytes`) and
  the binary identifies as a generic exec to launch services. cargo-bundle
  or a hand-rolled bundle script is the fix.

## Notes from the porting effort

- The "Source" column in [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)
  references docs in the original Windows project at
  `/Users/jkn/Source/Ferail/`. When something here looks regressed
  compared to Ferail, read the analogous code/doc there before
  redesigning. (See CLAUDE.md → Cross-Reference: Ferail.)
