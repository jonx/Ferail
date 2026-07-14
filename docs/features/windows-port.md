# Feraille — Windows port handoff

A self-contained orientation for picking up the Windows port from a Windows machine. Assumes you've worked on the macOS side recently — if not, read [docs/ARCHITECTURE.md](../ARCHITECTURE.md) first; the prime directives and crate boundaries described there apply unchanged on Windows.

This doc covers:

1. What Feraille is, in one minute.
2. The current state of the Windows port (what's done, what isn't).
3. Workspace map — where every kind of work lives.
4. The `platform_shell` indirection — how Windows code gets called.
5. Known macOS assumptions in `feraille-gpui` that need cfg-gating for correct Windows behavior.
6. Win32 implementation gaps — what's stubbed, mapped to the Microsoft API you'll likely reach for.
7. Day-one steps on a Windows machine.
8. Useful references.

---

## 1. What Feraille is

A fast file manager written in Rust, originally for macOS, built on Zed's [GPUI](https://github.com/zed-industries/zed) plus [longbridge/gpui-component](https://github.com/longbridge/gpui-component) for higher-level primitives (sidebar, title bar, settings, virtualized table, context menu). Today's macOS app ships with virtualized file listings, magic-first format detection, Finder tags, Quick Look previews, an async disk-usage window, multi-window + tabs, favorites with persistence, drag-and-drop, and a command catalogue surfaced through keyboard, menu, and Cmd+K palette.

