# Feraille — Linux port handoff

A self-contained orientation for starting the Linux port from a Linux machine.
Assumes you've worked on (or read about) the macOS side recently — if not, read
[docs/ARCHITECTURE.md](../ARCHITECTURE.md) first; the prime directive and crate
boundaries described there apply unchanged on Linux. The
[Windows port handoff](windows-port.md) is the sister document and shares the
same structure — much of the cross-platform discipline there applies verbatim,
so skim it too.

This doc covers:

1. What Feraille is, in one minute.
2. The current state of the Linux port (scaffold landed — a lot already comes
   for free).
3. Workspace map — where every kind of work lives.
4. The `platform_shell` indirection — and the **one change that makes the app
   compile on Linux at all**.
5. macOS assumptions in `feraille-gpui` that need cfg-gating for Linux.
6. The Linux shell surface — each function mapped to the freedesktop / D-Bus /
   XDG mechanism you'll likely reach for.
7. What "Linux" even means here, and where you can do the work (native Linux,
   WSL2 on Windows, or a Mac).
8. Day-one steps on a Linux machine.
9. Working on Linux without breaking Mac/Windows.
10. References.

---

## 1. What Feraille is

A fast file manager written in Rust, originally for macOS, built on Zed's
[GPUI](https://github.com/zed-industries/zed) plus
[longbridge/gpui-component](https://github.com/longbridge/gpui-component) for
higher-level primitives (sidebar, title bar, settings, virtualized table,
context menu). The macOS app ships virtualized file listings, magic-first
format detection, Finder tags, Quick Look previews, an async disk-usage window,
multi-window + tabs, favorites with persistence, drag-and-drop, and a command
catalogue surfaced through keyboard, menu, and Cmd+K palette. A Windows port is
in progress ([windows-port.md](windows-port.md)).

GPUI itself runs on Linux — Zed ships a Linux build on the same backend, over
**both Wayland and X11**, with a Vulkan renderer. So the rendering and
windowing foundation already exists at the rev we pin; the work is the
*platform shell* (the OS integration layer), not the graphics.

The **prime directive** from [ARCHITECTURE.md](../ARCHITECTURE.md): the UI must
never stop. Rendering, hover, hit-testing, scroll, resize, keyboard, text
input, and modal drawing are read-only and non-blocking. Anything blocking
(filesystem, SQLite, AppKit/Win32/D-Bus shell calls, magic, previews) runs off
the UI thread and reports back through GPUI's entity-update boundaries. This
applies on Linux just as strictly — and D-Bus round-trips (portals, udisks2)
are exactly the kind of latency that must never touch the hot path.

There is no Linux predecessor to port *from* (unlike Windows, which has
a private predecessor checkout). The reference is the macOS shell crate's *shape* plus the
freedesktop specifications.

---

## 2. State of the Linux port

**The compile scaffold has landed, plus a first batch of real shell impls.**
`feraille-shell-linux` exists, the
`#[cfg(target_os = "linux")] pub use feraille_shell_linux as platform_shell`
arm is wired, and the crate is in the workspace — so the *surface* the Linux
build needs is in place. The deterministic, verifiable functions are now
implemented for real (real arm under `cfg(target_os = "linux")`, no-op twin
under `cfg(not)`):

- **Pure `std` / process-based:** `duplicate_path`, `make_alias`,
  `make_alias_in`, `open_url`, `reveal_in_finder` (D-Bus `FileManager1` →
  `xdg-open` fallback), `open_terminal` (emulator detection chain),
  `system_is_dark` (gsettings v1), `open_with_app` (`gio launch` / exec).
- **`compress_paths`** — `.zip` via the `zip` crate, lifted verbatim from the
  (platform-neutral) win32 impl. Because it's not target-gated, its tests
  **run on any host** (`cargo test -p feraille-shell-linux` passes on macOS).
- **`prevent_idle_sleep`** — RAII `SleepBlocker` owning a `systemd-inhibit
  --what=idle` child; dropping it releases the lock.
- **File-type icons** (`feraille_fs_native::fetch_icon_rgba`, not a shell fn) —
  shared-mime-info MIME detection (`xdg-mime`) → freedesktop icon-theme lookup
  (`freedesktop-icons`, GTK theme via gsettings, hicolor cascade) → PNG/SVG
  rasterization (`image` / `resvg`, straight RGBA). Cached per-kind one level up
  in `IconCache`, never on the render path. Verified in WSL2 by dumping real
  theme glyphs to PNG (`examples/icon_dump.rs`) + a smoke test.

**Deliberately still stubbed — these need a running Linux desktop to get right,
not just to compile** (so they're the first things to do *on* a Linux box, not
blind from a Mac):

- **Clipboard file-URLs** — correct paste interop needs multiple MIME targets
  (`text/uri-list` *and* GNOME's `x-special/gnome-copied-files`), which a
  single `wl-copy`/`xclip` shell-out can't offer; wants a native multi-type
  clipboard client. Wrong target = silent no-op on paste.
- ~~**`fetch_quick_look_thumbnail`**~~ — **done.** Rides the shared freedesktop
  thumbnail cache (`$XDG_CACHE_HOME/thumbnails/{normal,large,x-large,xx-large}/
  <md5(file-uri)>.png`), regenerating with `gdk-pixbuf-thumbnailer` on a miss or
  stale entry (source newer than the cached PNG). The MD5 keying is locked to
  the freedesktop spec vector by a unit test, so we hit the same cache Nautilus
  writes. v1 covers gdk-pixbuf-loadable formats (images); video/PDF thumbnails
  need their own thumbnailers (totem/evince) or the Tumbler D-Bus dispatcher —
  a follow-up. Verified in WSL2: generated a real 256×256 thumbnail and it
  populated the shared cache (`examples/thumb_dump.rs`).
- **Theme / volume / power observers** — async D-Bus signal subscriptions
  (`ashpd`/`zbus`) bridged into the sync callback API; signal match rules and
  the runtime-thread bridge need live testing.
- **`eject_volume`, trash, `open_with_candidates`, video** — udisks2 /
  freedesktop-trash / MIME-association parsing / GStreamer respectively.

> **Verify Linux code from any host.** `cargo check` doesn't link, so the real
> arms type-check (and the Linux-gated unit tests compile) on a Mac/Windows box:
> `cargo check --target x86_64-unknown-linux-gnu -p feraille-shell-linux --tests`
> (after `rustup target add x86_64-unknown-linux-gnu`). This catches API/typing
> mistakes long before you reach a Linux machine — but it does **not** run the
> code or exercise D-Bus/`xdg-open`; that still needs a real Linux session (§7).

Beyond the shell crate, the architecture means a surprising amount already works
or compiles:

**Already free / cross-platform** (no Linux-specific code needed):

- `feraille-core`, `feraille-disk-usage`, `feraille-design` — zero platform
  deps; compile and behave identically.
- `feraille-meta` — SQLite via `rusqlite` with the `bundled` feature compiles
  from source with any C compiler; works on Linux out of the box (you just need
  `cc`/`gcc`).
- `feraille-fs-native` — **already compiles on Linux.** Several functions have a
  `cfg(not(any(target_os = "macos", windows)))` catch-all arm giving generic
  Unix behavior (e.g. `entry_is_hidden` → dot-prefix). `std::fs` enumeration,
  the magic-detection table, the disk-usage scanner, and the cross-platform
  `file_ops` copy/move engine are all platform-neutral.
- Path handling: `/` is the native separator, so the Windows-specific
  separator/long-path headaches simply don't exist here.
- Keybindings: gpui maps the `"cmd-X"` bind string to **Ctrl** on Linux (Zed
  convention), same as Windows — the catalogue's `Shortcut::primary` should land
  as Ctrl automatically.

**Not yet done — your work ahead** (rough priority order):

1. **Make it compile.** *Scaffold done* — the `platform_shell` Linux arm and a
   stub `feraille-shell-linux` exist (§4). Two unguarded direct
   `feraille_shell_mac::` call sites still bypass the alias and **will fail the
   Linux build** until routed through `platform_shell` or cfg-gated:
   `entry_info.rs` `set_hidden_extension` (≈L480) and `toggle_tag` (≈L488).
   (`search.rs`'s `spotlight_available` is already `cfg(target_os = "macos")`-
   guarded — fine.)
2. **Make it run.** Confirm GPUI's Linux backend renders the shell under your
   session (Wayland or X11). You need working Vulkan drivers — this is the most
   common first-run failure on Linux.
3. **Fill in real shell implementations** (§6) — clipboard, trash, reveal,
   open-with, dark-mode, volume enumeration. Each is small and isolated.
4. **Conditionalize macOS assumptions** in `feraille-gpui` that compile but
   behave wrong on Linux (§5).
5. **Add Linux-native UX:** freedesktop Trash, `user.xdg.origin.url` as the
   "where did this download come from" provenance signal, `.desktop`-based
   Open With, XDG thumbnail cache reuse.

The multi-window + tabs spec
([feraille-windows-instances-tabs-spec.md](feraille-windows-instances-tabs-spec.md))
is OS-agnostic — it'll work on Linux once the platform layer is caught up.

---

## 3. Workspace map

```text
crates/
├── feraille-core           Domain types, command catalogue, NodeId/FileEntry,
│                           NodeStore. Zero platform deps.
├── feraille-design         Shared visual constants. `TextTokens` is now the
│                           live type scale (see ARCHITECTURE Typography).
├── feraille-disk-usage     Pure disk-usage model + treemap layout. No I/O.
├── feraille-fs-native      std::fs backend + icons, magic detection,
│                           disk-usage scanner, xattr. Already has generic
│                           cfg(not(any(macos, windows))) arms — specialize
│                           them to cfg(target_os = "linux") for real impls.
├── feraille-meta           SQLite-backed metadata store. rusqlite bundled.
├── feraille-shell-mac      macOS platform shell — AppKit/Cocoa/NSWorkspace.
├── feraille-shell-win32    Windows platform shell — Win32 via `windows` 0.58.
│                           (Smaller, newer crate — the best scaffold to copy.)
├── feraille-shell-linux    ⟵ DOES NOT EXIST YET. You create this. Freedesktop
│                           / D-Bus / XDG integration. Your home base.
└── feraille-gpui           The app. Views, actions, tasks, sidebar, file
                            list, preview, tabs, multi-window. All platform
                            coupling goes through `platform_shell::*`.
```

**Where to put new Linux code:**

| Kind of work | Crate | File pattern |
|---|---|---|
| Native shell API (clipboard, trash, dark-mode, reveal, open-with) | `feraille-shell-linux` (new) | New `src/<topic>.rs`, re-exported via `lib.rs` |
| Filesystem (icons, volumes, trash, provenance xattr) | `feraille-fs-native` | Specialize the `cfg(not(any(target_os = "macos", windows)))` arm, or add a `cfg(target_os = "linux")` arm next to it |
| UI / view tree changes | `feraille-gpui` | Same file you'd touch on Mac; gate platform diffs with `cfg`, or call into `platform_shell` |
| Domain logic | `feraille-core` | Stays platform-agnostic; if you reach for `cfg` here, reconsider |
| SQLite schema for Linux-specific state | `feraille-meta` | New table or extended one |

---

## 4. The `platform_shell` indirection — and the one change that unblocks Linux

gpui never calls `feraille_shell_mac::` or `feraille_shell_win32::` directly (or
*shouldn't* — see the two stragglers in §2 and §5). It calls
`crate::platform_shell::X`. The alias is in
[crates/feraille-gpui/src/lib.rs](../../crates/feraille-gpui/src/lib.rs) and
**now has all three arms** (the Linux one landed with the stub scaffold):

```rust
#[cfg(target_os = "macos")]
pub use feraille_shell_mac as platform_shell;
#[cfg(windows)]
pub use feraille_shell_win32 as platform_shell;
#[cfg(target_os = "linux")]
pub use feraille_shell_linux as platform_shell;
```

This was step one of the port and **it's done**. For the record, the wiring is:

- [`crates/feraille-shell-linux`](../../crates/feraille-shell-linux) — the new
  crate. Every function gpui reaches through the alias is present as a no-op /
  empty stub, with the canonical macOS signature. `lib.rs`'s module doc is the
  authoritative surface inventory; §6 below maps each entry to its real Linux
  mechanism.
- [`crates/feraille-gpui/Cargo.toml`](../../crates/feraille-gpui/Cargo.toml) —
  `[target.'cfg(target_os = "linux")'.dependencies] feraille-shell-linux` so the
  dep is pulled only on Linux (mirroring the mac/win32 target stanzas).
- Root [`Cargo.toml`](../../Cargo.toml) — added to `members` and
  `[workspace.dependencies]`.

The crate is **plain stubs, not yet cfg-split**: because no body does real work,
a single set of bodies compiles on every host. When you write the first real
impl, put it behind `#[cfg(target_os = "linux")]` and keep a
`#[cfg(not(target_os = "linux"))]` no-op twin so the crate still compiles on
Mac/Windows as a workspace member (the pattern win32/mac already use).

**The shell-crate pattern** (mirror it in shell-linux): every function has a
real arm under `cfg(target_os = "linux")` and a no-op arm under
`cfg(not(target_os = "linux"))`. The no-op arm exists purely so the crate
compiles on Mac/Windows as a workspace member — it's never reached through the
alias, because cargo only links the matching shell crate per target. Shared
types (`OpenWithCandidate`, `SetIconResult`, …) are declared unconditionally
with identical shape in all shell crates so they round-trip through the alias.

**Adding any new shell surface** now means touching **three** crates (mac, win32,
linux) — real-or-stub in each — so the alias keeps compiling on every target.

---

## 5. macOS assumptions in `feraille-gpui` that need cfg-gating for Linux

Cases where the code compiles for Linux (the stubs cover the call surface) but
the *behavior* is wrong. Each needs a `#[cfg]` arm or a small redesign. Several
overlap with the Windows port — where the desired behavior is the same, gate on
`cfg(not(target_os = "macos"))` so Linux and Windows share an arm; where they
differ, be explicit with `cfg(target_os = "linux")`.

| Site | macOS behavior | Linux behavior wanted |
|---|---|---|
| [main.rs](../../crates/feraille-gpui/src/main.rs) `run_gui` | Stays resident with zero windows (Finder model). | **Quit when the last window closes.** Re-add the `on_window_closed → cx.quit()` handler (shared with Windows under `cfg(not(target_os = "macos"))`). |
| [main.rs](../../crates/feraille-gpui/src/main.rs) menu install | `install_app_menu(cx)` installs an NSApp menu bar. | No global menu bar on Linux. Use an in-window title-bar hamburger (same call as Windows) — drop the global menu. (Unity/appmenu exists but isn't worth targeting for v1.) |
| [main.rs](../../crates/feraille-gpui/src/main.rs) titlebar options | Reserves the macOS traffic-light area on the left. | Linux uses client-side decorations with min/max/close on the **right** (GNOME, KDE). Need a CSD-appropriate title-bar layout — check what `gpui-component` already offers, and respect the compositor's preference where possible. |
| Action labels "Reveal in Finder" / "Move to Trash" ([feraille-core/src/commands.rs](../../crates/feraille-core/src/commands.rs)) | Finder / Trash literal. | "Move to Trash" is fine (freedesktop calls it Trash). "Reveal in Finder" → **"Open Containing Folder"** / "Show in Files". Swap by cfg in the catalogue or a localized lookup. |
| Spacebar → Quick Look | Pops the Quick Look panel. | No Quick Look on Linux. Spacebar should pop the in-app preview pane (already wired) or no-op. (GNOME's `sushi` is an optional shell-out, not a dependency.) |
| Sidebar **Volumes** | `feraille-fs-native::list_volumes()` reads `/Volumes` / `mount`. | Enumerate from `/proc/self/mountinfo` (filter to user-meaningful mounts) or udisks2 / `GVolumeMonitor` for removable media + labels. Needs a Linux arm for `list_volumes`. |
| Trash (`MoveToTrash`) | NSFileManager trash via shell-mac. | **freedesktop Trash spec** — `$XDG_DATA_HOME/Trash/{files,info}` plus per-volume `.Trash-<uid>` with `.trashinfo` records. The `trash` crate implements this; sits in `feraille-fs-native` next to the existing trash arms. |
| Quarantine indicator (red shield) | `com.apple.quarantine` xattr ([feraille-fs-native/src/xattr_info.rs](../../crates/feraille-fs-native/src/xattr_info.rs)). | No exact equivalent. The closest is the freedesktop download-provenance xattr `user.xdg.origin.url` (+ `user.xdg.referrer.url`) that browsers set. Add a Linux arm to `fetch_quarantine_info`; relabel the UI as "downloaded from" provenance. |
| Finder tags (color chips) | `com.apple.metadata:_kMDItemUserTags`. | **No portable Linux tag system.** Drop for v1 (recommended) or back via `feraille-meta` SQLite as private tags (no file-manager interop). |
| Path separators in display strings | `/` (already native). | `/` is native on Linux too — *easier* than Windows. Still double-check no display string assumes a macOS-only convention. |
| `--screenshot` headless harness | macOS `render_to_image` + icon install in `screenshot::run`. | Verify gpui's Linux backend supports `render_to_image` at our pinned rev. If not, you'll need an on-screen capture path like the Windows `capture_window_rgba` route. The icon-install call already routes through `platform_shell::set_app_icon_from_png_bytes` (make it a no-op on Linux — see §6). |

---

## 6. The Linux shell surface

`feraille-shell-linux` must mirror the public `pub fn` surface of
`feraille-shell-mac` (and `-win32`). Below is each function mapped to the Linux
mechanism. **Strong recommendation:** lean on the
[`ashpd`](https://crates.io/crates/ashpd) crate for XDG Desktop Portals
(appearance/dark-mode, open-uri, file-chooser) and
[`zbus`](https://crates.io/crates/zbus) for raw D-Bus (udisks2, FileManager1) —
both async, both the idiomatic Rust path. Run portal/D-Bus calls **off the UI
thread** (prime directive).

### Recommended first three (each unblocks a working macOS feature)

| Function | Linux approach |
|---|---|
| `clipboard_copy_file_urls` / `clipboard_read_file_urls` | Clipboard with a `text/uri-list` target carrying `file://` URIs. GNOME apps additionally use the `x-special/gnome-copied-files` target (`copy\nfile:///path\n…`) and KDE its own — write/read the common `text/uri-list` first, add the GNOME target for Nautilus interop. Wayland: `smithay-clipboard` or `wl-clipboard` shell-out; X11: raw selections or `xclip`. (Plain text `copy_to_clipboard` can use [`arboard`](https://crates.io/crates/arboard), but it doesn't do custom MIME targets well — file-URLs need lower-level access.) |
| `start_volume_observer` | udisks2 `InterfacesAdded`/`InterfacesRemoved` D-Bus signals on `org.freedesktop.UDisks2` (via `zbus`), or `GVolumeMonitor` mount/unmount signals, or an inotify watch on `/proc/self/mountinfo`. Mirror the macOS callback contract. |
| *(text-naming prompt — no Linux work needed)* | Already solved cross-platform: the shared `open_text_prompt` gpui modal in `feraille-gpui` handles rename + new-folder on every platform, and the native `prompt_for_text` shell stub was deleted (2026-06-20). Nothing to port here. |

### The rest

| Function | Linux approach |
|---|---|
| `show_alert(title, body)` | A gpui modal (preferred, cross-platform), or `zenity --info` shell-out for a quick stub. |
| `copy_to_clipboard(text)` | `arboard` (handles Wayland + X11). |
| `open_url(url)` | `xdg-open <url>`, or portal `org.freedesktop.portal.OpenURI` via `ashpd`. |
| `reveal_in_finder(path)` | D-Bus `org.freedesktop.FileManager1.ShowItems` (works with Nautilus/Dolphin/Nemo/Files), fallback `xdg-open <parent dir>`. |
| `open_terminal(dir)` | No standard. Detection chain: `$TERMINAL`, then `x-terminal-emulator` (Debian alternatives), then probe known emulators (gnome-terminal, konsole, kgx, alacritty, xterm). Document the order. |
| `duplicate_path(src)` | Pure `std::fs` (lift the Windows impl almost verbatim; just use a Linux-ish copy-name convention, e.g. `<stem> (copy).<ext>`). |
| `make_alias(target)` | `std::os::unix::fs::symlink` — trivial. (A `.desktop` launcher is the heavier alternative; a symlink is the right default.) |
| `compress_paths(targets)` | The `zip` crate — already cross-platform; reuse the shared logic. |
| `open_with_candidates(path)` / `open_with_app` | Resolve MIME via `xdg-mime query filetype` (or GIO), then enumerate handler `.desktop` files from the MIME associations (`mimeapps.list` + `applications/`). The `gio` CLI (`gio open`, `gio mime`) is the pragmatic shortcut; the proper path parses desktop entries (consider a `freedesktop`/`xdg` crate). |
| `fetch_quick_look_thumbnail(path)` | **Reuse the freedesktop thumbnail cache first:** check `$XDG_CACHE_HOME/thumbnails/{normal,large}/<md5(canonical file:// URI)>.png`. On miss, generate (gdk-pixbuf for images, ffmpeg for video) or ask the `org.freedesktop.thumbnails.Thumbnailer1` (Tumbler) D-Bus service. Images-only via gdk-pixbuf is a fine v1. |
| `show_quick_look(paths)` | No Quick Look. Route to the in-app preview pane, or `gio open` / GNOME `sushi` as an optional shell-out. |
| `read_canonical_tags` / `toggle_tag` / `clear_tags` | No portable native tags. Back via `feraille-meta` SQLite (private), or drop for v1. |
| `system_is_dark()` | Portal `org.freedesktop.portal.Settings.ReadOne("org.freedesktop.appearance", "color-scheme")` (1 = prefer-dark) via `ashpd::desktop::settings`. Fallback: `gsettings get org.gnome.desktop.interface color-scheme`. |
| `start_system_theme_observer(cb)` | Subscribe to the portal `SettingChanged` signal for `org.freedesktop.appearance` / `color-scheme` (`ashpd` exposes a stream); fire the callback off-thread. |
| `set_app_icon_from_png_bytes` | **No-op.** On Linux app identity/icon comes from a `.desktop` file + the Wayland `app_id` / X11 `WM_CLASS`, not a runtime swap. Ship a `feraille.desktop` and an icon in the hicolor theme instead. |
| `set_app_user_model_id(id)` | **No-op** (Windows taskbar concept). The Wayland `app_id` plays the grouping role; ensure gpui sets it to match the `.desktop` file's basename. |
| `app_bundle_path()` | `None` (no bundle concept), or the install prefix if ever useful. |
| `video_overlay_show` / `set_frame` / `remove` | GStreamer (`gstreamer` crate) or libmpv child surface floated over the viewer stage rect. Viewer-feature-sized; defer. See VIEWER.md. |
| `install_app_menu`, `register_command_callback`, `set_tab_count`, `set_command_state`, `set_about_options` / `show_about_panel` | NSApp main-menu paradigm — no Linux global menu. Title-bar hamburger covers about/settings; make these no-ops for v1. |

### Not reached through the alias (winit-window-taking surface)

`begin_drag`, `show_context_menu`, `install_services_anchor`,
`set_services_selection`, `show_share_picker`, `apply_native_chrome` take a
`&winit::window::Window` and aren't called through `platform_shell` (gpui
doesn't hand out winit handles). Drag-out-to-file-manager and a share action
will grow fresh functions when needed — drag-and-drop via the Wayland/X11 DND
protocols (gpui may already mediate this), and "share" via the portal
`org.freedesktop.portal.OpenURI` / email handler. Don't try to recycle these
macOS signatures.

---

## 7. What "Linux" means here, and where to do the work

Two questions that come up before you've even cloned the repo: *which Linux do I
target?* and *do I even need a Linux machine to start?*

### "Linux" is not a distribution choice

Unlike macOS (one Finder, one NSWorkspace) or Windows (one Win32 shell), there
is **no single "Linux shell"** to port to — but the flip side is you **don't
pick a distribution** at the source level either. The binary targets the Linux
kernel ABI + glibc (or musl), and everything in §6 targets **freedesktop.org
standards**, not a distro:

- The *display server* — **Wayland vs X11** — is abstracted by GPUI (it supports
  both). You don't choose; you support both.
- The *desktop environment* — **GNOME vs KDE vs others** — is what actually
  varies, and you abstract it via **D-Bus portals** (`org.freedesktop.portal.*`)
  and freedesktop specs (Trash, MIME-apps, thumbnails, icon theme). That's the
  whole point of §6: write to the spec, not to GNOME or KDE specifically.
- A *distribution* (Ubuntu, Fedora, Arch, …) only matters as a **dev/test/CI
  target** — which box you run and screenshot on, which `-dev` packages and
  Vulkan drivers you install (§8). It is not a source-level fork. Pick **Ubuntu
  LTS or Fedora** as the primary test target because they're what the Vulkan
  drivers and `gpui` deps are best exercised against; nothing in the code
  branches on it.

So: target *freedesktop*, test on *a couple of mainstream distros*, support
*both display servers*. There is no `cfg(distro = …)`.

### You don't need to be on Linux to start

A large fraction of the port is host-agnostic Rust that compiles on any
machine. Three realistic dev hosts, in rough order of how much of the loop they
cover:

| Host | Build / `cargo check` / `cargo test` | Write shell-linux + fs-native arms | Run the GPU UI + screenshot |
|---|---|---|---|
| **Native Linux** (or a Linux VM) | ✅ | ✅ | ✅ (needs working Vulkan — §8) |
| **WSL2 on Windows** | ✅ genuine Linux kernel | ✅ | ⚠️ via WSLg; hardware Vulkan (Dozen) is experimental — falls back to **lavapipe** software Vulkan, which is fine for `--screenshot` but not smooth interactive use |
| **macOS** (this repo's home) | ⚠️ only the *workspace-member* check below; cannot cross-compile `feraille-gpui` to Linux (Apple→Linux cross with the gpui native/Vulkan deps is impractical) | ✅ | ❌ — no Linux runtime; use a VM/remote box |

**What "the workspace-member check" means on a Mac:** `feraille-shell-linux` and
the `cfg(target_os = "linux")` arms in `feraille-fs-native` are plain Rust with
no Linux-only deps, so they compile *as workspace members* on macOS
(`cargo check -p feraille-shell-linux` is green today). That lets you write and
type-check the entire stub surface and much of the real shell logic from the
Mac. What you **cannot** do from the Mac is build `feraille-gpui` *for*
`target_os = "linux"` (its GPUI/Vulkan/Wayland deps don't cross-compile from
Darwin) or run anything — so the moment you need to see pixels or exercise a
real portal call, move to a Linux host.

**Recommended split if you're starting from a Windows or Mac box:**

1. Do the mechanical, type-checkable work anywhere: flesh out
   `feraille-shell-linux`, add `fs-native` Linux arms, keep
   `cargo check -p feraille-shell-linux` green.
2. Stand up **one** real Linux environment for the run/screenshot loop — a
   **WSL2** instance (easiest if you're on Windows; `vulkaninfo` to see whether
   Dozen gives you a GPU, else set lavapipe for screenshots) or a **Linux VM /
   cloud box** (best for smooth interactive testing). Push early so CI on the
   other platforms stays honest (§9).

> Naming caveat: CLAUDE.md's porting rule "WSL features are not macOS v1
> features" is about WSL as a *feature to support in the old Ferail* (browsing
> `\\wsl$` paths), **not** about using WSL2 as a build host for this port. The
> two are unrelated; don't conflate them.

---

## 8. Day-one steps on a Linux machine

Assumes a recent distro with a working graphical session. GPUI needs a
**Vulkan** driver — this is the single most common first-run blocker.

```sh
# 1. Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable

# 2. System build deps — Debian/Ubuntu
sudo apt install -y build-essential pkg-config cmake \
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  xorg-dev libxcb1-dev \
  libvulkan1 mesa-vulkan-drivers vulkan-tools \
  libssl-dev libzstd-dev libdbus-1-dev
#   (rusqlite `bundled` compiles SQLite from source — build-essential covers it.)
#   Fedora equivalents: gcc gcc-c++ make cmake pkgconf-pkg-config
#     wayland-devel libxkbcommon-devel libxkbcommon-x11-devel
#     libX11-devel libxcb-devel vulkan-loader mesa-vulkan-drivers
#     vulkan-tools openssl-devel libzstd-devel dbus-devel
#   Authoritative list drifts with the gpui rev — cross-check Zed's
#   script/linux in the zed-industries/zed repo we pin.

# 3. Confirm Vulkan actually works (blank window / crash on launch == this)
vulkaninfo | head -n 20      # should print a device, not an error

# 4. Clone and build
git clone <your-feraille-remote> Feraille && cd Feraille
cargo check --workspace --all-targets   # expect a platform_shell error first — see §4
```

**What'll fail first, in rough order:**

1. **`platform_shell` is undefined on Linux.** Compile error in `feraille-gpui`.
   This is expected — fix it via §4 (create `feraille-shell-linux`, or the
   temporary win32-stub alias to see pixels) before anything else.
2. **No Vulkan device → blank window or panic on launch.** Install
   `mesa-vulkan-drivers` (or vendor drivers); verify with `vulkaninfo`. In a VM,
   ensure 3D acceleration / a software Vulkan (lavapipe) is available.
3. **Wayland vs X11 quirks.** gpui supports both; if one session misbehaves,
   force the other to isolate the issue (e.g. run under an X11 session, or set
   the relevant backend env var) and file what you find.
4. **gpui-component visual oddities.** Title-bar height, CSD button placement,
   sidebar borders — tested primarily on macOS. Screenshot and triage by element.
5. **Stay-resident-at-zero-windows is wrong on Linux.** Closing the last window
   leaves the process running invisibly — first behavioral bug to fix (§5 row 1).
6. **Sidebar Volumes empty/wrong, Trash a no-op.** Until you write the Linux
   `list_volumes` arm and trash impl (§5, §6).

**Verification commands:**

```sh
cargo check --workspace --all-targets          # sanity at any point
cargo run --bin feraille-gpui                   # run the shell
cargo run --bin feraille-gpui -- --screenshot screenshots/linux-baseline.png
cargo test --workspace
```

**Convention reminders:**

- Screenshots go in the repo `screenshots/` folder, not `/tmp`.
- Never run broad formatters (`cargo fmt --all`); the repo often has
  work-in-progress local changes.
- New native code in `feraille-shell-linux`; new filesystem code as
  `cfg(target_os = "linux")` arms in `feraille-fs-native`. Don't put D-Bus/XDG
  calls in `feraille-gpui` directly — go through `platform_shell`.

---

## 9. Working on Linux without breaking Mac/Windows

There are now **three** platforms. macOS is the established one and Windows is
mid-port; your job from Linux is to add the Linux side **without regressing the
two you can't see**.

**You can't cross-compile to macOS from Linux** (Apple toolchain is
Apple-hardware-only), and cross-compiling to Windows MSVC from Linux is fiddly.
So **CI or a teammate's machine is the gate** for Mac/Windows correctness. Push
early; treat those signals as load-bearing.

**Every new `platform_shell::X` surface now needs all three shell crates.** Add
the real arm under `cfg(target_os = "linux")` in `feraille-shell-linux`, and a
matching signature (real-or-stub) in `feraille-shell-mac` and
`feraille-shell-win32`. The alias only resolves when every shell crate exposes
the symbol.

**Mind the `cfg` traps — they're sharper with three platforms:**

- `cfg(unix)` catches **both macOS and Linux** — never use it for a macOS-only
  or Linux-only branch. Use `cfg(target_os = "macos")` / `cfg(target_os =
  "linux")`.
- `cfg(not(windows))` catches **macOS and Linux**. Good when Linux and Mac want
  identical behavior; wrong when they differ.
- `cfg(not(target_os = "macos"))` catches **Windows and Linux** — handy for the
  "everything that isn't the Finder model" cases (e.g. quit-on-last-window).
- When you cfg-gate behavior in `feraille-gpui`, **write every arm you affect**
  so you're forced to think about the other platforms.

**`feraille-fs-native` already has Linux arms via `cfg(not(any(target_os =
"macos", windows)))`.** When you add real Linux behavior, decide: specialize
that catch-all to `cfg(target_os = "linux")` (if the generic fallback no longer
fits) or add a `cfg(target_os = "linux")` arm beside it. Don't silently change
the catch-all's behavior for other Unixes you haven't tested.

**The command catalogue is shared and platform-agnostic.**
`crates/feraille-core/src/commands.rs` defines actions and shortcuts for all
platforms. Gate the *handler*, not the catalogue entry, for a Linux-only action.
`Shortcut::primary("X")` maps to Cmd on Mac and Ctrl on Windows/Linux — verify a
chord doesn't collide with a compositor/WM global on your DE.

**Things you can safely change from Linux** without Mac/Windows risk:
`feraille-shell-linux`, `cfg(target_os = "linux")` arms anywhere, anything in
`feraille-core` / `feraille-disk-usage` / `feraille-design` (no platform code by
design), and gpui code where you've added matching arms for the other targets.

---

## 10. References

**Within the repo:**

- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — crate boundaries, data model,
  work-scheduling rules. Apply unchanged on Linux.
- [windows-port.md](windows-port.md) — the sister port doc; the cross-platform
  discipline and `platform_shell` mechanics are shared.
- [CLAUDE.md](../../CLAUDE.md) — operating manual for AI / human edits.
- [TODO.md](../../TODO.md) — open work backlog; Linux items will accrue here.

**Freedesktop specs (the authoritative behavior reference):**

- [Trash spec](https://specifications.freedesktop.org/trash-spec/latest/) — how
  Move-to-Trash must behave (`Trash/files`, `Trash/info`, per-volume `.Trash-<uid>`).
- [Thumbnail managing spec](https://specifications.freedesktop.org/thumbnail-spec/latest/) —
  the `~/.cache/thumbnails` cache layout and naming to reuse.
- [Desktop entry spec](https://specifications.freedesktop.org/desktop-entry-spec/latest/)
  and [MIME apps / associations](https://specifications.freedesktop.org/mime-apps-spec/latest/) —
  for Open With and the app's own `.desktop` file.
- [Icon theme spec](https://specifications.freedesktop.org/icon-theme-spec/latest/) —
  per-MIME icon lookup.
- [Base directory spec (XDG)](https://specifications.freedesktop.org/basedir-spec/latest/) —
  where config/cache/data live.

**D-Bus / portals / volumes:**

- [XDG Desktop Portals](https://flatpak.github.io/xdg-desktop-portal/docs/) —
  `Settings` (appearance/dark-mode), `OpenURI`, `FileChooser`.
- [`ashpd`](https://docs.rs/ashpd) — idiomatic Rust portals client. Start here
  for dark-mode + open-uri.
- [`zbus`](https://docs.rs/zbus) — raw D-Bus, for udisks2 and `FileManager1`.
- [udisks2](http://storaged.org/doc/udisks2-api/latest/) — drive/volume
  enumeration and add/remove signals.
- `org.freedesktop.FileManager1` — the `ShowItems` interface every major file
  manager implements (for "reveal").

**Useful Rust crates:** `ashpd` (portals), `zbus` (D-Bus), `trash` (freedesktop
trash), `arboard` (plain-text clipboard), `xattr` (already used on macOS — reuse
for provenance), and a desktop-entry/`xdg-mime` parser for Open With.

**GPUI on Linux:**

- [Zed's gpui platform backends](https://github.com/zed-industries/zed/tree/main/crates/gpui_platform/src/platform/linux) —
  the X11/Wayland code gpui uses. When in doubt about windowing/DND behavior,
  read here.
- Zed's `script/linux` (same repo) — the canonical list of system build
  dependencies for the rev we pin; cross-check it when the apt list above drifts.
