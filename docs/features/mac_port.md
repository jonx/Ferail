# Feraille — Mac-side verification after the Windows port

Companion to [windows-port.md](windows-port.md). That doc walks through getting Feraille building and running on Windows; this one is the reverse direction — what a Mac dev needs to know when picking the codebase back up after a Windows-development sprint has landed.

The Windows port adds platform-specific code paths and refactors a few shared modules. macOS arms were kept intact in spirit, but several call sites changed shape and a handful of behaviors shifted slightly. This doc enumerates the changes so you know where to look if something on Mac feels off.

---

## 1. Architecture changes (still macOS-friendly)

Three new modules under `feraille-fs-native`:

- [`paths`](../../crates/feraille-fs-native/src/paths.rs) — `home_dir()` + `well_known_locations()`. macOS arm uses `$HOME` and the standard subfolder names (`Applications`, `Desktop`, `Documents`, `Downloads`, `.Trash`, `Movies`, `Music`, `Pictures`) — byte-identical to the old `LOCATIONS` const that used to live in `feraille-gpui::shell`.
- [`volumes`](../../crates/feraille-fs-native/src/volumes.rs) — `list_volumes()` extracted from `lib.rs`. The macOS arm reads `/Volumes` and resolves NSURL keys via the existing `volume_info_for_path` (which stayed at the top of `lib.rs`). Behavior is unchanged on Mac.
- Windows-arm additions in [`icons`](../../crates/feraille-fs-native/src/icons.rs) and [`xattr_info`](../../crates/feraille-fs-native/src/xattr_info.rs). macOS arms untouched.

New cfg-gated UI primitive:

- `Shell::menu_bar: Option<Entity<gpui_component::menu::AppMenuBar>>`. On Mac it's `None`; the in-window menu strip is never instantiated. The mac path keeps the NSApp menu bar driven by `cx.set_menus()` exactly as before.

Cross-platform label constants:

- [`feraille_core::commands::REVEAL_LABEL`](../../crates/feraille-core/src/commands.rs) and `TRASH_LABEL` — cfg-gated. On macOS they resolve to `"Reveal in Finder"` and `"Move to Trash"` (unchanged literal strings). On Windows they become `"Reveal in Explorer"` / `"Move to Recycle Bin"`. Every menu / tooltip site that previously hardcoded the macOS strings now routes through these constants.

---

## 2. Behavioral changes on Mac (not just compile concerns)

These deltas are deliberate but reach the macOS path. Verify each in interactive testing.

### Reveal in Finder is now per-path, not batch