It started as a port of Ferail, a **private predecessor project** (not part of this repo — if you don't have a checkout, treat every Ferail mention as design lineage only). That codebase is the porting *source* for native Win32 patterns (COM drag-drop, IContextMenu, IShellLink, shell pump, change notifications). Translate by intent, not by copy: Ferail is D2D/GDI + old Windows-Rust idioms; Feraille's renderer is GPUI's D3D11 backend on Windows. Architectural lessons port; literal code mostly doesn't.

The **prime directive** from [ARCHITECTURE.md](../ARCHITECTURE.md): the UI must never stop. Rendering, hover, hit-testing, scroll, resize, keyboard, text input, and modal drawing are read-only and non-blocking. Anything blocking (filesystem, SQLite, AppKit/Win32 shell calls, magic, previews) runs off the UI thread and reports back through GPUI's entity-update boundaries. This applies on Windows just as strictly.

---

## 2.1 Status update — parity pass (2026-07-14)

After absorbing the Mac progress (the July merges + `origin/aros-port`), a
parity pass on the Windows box closed the last big Windows capability gap and
fixed several latent Mac-authored assumptions:

- **Resilient file-ops shipped** (`feraille-shell-win32/src/elevation.rs`). The
  three "Chunk C" stubs are now real: `run_elevated_self` (`ShellExecuteExW`
  verb `"runas"` UAC + wait) powers **Retry as administrator**; `processes_using`
  (**Restart Manager**) names the process holding a locked file;
  `force_close_processes` (`RmShutdown` + `TerminateProcess` fallback) powers a
  new **"What's using it?" → "Close & retry"** toast. `elevation_available()` /
  `lock_diagnostics_available()` return true on Windows. Verified end-to-end
  against a real exclusive lock (named the holder, force-closed it, lock
  released).
- **`same_volume` got its real Windows arm** — volume serial via
  `CreateFileW(0 access, BACKUP_SEMANTICS)` + `GetFileInformationByHandle`,
  nearest-existing-ancestor walk (a drive-letter compare would lie under
  junction-mounted volumes). Windows moves now take the rename fast path instead
  of always copy+delete; the Mac-authored `move_renames_on_same_volume` test
  passes on Windows.
- **`recreate_symlink` got a real Windows arm** (`symlink_file`/`symlink_dir`
  by resolved target kind).
- **`video_mf` end-detection bug fixed** — a decode ERROR now fires the
  `on_ended` callback (it set a flag nobody read, stalling playlist
  auto-advance on a broken file).
- **mpv verified on Windows** — the optional libmpv backend loads via
  `LoadLibraryW` and plays real frames, matching the Mac; native Media
  Foundation video (`video_mf.rs`) also confirmed after the viewer refactor.

Still remaining on Windows: reserved-name/char input validation, pasteboard
volume-identity, the two Ferail-only capabilities (§6b B.4 shell verbs, B.5
WSL), and the `\\?\` verbatim prefix leaking into a couple of display strings.

## 2.0 Status update — `windows-parity` branch (2026-06-23)

The port was first built/run **natively on Windows** (not just cross-checked
from Mac) and brought to broad feature parity. All of the below is verified by
building + running + screenshotting on a real Windows 11 box. Each landed as its
own commit on the `windows-parity` branch:

- **Build restored.** The big macOS refresh had leaked 5 Mac-only assumptions
  into `feraille-gpui` (direct `feraille_shell_mac::` calls in `entry_info.rs`;
  `show_desktop` / `show_desktop_available` missing from win32/linux). Routed
  through `platform_shell`; added the missing surface to both shell crates.
- **mpv video is cross-platform.** Its SW-render frame-pull model is
  platform-neutral — only the `dlopen` loader + dylib layout are Mac-shaped.
  A `dynload` shim (dlopen on unix, `LoadLibraryW` on Windows) + per-OS path
  resolution generalise it. `--features mpv` works on Windows/Linux; point
  Settings → Plugins at the libmpv library (e.g. `libmpv-2.dll` on Windows).
- **Headless `--screenshot` works** (it had regressed — gpui_windows has no
  `render_to_image`). Restored via `PrintWindow(PW_RENDERFULLCONTENT)`, with the
  window placed off-screen + `WS_EX_TOOLWINDOW` so it's invisible to the user.
- **Get Info is now Properties-dialog-level.** `read_stat_info` was unix-only →
  returned `None` on Windows (Size "0 B", no dates). Implemented via
  `MetadataExt` (size, created/modified/accessed, read-only/hidden attrs); the
  Locked/Invisible toggles write via `Get/SetFileAttributesW`; `read_shell_info`
  returns the native type name (`SHGetFileInfoW`/`SHGFI_TYPENAME`).
- **Cmd+P preview fixed** — an auto-hide below 900px was silently suppressing
  the explicit toggle on smaller windows.
- **make_alias_in / pick_folder / eject_volume** implemented (IShellLink in a
  dest dir; `IFileOpenDialog` + `FOS_PICKFOLDERS`; dismount + `IOCTL_STORAGE_EJECT_MEDIA`).
- **Sidebar Locations resolve via `SHGetKnownFolderPath`** so OneDrive-moved
  Documents/Pictures/etc. point at the real path (was a literal
  `%USERPROFILE%\Pictures` that "Folder not found"-ed on most OneDrive boxes).
- **Verified working as-is** (no change needed): Mark-of-the-Web (`Zone.Identifier`
  ADS → the same quarantine UI as macOS, with Source/Referrer + Unblock), drive
  Volumes, per-file icons, dark theme chrome (custom titlebar), grid/list/
  settings/disk-usage/search/duplicates rendering.

**Both of the previously-large items are now DONE:**

1. **Native-default video** — *done.* `video_overlay_*` is implemented natively
   via Media Foundation's `IMFMediaEngine` frame-server (D3D11 device +
   `TransferVideoFrame` readback + automatic audio/sync), the analogue of macOS
   AVFoundation. Verified decoding + displaying real frames. mpv remains the
   optional cross-platform plugin. See `crates/feraille-shell-win32/src/video_mf.rs`.
2. **Truly-headless screenshot** — *done.* `render_to_image` is implemented in
   `gpui_windows` (D3D11 staging-texture readback); the harness captures with
   the window never shown — no flash. The change lives in
   [`patches/gpui-windows-render-to-image.patch`](../../patches/gpui-windows-render-to-image.patch),
   applied locally via a `[patch]` to a sibling zed clone (NOT committed — a
   local path would break the macOS build). Open it as a zed PR to land it
   permanently; until then it's a local-dev patch. See
   [GPUI-UPSTREAM.md](../GPUI-UPSTREAM.md) item 7.

**Still genuinely remaining:**

- A handful of interaction/UX items surfaced by on-device testing (e.g. the
  Settings → Plugins mpv dropdown selection — logic verified, but needs an
  interactive repro).
- Linux parity (the sister [linux-port.md](linux-port.md)).

The rest of §2–§6 below is the original Mac-authored handoff and predates this
work; read it for the architecture/mechanics, but treat §2.0 as the current
state.

---

## 2. State of the Windows port (as of this writing)

**Already done from macOS** (committed; verified from Mac via `cargo check --target x86_64-pc-windows-msvc`):

- Workspace builds end-to-end for `x86_64-pc-windows-msvc` for every crate that doesn't pull SQLite/`psm` C deps.
- Source-level audit complete — no direct `objc2` / AppKit use in `feraille-gpui` or any non-shell-mac crate. All macOS coupling funnels through `feraille-shell-mac`.
- `feraille-shell-win32` crate exists at [crates/feraille-shell-win32](../../crates/feraille-shell-win32/) and mirrors `feraille-shell-mac`'s public API surface.
- A cfg-alias `feraille_gpui::platform_shell` resolves to the right shell crate per target (see §4). All 27 call sites in gpui go through it.
- A growing set of surfaces have **real Win32 implementations** in shell-win32: `show_alert` (MessageBoxW), `copy_to_clipboard` (CF_UNICODETEXT), `system_is_dark` + `start_system_theme_observer` (registry read of `AppsUseLightTheme` + a `WM_SETTINGCHANGE` message-only window), `reveal_in_finder` (`explorer /select`), `open_terminal` (`wt.exe -d`), `open_url` (`cmd /C start`), `duplicate_path` (pure `std::fs` with Explorer's `- Copy (N)` naming), `make_alias` (`IShellLinkW` + `IPersistFile` → `.lnk`), `compress_paths` (`zip` crate), `open_with_candidates` / `open_with_app` (`SHAssocEnumHandlers`), `set_app_user_model_id`, `fetch_quick_look_thumbnail` (`IShellItemImageFactory`), plus the headless `capture_window_rgba` (PrintWindow) and `preview_handler` (`IPreviewHandler`) pipelines. See §6 for the live real/stub split.
- `feraille-shell-win32` cross-compiles cleanly to MSVC from Mac (no C deps).
- `windows = "0.58"` is wired as a `cfg(windows)`-only dep with a tight feature set.

**Not yet done — your work ahead** (rough priority order):

1. **Build the whole workspace on Windows.** From Mac we can only cross-compile up to feraille-meta (then `libsqlite3-sys` and `psm` need MSVC C headers we don't have). On Windows that's not an issue. Step 1 is "does it actually build?"
2. **Run it.** Confirm GPUI's `gpui_windows` backend (it ships at the Zed rev we pin; `cargo tree -i psm` shows it transitively in the graph) renders our shell. Triage anything broken in `gpui-component` on Windows — longbridge tests primarily on macOS.
3. **Fill in real Win32 implementations** for the still-stubbed surfaces (§6). Each one is small and isolated; bite-sized.
4. **Conditionalize macOS assumptions** in `feraille-gpui` that compile on Windows but behave wrong (§5).
5. **Add Windows-specific UX:** Mark-of-the-Web (NTFS `Zone.Identifier`) instead of `com.apple.quarantine` xattr, long-path (`\\?\`) handling, native chrome adjustments. (Drive-letter enumeration and Recycle Bin trash already landed in `feraille-fs-native` — see §6.)

The macOS feature spec in [feraille-windows-instances-tabs-spec.md](feraille-windows-instances-tabs-spec.md) covers multi-window + tabs + tear-off intent at the product level — none of that is OS-specific; it'll all work on Windows once the platform layer's caught up.

---

## 3. Workspace map

```text
crates/
├── feraille-core           Domain types, command catalogue, NodeId/FileEntry,
│                           NodeStore. Zero platform deps.
├── feraille-design         Shared visual constants. `TextTokens` is now the
│                           live type scale (see ARCHITECTURE Typography).
├── feraille-disk-usage     Pure disk-usage model + treemap layout. No I/O.
├── feraille-fs-native      std::fs backend + icons (NSWorkspace on Mac),
│                           magic detection, disk-usage scanner, xattr
│                           (Mac quarantine + tags). cfg-gated mac arms.
├── feraille-meta           SQLite-backed metadata store. rusqlite bundled.
├── feraille-shell-mac      macOS platform shell — AppKit/Cocoa/NSWorkspace.
│                           Real impls under cfg(target_os = "macos");
│                           no-op stubs under cfg(not(target_os = "macos")).
├── feraille-shell-win32    Windows platform shell — Win32 via `windows` 0.58.
│                           Real impls under cfg(windows); no-op stubs
│                           under cfg(not(windows)). This is your home base.
└── feraille-gpui           The app. Views, actions, tasks, sidebar, file
                            list, preview, tabs, multi-window. All Mac
                            coupling goes through `platform_shell::*`.
```

**Where to put new Windows code:**

| Kind of work | Crate | File pattern |
|---|---|---|
| Native shell API (clipboard, registry, MessageBox, etc.) | `feraille-shell-win32` | New `src/<topic>.rs`, re-exported via `lib.rs` |
| Filesystem (icons, MoTW, ADS, drive enumeration) | `feraille-fs-native` | Add `cfg(windows)` arms next to the existing `cfg(target_os = "macos")` ones |
| UI / view tree changes | `feraille-gpui` | Same file you'd touch on Mac; gate platform diffs with `cfg`, or call into `platform_shell` |
| Domain logic | `feraille-core` | Should stay platform-agnostic; if you reach for `cfg` here, reconsider |
| SQLite schema for Windows-specific state | `feraille-meta` | New table or extended one |

**Reference repos to read alongside:**

- Ferail's `ferail-win32` crate (private predecessor): working Win32 implementations of drag-drop (`drag_drop.rs`), context menus (`popup_menu.rs`), shell namespace (`shell.rs`), change notifications (`shell_pump.rs`). The COM dance is the part to port.
- `crates/feraille-shell-mac/src/`: per-feature module structure; mirror it in shell-win32 once individual surfaces grow beyond a single function.

---

## 4. The `platform_shell` indirection

Gpui never calls `feraille_shell_mac::` or `feraille_shell_win32::` directly. It calls `crate::platform_shell::X` (or `feraille_gpui::platform_shell::X` from the binary entry point).

The alias is set in [crates/feraille-gpui/src/lib.rs](../../crates/feraille-gpui/src/lib.rs):

```rust
#[cfg(target_os = "macos")]
pub use feraille_shell_mac as platform_shell;
#[cfg(windows)]
pub use feraille_shell_win32 as platform_shell;
```

The dep in [crates/feraille-gpui/Cargo.toml](../../crates/feraille-gpui/Cargo.toml) is target-conditional, so exactly one shell crate is linked per build:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
feraille-shell-mac.workspace = true

[target.'cfg(windows)'.dependencies]
feraille-shell-win32.workspace = true
```

**Adding a new shell surface:**

1. Implement it under `cfg(windows)` in `feraille-shell-win32`.
2. Add a `cfg(not(windows))` no-op fallback so Mac builds of shell-win32 (workspace check on Mac) still compile.
3. Add the same function signature to `feraille-shell-mac`, real-or-stubbed.
4. Call it from gpui as `crate::platform_shell::your_function(...)`.

The shell crates' "other-OS" no-op arms exist purely so each crate compiles on the *other* host as a workspace member. They're never reached through the `platform_shell` alias — `cargo` only links the matching crate per target.

**Types** (`OpenWithCandidate`, `SetIconResult`, etc.) are defined unconditionally in both shell crates with identical shape, so they round-trip through the alias.

---

## 5. macOS assumptions in `feraille-gpui` that need cfg-gating

These are the cases where the code compiles for Windows today (because `platform_shell` stubs cover the call surface) but the *behavior* would be wrong. Each needs a `#[cfg]` arm or a small redesign.

| Site | macOS behavior | Windows behavior wanted |
|---|---|---|
| [main.rs](../../crates/feraille-gpui/src/main.rs) `run_gui` | Stay resident with zero windows (Phase C decision; Finder/Safari model). Removed `cx.on_window_closed`. | **Quit when last window closes.** Windows apps don't stay resident with no UI; no dock equivalent. Re-add the `on_window_closed → cx.quit()` handler under `cfg(windows)`. |
| [main.rs](../../crates/feraille-gpui/src/main.rs) menu install | `install_app_menus(cx)` installs an NSApp-level menu bar. | No global menu on Windows. Either drop into a per-window `HMENU` via gpui's window menu API (if it exists) or a hamburger button in the title bar. |
| [main.rs](../../crates/feraille-gpui/src/main.rs) titlebar options | `gpui_component::TitleBar::title_bar_options()` reserves macOS traffic-light area on the left. | Windows caption buttons sit on the **right** (min/max/close). Need a Windows-appropriate title-bar layout — check what `gpui-component` already offers. |
| Action labels: "Reveal in Finder", "Move to Trash" (catalogue strings in [feraille-core/src/commands.rs](../../crates/feraille-core/src/commands.rs)) | Finder / Trash literal. | "Reveal in Explorer" / "Move to Recycle Bin". Either swap by cfg in the catalogue or by a localized-string lookup. |
| Spacebar → Quick Look | Pops Quick Look panel on the selected file. | Windows has no Quick Look. Either drop the binding on Windows or build an in-process preview window (already a TODO.md item: "Add real previews"). |
| Sidebar **Volumes** | Reads from `feraille-fs-native::list_volumes()` which today shells out to `mount` / reads `/Volumes`. | Enumerate drive letters via `GetLogicalDrives` / `GetDriveTypeW`, plus optional network mounts and UNC roots. Wants a Windows arm in [crates/feraille-fs-native/src/lib.rs](../../crates/feraille-fs-native/src/lib.rs) for `list_volumes`. |
| Trash (`MoveToTrash` action) | macOS Trash via NSFileManager (currently called through shell-mac's `file_ops::move_to_trash`). | Recycle Bin via `SHFileOperationW` (`FO_DELETE` + `FOF_ALLOWUNDO`) or modern `IFileOperation`. Sits naturally in `feraille-shell-win32` as `move_to_recycle_bin`. |
| Quarantine indicators (the red shield in the file table) | Reads `com.apple.quarantine` xattr via [feraille-fs-native/src/xattr_info.rs](../../crates/feraille-fs-native/src/xattr_info.rs). | Windows Mark-of-the-Web lives in the NTFS Alternate Data Stream `<file>:Zone.Identifier`. Different format, similar UI surface. Add a Windows arm to `fetch_quarantine_info`. |
| Finder tags (color chips in the file table) | `read_canonical_tags` / `toggle_tag` via `com.apple.metadata:_kMDItemUserTags`. | **No Windows equivalent.** Either drop the feature on Windows (recommended for v1) or back it via `feraille-meta` SQLite (no system-wide integration with Explorer). |
| Path separators in display strings | `/` everywhere (macOS-native). | `std::path::PathBuf` handles separators correctly; double-check that display strings (e.g. breadcrumb segments in [feraille-gpui/src/shell/path.rs](../../crates/feraille-gpui/src/shell/path.rs)) don't hardcode `/`. |
| Cmd+W / Cmd+T / Cmd+N in [keymap.rs](../../crates/feraille-gpui/src/keymap.rs) | macOS `cmd` key. | gpui's `"cmd-X"` keybind string maps `cmd` → Ctrl on Windows/Linux automatically (Zed convention). Should "just work" — but verify the catalogue's `primary` semantics ([feraille-core/src/commands.rs](../../crates/feraille-core/src/commands.rs)) land as Ctrl, not Win key. |
| `cargo run --bin feraille-gpui -- --screenshot ...` headless harness | macOS-specific app icon installation in `screenshot::run`. | The icon-install call already routes through `platform_shell::set_app_icon_from_png_bytes` (currently a stub on Windows). Should be functional once we wire `WM_SETICON` or attach via manifest. **Fixed 2026-05-15:** `gpui_windows` has no `render_to_image`; the harness now routes through [`feraille_shell_win32::capture_window_rgba`](../../crates/feraille-shell-win32/src/capture.rs) (PrintWindow with `PW_RENDERFULLCONTENT`). The window is shown on-screen during capture on Windows since DirectComposition swap chains don't present when hidden — brief flash, acceptable for a CLI tool. |

---

## 6. Win32 implementation gaps

The `feraille-shell-win32` public surface mirrors shell-mac. What's already real vs. what's a stub returning `Err(...)` / `None` / no-op:

### Real implementations (as of 2026-06-20)

| Function | Implementation strategy |
|---|---|
| `show_alert(title, body)` | `MessageBoxW` with `MB_ICONINFORMATION + MB_OK`. |
| `copy_to_clipboard(text)` | `OpenClipboard` → `EmptyClipboard` → `GlobalAlloc(GHND)` → `GlobalLock`/`Unlock` → `SetClipboardData(CF_UNICODETEXT, …)` → `CloseClipboard`. RAII guard for paired close. |
| `system_is_dark()` | `RegGetValueW(HKCU, "…\\Themes\\Personalize", "AppsUseLightTheme")`. `0` = dark, `1` = light. Missing key → light. |
| `start_system_theme_observer(cb)` | Message-only window via `CreateWindowExW(HWND_MESSAGE)`; WndProc filters `WM_SETTINGCHANGE` lParam `"ImmersiveColorSet"`; re-reads `system_is_dark()` and fires the callback on a worker thread. (Box still leaks — future work keeps a handle.) |
| `reveal_in_finder(path)` | `explorer.exe /select,<path>` shellout. |
| `open_terminal(dir)` | `wt.exe -d <dir>` shellout. |
| `open_url(url)` | `cmd /C start "" <url>` shellout. (Future: `ShellExecuteW` so we don't spawn cmd.) |
| `duplicate_path(src)` | `std::fs::copy` for files; recursive walk for dirs. Naming: `<stem> - Copy.<ext>`, then `<stem> - Copy (2).<ext>`…, capped at 99. |
| `make_alias(target)` | `CoCreateInstance(ShellLink)` → `IShellLinkW::SetPath` → `IPersistFile::Save(<target>.lnk)`. |
| `compress_paths(targets)` | Pure-Rust `zip` crate; recursive dir walk, symlinks skipped. Explorer naming (`<basename>.zip` / `Archive.zip`). |
| `open_with_candidates(path)` | `SHAssocEnumHandlers(ASSOC_FILTER_RECOMMENDED)` → `IAssocHandler::GetName`/`GetUIName`, capped at 12. |
| `open_with_app(target, app)` | spawns the chosen handler with the target as argument. |
| `set_app_user_model_id()` | `SetCurrentProcessExplicitAppUserModelID` for taskbar grouping. |
| `fetch_quick_look_thumbnail(path)` | `IShellItemImageFactory::GetImage(SIIGBF_THUMBNAILONLY | SIIGBF_RESIZETOFIT)`, reads `DIBSECTION` directly to preserve alpha. |
| `clipboard_copy_file_urls` / `clipboard_read_file_urls` | `CF_HDROP`: write packs a `DROPFILES` header + double-null-terminated UTF-16 path list into one `GHND` HGLOBAL → `SetClipboardData(CF_HDROP, …)`; read walks the drop with `DragQueryFileW` (`0xFFFFFFFF` for count, then per-index length+content). The clipboard owns the read handle — no `DragFinish`/free. Gives Cmd+C/Cmd+V parity + Explorer interop. |
| `start_volume_observer` | `WM_DEVICECHANGE` (`DBT_DEVICEARRIVAL` / `DBT_DEVICEREMOVECOMPLETE`, filtered to `DBT_DEVTYP_VOLUME` by reading the `DEV_BROADCAST_HDR` device-type field by offset) on a worker thread. **Uses a hidden _top-level_ window, not the theme observer's `HWND_MESSAGE` one** — message-only windows are excluded from broadcasts, and drive-letter volume changes arrive only as broadcasts to top-level windows (`DBT_DEVTYP_VOLUME` isn't obtainable via `RegisterDeviceNotification`). Callback fires on the worker thread (`Send`); host marshals. |
| `capture_window_rgba` (capture.rs) | `PrintWindow(PW_RENDERFULLCONTENT)` → top-down BGRA DIB. Headless `--screenshot` harness. |
| `preview_handler` (preview_handler.rs) | `AssocQueryStringW(ASSOCSTR_SHELLEXTENSION)` → `IPreviewHandler` (Office/PDF) rendered into an off-screen host HWND, then `PrintWindow`-captured. |

Also real but living in **`feraille-fs-native`** (not the shell crate), so already at parity cross-platform: drive-letter enumeration (`GetLogicalDrives`/`GetDriveTypeW`/`GetVolumeInformationW`), per-file icons (`SHGetFileInfo`/`IShellItemImageFactory`), Recycle Bin trash (`SHFileOperationW` `FO_DELETE | FOF_ALLOWUNDO`), and the cross-platform `file_ops` copy/move engine.

### Stubs awaiting real implementation

Ordered by approximate value × ease. Each function has a TODO at the top of its body in [crates/feraille-shell-win32/src/lib.rs](../../crates/feraille-shell-win32/src/lib.rs). All three behavior-breaking stubs are now closed: `clipboard_copy_file_urls` / `clipboard_read_file_urls` (`CF_HDROP`) and `start_volume_observer` (`WM_DEVICECHANGE`) **shipped 2026-06-20** and moved to the real-implementations table above; the text-naming gap is closed by the shared `open_text_prompt` gpui modal in `feraille-gpui` (rename + new-folder both route through it), and the dead `prompt_for_text` shell stub was deleted from both platform crates. Only feature-sized stubs remain.

| Function | Breaks | Suggested approach |
|---|---|---|
| `video_overlay_show` / `set_frame` / `remove` | Viewer video playback (shows static poster only). | Media Foundation / MFPlay child HWND floated over the viewer stage rect. Larger; viewer-feature-sized. See VIEWER.md. |
| `read_canonical_tags`, `clear_tags` (+ `toggle_tag`) | Finder-tag color chips. | No native Windows equivalent. Drop for v1, or back via `feraille-meta` SQLite as private tags (no Explorer integration). |
| `show_quick_look` | Spacebar Quick Look. | No Quick Look on Windows. Spacebar should pop the in-app preview pane (already wired) or no-op. |
| `set_app_icon_from_png_bytes` | Runtime icon swap. | Icon is baked into the binary via the build manifest; runtime swap intentionally skipped. Leave as no-op. |
| `install_app_menu`, `register_command_callback`, `set_tab_count`, `set_command_state`, `set_about_options`/`show_about_panel` chrome | NSApp main-menu paradigm. | No global menu on Windows — a title-bar hamburger covers about/settings. Drop for v1, or per-window `HMENU`. |

### Not in the win32 shell crate today (winit-window-taking surface)

These shell-mac functions take `&winit::window::Window` and aren't reachable through the `platform_shell` alias (gpui doesn't pass winit handles): `begin_drag`, `show_context_menu`, `install_services_anchor`, `set_services_selection`, `show_share_picker`, `apply_native_chrome`. When gpui needs drag-out-to-Explorer, it'll grow a fresh function that takes an HWND or gpui's own window handle — don't try to recycle these signatures.

---

## 6b. Parity with Ferail — capability diff

This is the honest reckoning of where the GPUI rewrite stands against the private predecessor's Win32 crate (Ferail's `ferail-win32`, ~3,700 lines). **Read it by intent, not by line count** — Feraille is *ahead* of Ferail in feature breadth and architecture on both platforms, but Ferail still does a handful of Windows-native things the port hasn't reimplemented yet. Three buckets:

### A. Already at parity — covered by GPUI or fs-native, do **not** re-port

Ferail hand-rolled these in Win32; in Feraille they're handled cross-platform and need no Windows-specific work:

| Ferail module | Why it's already covered |
|---|---|
| `d2d.rs` (Direct2D + DirectWrite + WIC rendering) | GPUI's D3D11 backend paints everything. Renderer lessons only — never port this. |
| `gdi.rs` (DPI query, physical↔DIP math) | GPUI owns DPI scaling and hit-testing. |
| `popup_menu.rs` (HMENU build + timer pump) | GPUI / gpui-component renders the context menu natively. |
| `menu_model.rs`, `menu_preload.rs` (HMENU model + preload) | The menu *model* is GPUI's; the preload *idea* may return (see C below), but the HMENU-specific machinery is obsolete. |
| `enumerate.rs` (`FindFirstFileExW` large-fetch) | `feraille-fs-native` enumerates cross-platform via `std::fs`. A `FindFirstFileExW` fast path is an *optional* perf win for huge dirs, not a feature gap. |
| `shell.rs` (`SHGetFileInfo` icons/type names) | Ported into `feraille-fs-native/src/icons.rs` under `cfg(windows)`. |
| `com.rs` (COM RAII guard) | Trivial; done inline where needed. |