[shell/file_ops.rs:50-52](../../crates/feraille-gpui/src/shell/file_ops.rs#L50-L52)

Old: `Command::new("/usr/bin/open").arg("-R").args(&paths).spawn()` — one process invocation, Finder opens one window with all selections highlighted (when they share a parent dir).

New: `for path in &paths { crate::platform_shell::reveal_in_finder(path); }` — N invocations of `open -R <path>`. Multi-selection from different folders now opens N Finder windows; same-folder multi-selection still resolves to one Finder window but the selection state may differ.

**Fix if regressed**: add a Mac-specific batch path back in `on_reveal_in_finder` under `#[cfg(target_os = "macos")]`.

### `install_app_menus` now mirrors into `gpui_component::global_state::GlobalState`

[main.rs:521-526](../../crates/feraille-gpui/src/main.rs#L521-L526)

After `cx.set_menus([...])`, we call `cx.get_menus()` and push the owned menus into gpui-component's `GlobalState` so the Windows `AppMenuBar` can read them. On macOS this extra call is read by nothing (the NSApp mainMenu drives the visible bar) — but it does run on Mac as an extra global-state write at startup. If you see unexpected memory or startup-cost, this is the only new runtime call on Mac startup.

### `feraille thumb` CLI subcommand

[main.rs](../../crates/feraille-gpui/src/main.rs) gained a new `thumb` subcommand. On macOS it routes through `platform_shell::fetch_quick_look_thumbnail` which still shells out to `qlmanage -t`. Same pipeline as the preview pane uses, just exposed for scripting / debugging.

```sh
feraille thumb /Applications/Safari.app --out safari.png --size 512
feraille thumb /Users/jkn/Downloads/some.pdf
```

Not exercised on Mac before — worth one sanity check that it produces a real PNG. If `qlmanage` is missing or slow it'll surface here.

---

## 3. Compile-side risks to verify

In rough order of likelihood-of-breakage:

### a. `feraille-fs-native::volumes::list_volumes()` macOS arm

The function moved from `lib.rs` into a new `volumes` module. Its macOS implementation calls `crate::volume_info_for_path` — the NSURL-based function that stayed at the top of `lib.rs`. Compile should be clean (`pub use volumes::list_volumes;` is exported from `lib.rs` so callers don't need to change imports) but worth a fresh build to confirm.

### b. `feraille-fs-native::paths::well_known_locations()`

This replaces the old `LOCATIONS` const previously in `feraille-gpui::shell`. The macOS arm produces a `Vec<WellKnownLocation>` with identical labels / sub-paths / icons. If the sidebar's Locations section renders wrong on Mac, this is where to look. The `Shell::render` call site uses `paths::well_known_locations()` instead of iterating the deleted const.

### c. `path_segments` tests cfg-gated

[tests/path_segments.rs](../../crates/feraille-gpui/tests/path_segments.rs) — the original Unix tests are now under `#[cfg(unix)]` and three Windows-shaped tests live under `#[cfg(windows)]`. `cargo test -p feraille-gpui` on Mac should run only the Unix tests (3 of them, all passing).

### d. `formats_compatible` regression tests

[feraille-core/src/lib.rs](../../crates/feraille-core/src/lib.rs) — 3 new tests for Office-format magic detection: `pptx_vs_powerpoint_presentation_is_compatible`, `docx_vs_word_document_is_compatible`, `xlsx_vs_excel_spreadsheet_is_compatible`. Pure-domain; should pass identically on Mac. Run `cargo test -p feraille-core`.

### e. `shell::Shell` struct gained a field

Adding `pub menu_bar: Option<Entity<gpui_component::menu::AppMenuBar>>` could surface as a "missing field" error in any `Shell { ... }` constructor outside `Shell::new`. The grep I ran said `Shell::new` is the only constructor; if you see field-init errors on Mac, search for stale literal `Shell {` blocks.

### f. `Shell::new` builds `menu_bar` from `cfg!(target_os = "macos")`

[shell.rs](../../crates/feraille-gpui/src/shell.rs) — the construction is `if cfg!(target_os = "macos") { None } else { Some(AppMenuBar::new(cx)) }`. On Mac this is always `None`, evaluated at compile time. Zero-cost.

---

## 4. Mac interactive sanity pass

After `cargo run --bin feraille-gpui`, walk through these. None should behave differently from before the Windows port.

| Feature | Expected on Mac | Where to look if broken |
|---|---|---|
| **Sidebar Locations** | Home / Applications / Desktop / Documents / Downloads / Trash / Movies / Music / Pictures (in that order) | [feraille-fs-native/src/paths.rs](../../crates/feraille-fs-native/src/paths.rs) macOS arm |
| **Sidebar Volumes** | `/Volumes` entries with NSURL-resolved labels + capacities | [feraille-fs-native/src/volumes.rs](../../crates/feraille-fs-native/src/volumes.rs) macOS arm |
| **Breadcrumb root** | `/` → `Users` → … (display same as before) | [shell/path.rs](../../crates/feraille-gpui/src/shell/path.rs) — refactored `path_segments` should be Mac-equivalent for Unix-rooted paths |
| **Copy Path** | Forward-slash paths, no `\` mixed in | n/a — uses `PathBuf::to_string_lossy()` directly |
| **Quick Look (Spacebar)** | Pops the macOS Quick Look HUD via `qlmanage -p` | [shell/file_ops.rs:494-525](../../crates/feraille-gpui/src/shell/file_ops.rs) — `on_quick_look` has explicit `cfg(target_os = "macos")` branch calling `platform_shell::show_quick_look`. Non-mac path toggles preview pane instead. |
| **Move to Trash** | NSFileManager `trashItemAtURL:` Trash | [feraille-fs-native/src/lib.rs](../../crates/feraille-fs-native/src/lib.rs) `move_to_trash` macOS arm — untouched |
| **Reveal in Finder** | Single Finder window for same-folder selection (may regress to N windows — see §2) | [shell/file_ops.rs](../../crates/feraille-gpui/src/shell/file_ops.rs) `on_reveal_in_finder` |
| **Title bar** | Traffic lights on left, no menu strip below | gpui-component's `TitleBar` is platform-aware; `cfg!(target_os = "macos") { None }` skips menu strip rendering |
| **App lifecycle** | Process stays resident after last window closes (Cmd+Q to exit) | [main.rs:359-371](../../crates/feraille-gpui/src/main.rs) — `on_window_closed → cx.quit()` is `cfg(not(target_os = "macos"))` only |
| **Disk Usage** package-descend toggle | Button visible in Disk Usage window header | [disk_usage.rs](../../crates/feraille-gpui/src/disk_usage.rs) — `.when(cfg!(target_os = "macos"), ...)` |
| **About menu** | Standard NSApp About panel with name/tagline/version | [feraille-shell-mac/src/app_menu.rs](../../crates/feraille-shell-mac/src/app_menu.rs) — untouched |
| **Theme follow** | App theme follows System Appearance dark/light toggle live | shell-mac's NSDistributedNotificationCenter observer — untouched |
| **Office files** (.docx, .xlsx, .pptx) in file list | No red-shield mismatch indicator | [feraille-core/src/lib.rs](../../crates/feraille-core/src/lib.rs) `formats_compatible` — new Office-app pairings work on both platforms |

---

## 5. Suggested order on Mac

```sh
# 1. Verify everything compiles.
cargo check --workspace --all-targets

# 2. Run all tests. Should include 3 new Office-pairing `format_label`
#    tests plus the Unix-cfg-gated path_segments tests.
cargo test --workspace

# 3. Smoke-launch the GUI.
cargo run --bin feraille-gpui

# 4. Eyeball the table in §4. If anything's wrong, jump to the
#    "Where to look if broken" column.

# 5. New CLI: smoke-test it produces a PNG.
cargo run --bin feraille-gpui -- thumb /Applications/Safari.app
```

If `cargo check` flags an error in a file I touched on Windows, it's most likely:

- An unused-import that's needed only on Windows and not gated → suppress with `#[allow(unused_imports)]` or add a `#[cfg(windows)]` gate.
- A trait bound that I tightened on Windows (`Send`) but didn't on Mac → the Mac signature should match for symmetry. The `start_system_theme_observer` callback is the only known site where I changed Windows but not Mac; verify the callsite closure satisfies both.

---

## 6. Things explicitly NOT touched

Per [windows-port.md §8](windows-port.md#8-working-on-windows-without-breaking-mac):

> Don't edit `feraille-shell-mac` source unless you're prepared for it to silently regress.

This session did NOT edit `feraille-shell-mac`. Every Windows arm lives in `feraille-shell-win32` (separate crate) or under `cfg(windows)` / `cfg(not(target_os = "macos"))` in shared crates.

The `feraille-shell-win32` crate has Windows-only function bodies plus `cfg(not(windows))` no-op stubs so the workspace `cargo check` keeps compiling it on Mac. Calling those stubs from a Mac build of `feraille-gpui` would never happen — the `platform_shell` cfg-alias only links `shell-mac` on Mac and `shell-win32` on Windows. But the stubs exist so the cross-target `cargo check --target x86_64-pc-windows-msvc` from Mac still resolves.

---

## 7. Useful diagnostics

- `FERAILLE_THUMB_DEBUG=1 cargo run --bin feraille-gpui -- thumb <path>` — prints per-step diagnostics for the thumbnail/preview pipeline. On macOS, this routes through `platform_shell::fetch_quick_look_thumbnail` which shells out to `qlmanage -t`. The debug var is honored by shell-mac on Mac and shell-win32 on Windows.
- `cargo test -p feraille-core format_label` — runs just the new Office-pairings tests in isolation.
- `cargo run --bin feraille-gpui -- du <path>` — disk-usage CLI. Same pipeline the Disk Usage window uses.
- `cargo run --bin feraille-gpui -- magic <path>` — magic-byte detection CLI.

---

## 8. Quick reference: what's in each touched file

| File | Change |
|---|---|
| `crates/feraille-core/src/commands.rs` | Added `REVEAL_LABEL` + `TRASH_LABEL` cfg-gated consts. Existing catalogue title fields now reference them. |
| `crates/feraille-core/src/lib.rs` | Office-format compatibility pairings in `formats_compatible` + 3 new tests. |
| `crates/feraille-fs-native/src/paths.rs` | NEW. `home_dir()`, `well_known_locations()`, `WellKnownLocation` struct. |
| `crates/feraille-fs-native/src/volumes.rs` | NEW. `list_volumes()` extracted from `lib.rs`. |
| `crates/feraille-fs-native/src/icons.rs` | Windows arm via `IShellItemImageFactory` + `SIIGBF_ICONONLY`. Mac arm untouched. |
| `crates/feraille-fs-native/src/xattr_info.rs` | Windows arm reading NTFS `Zone.Identifier` ADS. Mac arm untouched. |
| `crates/feraille-fs-native/src/lib.rs` | Removed inline `home_dir` / `list_volumes` (now re-exported from modules); added `move_to_trash` Windows arm. Mac arms preserved. |
| `crates/feraille-fs-native/Cargo.toml` | Added `windows = "0.58"` as cfg(windows) dep. |
| `crates/feraille-shell-win32/**` | Many additions — all under `cfg(windows)`. Stubs for `cfg(not(windows))` remain. |
| `crates/feraille-gpui/src/shell.rs` | Removed `LOCATIONS` const, `Location` struct, `impl Location::path`. Added `Shell::menu_bar` field. |
| `crates/feraille-gpui/src/shell/path.rs` | Refactored `path_segments` to handle Win32 `Prefix` components and runtime drive roots. Unix paths still produce the same `Vec` shape. |
| `crates/feraille-gpui/src/shell/file_ops.rs` | Reveal-in-Finder routed through `platform_shell`. Quick Look cfg-gated. |
| `crates/feraille-gpui/src/shell/render.rs` | Title bar drag-capture fix (mouse-down stop-propagation). Menu strip rendering for Windows. |
| `crates/feraille-gpui/src/main.rs` | `feraille thumb` CLI subcommand. `quit-on-last-window` for Windows. Menu-list mirror to gpui-component GlobalState. |
| `crates/feraille-gpui/src/disk_usage.rs` | Package-descend toggle button cfg-gated to macOS. |
| `crates/feraille-gpui/tests/path_segments.rs` | Unix tests cfg-gated, Windows tests added. |
| `crates/feraille-gpui/src/task_panel.rs` | Visual polish — thicker progress bar, more prominent header. Cross-platform. |