### B. Real remaining gaps — Windows features Ferail had that Feraille lacks

In rough priority order. The first three were the §6 stubs above and are now all shipped; the last two are genuinely absent from the whole workspace, not just stubbed:

1. ~~**File-URL clipboard (`CF_HDROP`)**~~ — **shipped 2026-06-20** (§6 real table). Keyboard copy-paste + Explorer interop now work.
2. ~~**Volume device-change observer (`WM_DEVICECHANGE`)**~~ — **shipped 2026-06-20** (§6 real table). Note the watch needed a hidden top-level window, *not* the theme observer's message-only one — message-only windows don't get broadcasts.
3. ~~**Inline-rename / new-folder text input**~~ — **shipped 2026-06-20.** Solved as the shared cross-platform `open_text_prompt` gpui modal (`feraille-gpui`) rather than Win32; rename and new-folder both route through it (focus + select-on-open), and the dead native `prompt_for_text` stub was removed from both shell crates.
4. **Third-party shell-extension context-menu verbs** — *absent everywhere.* Ferail's `shell_pump.rs` (~1,400 lines) enumerated registry `shellex\ContextMenuHandlers`, `CoCreateInstance`'d each handler, `IShellExtInit::Initialize`'d it with an `IDataObject`, and `QueryContextMenu`'d the verbs into the menu (7-Zip, TortoiseGit, "Scan with Defender", etc.) — including the undocumented `IWaitCursorManager` trick to suppress the busy cursor. Feraille's GPUI context menu has built-in actions + `Open With`, but **no third-party shell verbs.** This is the single biggest capability gap. It's also the hardest: STA-thread-only, PIDL lifetime management, and it must feed GPUI's menu model rather than an HMENU. Treat as its own feature iteration; the `IContextMenu` enumeration logic ports closely even though the rendering doesn't.
5. **WSL integration** — *absent everywhere.* Ferail's `wsl.rs` (~480 lines): registry distro enumeration (`…\Lxss`), `\\wsl$\` / `\\wsl.localhost\` UNC path parsing, and `wsl.exe readlink` symlink resolution. Per the [porting rule](../../CLAUDE.md) WSL was deliberately deferred ("not a macOS v1 feature unless it maps to network/remote volumes"). It's a Windows-only differentiator worth restoring **after** the core port runs — net-new value, not a regression.

### C. Optional / lessons-only

- **Menu preload** (`menu_preload.rs`): the *concept* — warm the expensive shell-verb enumeration on selection-change so the menu opens instantly — becomes valuable again **if and when** gap B.4 lands. Until there are third-party verbs to enumerate, there's nothing to preload. Keep the idea; the code is HMENU-bound.
- **`FindFirstFileExW` fast enumeration**: revisit only if profiling shows `std::fs::read_dir` is a bottleneck on large Windows directories.

**Bottom line:** B.1–B.3 all shipped 2026-06-20 — the Windows build is now at behavioral parity for everyday file management, with no behavior-breaking platform-shell stubs left. B.4 (shell verbs) and B.5 (WSL) are the two substantial Ferail capabilities still worth a dedicated port afterward — and they're the only places Ferail remains genuinely "more advanced" than Feraille.

---

## 7. Day-one steps on a Windows machine

Assumes Windows 10+ with admin to install toolchain. Adjust paths if your dev drive isn't `C:\dev\`.

```pwsh
# 1. Toolchain
winget install --id Rustlang.Rustup
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--passive --add Microsoft.VisualStudio.Workload.VCTools `
  --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
  --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
winget install --id Git.Git

# 2. Toolchain config (after rustup completes)
rustup default stable
rustup target add x86_64-pc-windows-msvc  # usually default on Windows; idempotent

# 3. Clone Feraille
mkdir C:\dev ; cd C:\dev
git clone <your-feraille-remote> Feraille
cd Feraille

# 4. Optional: also clone the Windows predecessor for porting reference
git clone <your-ferail-remote> ..\Ferail

# 5. First build (expect this to surface real issues — see below)
cargo check --workspace --all-targets
cargo build --bin feraille-gpui
cargo run --bin feraille-gpui
```

**What'll probably fail first, in rough order:**

1. **`libsqlite3-sys` build script wants a C compiler.** Should work once VC Tools is installed; if not, `set CC=cl.exe`. The `bundled` feature compiles SQLite from source; on Windows this needs `cl.exe` on `PATH` (the "Developer Command Prompt for VS 2022" sets this up; or run `vcvarsall.bat amd64` from a regular shell).
2. **`gpui_windows` linker errors.** Zed pins specific Windows SDK versions in its build script. If `cargo build` complains about missing imports, you may need a newer Windows 11 SDK component than the one bundled with VC Tools by default (use Visual Studio Installer → Modify → Individual components → Windows 11 SDK 10.0.22621.x).
3. **gpui-component visual oddities.** Title bar height, traffic-light insets, sidebar borders — anything visual was tested primarily on macOS. Take a screenshot and triage by element.
4. **Stay-resident-at-zero-windows is wrong on Windows.** Close the last window → process keeps running invisibly. First behavioral bug to fix; see §5 row 1.
5. **Sidebar Volumes is empty or wrong.** `feraille-fs-native::list_volumes()` doesn't have a Windows arm yet; add one (`GetLogicalDrives` + per-drive `GetDriveTypeW` + `GetVolumeInformationW` for labels).

**Verification commands:**

```pwsh
# Quick sanity at any point:
cargo check --workspace --all-targets

# Mac side stays green (CI / cross-team safety):
# (run on macOS if you have access)
cargo check --workspace --all-targets

# Run the shell:
cargo run --bin feraille-gpui

# Headless render — same flags as on Mac:
cargo run --bin feraille-gpui -- --screenshot screenshots\win-baseline.png

# Tests:
cargo test --workspace
```

**Convention reminders:**

- Screenshots go in the repo's `screenshots/` folder, not `%TEMP%`.
- Never run broad formatters (`cargo fmt --all`); the repo often has work-in-progress local changes.
- New native code in `feraille-shell-win32`; new filesystem code as `cfg(windows)` arms in `feraille-fs-native`. Don't put Win32 calls in `feraille-gpui` directly.

---

## 8. Working on Windows without breaking Mac

macOS is the established platform — `feraille-shell-mac` is a much bigger, more battle-tested crate than `feraille-shell-win32`, and the visible product lives there. Your job from Windows is to advance the Win32 side **without regressing the Mac side you can't see**. A few specific disciplines:

**You can't cross-compile to macOS from Windows.** Apple restricts the toolchain to Apple hardware; there's no `cargo-xwin` equivalent for darwin targets. So **CI or a teammate's Mac is the gate for Mac correctness**. Push early and often; treat the Mac CI signal as load-bearing.

**Every new `platform_shell::X` surface needs both crates.** When you add a function under `cfg(windows)` in `feraille-shell-win32`, add the matching signature to `feraille-shell-mac` too — real impl if you can write one, a `cfg(not(target_os = "macos"))`-style stub if you can't. The alias only works when both crates expose the symbol. The reverse is also true: if you find a shell-mac function you want to remove because it's dead on Mac too, remove it from both.

**When you cfg-gate behavior in `feraille-gpui`, write both arms.** A `#[cfg(windows)] { ... } #[cfg(not(windows)) { ... }]` block forces you to think about the Mac side. `cfg(target_os = "macos")` is fine for the Mac arm; **don't** use `cfg(unix)` (it'd catch Linux too) or `cfg(not(windows))` (it'd catch every non-Windows target). Be specific.

**Don't edit `feraille-shell-mac` source unless you're prepared for it to silently regress.** `cargo check --workspace` on Windows builds shell-mac's `cfg(not(target_os = "macos"))` no-op arms, not the real AppKit code — so a typo in a `cfg(target_os = "macos")` block won't catch fire until someone builds on Mac. Two safe ways to touch shell-mac from Windows: (a) keep edits to the no-op arms or to the API signature, (b) get a Mac dev to run `cargo check --target aarch64-apple-darwin` on the branch before you push.

**The command catalogue is shared and platform-agnostic.** `crates/feraille-core/src/commands.rs` defines actions and shortcuts for both platforms. If you add a Windows-only command, that's fine — gate the handler, not the catalogue entry. If you change a shortcut, you're changing it for Mac too. The `Shortcut::primary("X")` helper maps to Cmd on Mac and Ctrl on Windows automatically, so the *binding* stays portable; just be aware that a chord like Cmd+Option+Shift maps to Ctrl+Alt+Shift on Windows and may collide with something native that Mac doesn't have.

**Tests don't catch macOS regressions from Windows.** `cargo test --workspace` from Windows builds and runs only `cfg(not(target_os = "macos"))` paths. Test failures from Mac-only behavior will only show up on Mac CI / a Mac dev. Plan for it; don't assume green tests locally means green on Mac.

**Things you *can* safely change from Windows** without Mac-side risk: `feraille-shell-win32` (the cfg(windows) arms), `cfg(windows)` arms anywhere else, the catalogue's shortcut declarations if you understand the cross-platform mapping, anything in `feraille-core` / `feraille-disk-usage` / `feraille-design` (no platform code by design), gpui code where you've added matching `cfg(target_os = "macos")` arms.

---

## 9. References

**Within the repo:**

- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — crate boundaries, data model, work-scheduling rules. Apply unchanged on Windows.
- [CLAUDE.md](../../CLAUDE.md) — operating manual for AI / human edits.
- [TODO.md](../../TODO.md) — open work backlog. Windows port items will accrue here as they're discovered.
- [NOTES.md](../../NOTES.md) — multi-iter spec working log (selection + windows/instances/tabs specs).
- [docs/features/](.) — feature design notes, including the multi-window + tabs spec which is OS-agnostic.

**External / Microsoft:**

- [`windows` crate docs](https://microsoft.github.io/windows-docs-rs/doc/windows/) — current Microsoft-maintained Rust bindings. Search by Win32 function name; the crate's modules mirror the SDK headers.
- [`windows-rs` GitHub](https://github.com/microsoft/windows-rs) — issue tracker; many gotchas with feature flags are answered here.
- [Win32 docs on learn.microsoft.com](https://learn.microsoft.com/en-us/windows/win32/) — authoritative behavior reference. The semantics matter more than the literal call shape.

**Reference repos:**

- Ferail's `ferail-win32` sources (private predecessor; maintainer-only) — working Win32 patterns. `drag_drop.rs`, `popup_menu.rs`, `shell.rs`, `shell_pump.rs` are the high-value reads. Don't paste-port; the renderer model is different.
- [Zed's `gpui_windows`](https://github.com/zed-industries/zed/tree/main/crates/gpui_platform/src/platform/windows) — the platform backend gpui uses on Windows. When in doubt about how gpui interacts with HWNDs, read here.
