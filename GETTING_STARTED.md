# Getting Started

Zero to a running Ferail. This guide assumes nothing is installed yet. If you
already build Rust projects, skip to [Build & run](#3-build--run).

> **Where Ferail runs today.** macOS is the primary, daily-driver platform.
> Windows builds and runs natively with broad parity (verified on-device).
> Linux builds and runs but is an early port. See the
> [Current status](README.md#current-status) table for the honest breakdown —
> this guide flags the per-platform steps as you go.

There is no `cargo install` and no prebuilt download: `gpui` / `gpui-component`
are **git** dependencies (not on crates.io), so Ferail is built from source.
The committed `Cargo.lock` pins every dependency, so your build matches the
maintainer's.

---

## 1. Install the prerequisites

### Rust (all platforms)

Install Rust via [rustup](https://rustup.rs):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then restart your shell (or `source "$HOME/.cargo/env"`). You **don't** need to
pick a version — the repo's [`rust-toolchain.toml`](rust-toolchain.toml) pins
the stable channel (with `clippy` + `rustfmt`), and rustup installs/selects it
automatically the first time you build inside the repo. Ferail needs a recent
stable toolchain (GPUI uses Rust edition 2024).

You also need **git** to clone the repo and to fetch the git dependencies.

### Platform build tools

- **macOS** — install the Xcode Command Line Tools (the C linker + system SDKs):

  ```sh
  xcode-select --install
  ```

  That's all the system tooling a default build needs.

- **Windows** — install the **MSVC** toolchain: Visual Studio Build Tools with
  the "Desktop development with C++" workload (or full Visual Studio), then the
  `x86_64-pc-windows-msvc` Rust target (rustup's default on Windows). See
  [docs/features/windows-port.md](docs/features/windows-port.md) for the current
  state of the Windows port.

- **Linux** — you need a C toolchain, `pkg-config`, `cmake`, and **working
  Vulkan drivers** (the single most common first-run blocker). On Ubuntu/Debian:

  ```sh
  sudo apt install -y build-essential pkg-config cmake \
    libwayland-dev libxkbcommon-dev libxcb1-dev \
    libvulkan1 mesa-vulkan-drivers vulkan-tools \
    libssl-dev libzstd-dev libdbus-1-dev
  vulkaninfo | head -n 20   # should print a device, not an error
  ```

  Fedora equivalents and the full Linux story (including WSL2) are in
  [docs/features/linux-port.md](docs/features/linux-port.md).

### Optional: libmpv (for the any-format video player)

The built-in viewer plays `mp4` / `m4v` / `mov` out of the box. The optional
**mpv** backend (any container, plus live grading and the transparent
chroma-key windows) is **off by default** and loads a **user-installed** libmpv
at runtime — Ferail never bundles it. Install it only if you want that path:

```sh
# macOS
brew install mpv
```

Then build with `--features mpv` and select mpv under **Settings → Plugins** (see
[step 6](#6-optional-enable-the-mpv-video-backend)).

---

## 2. Get the code

```sh
git clone https://github.com/jonx/Ferail.git
cd Ferail
```

---

## 3. Build & run

```sh
cargo run --bin ferail-gpui
```

The **first** build clones `gpui` / `gpui-component` from git and compiles a
large dependency tree — expect several minutes and a lot of crates. This is
normal and happens once; later builds are incremental and fast.

A window opens onto your home folder. You're running Ferail.

> **macOS note — folder access.** Run loosely like this, Ferail inherits your
> terminal's privacy identity, so reads of protected folders (Desktop /
> Documents / Downloads / removable / network) show an in-app "Access required"
> screen instead of the normal system prompt. To get the real
> *"…would like to access…"* prompts, run the signed app bundle — see
> [step 5](#5-macos-the-signed-app-bundle) and its
> [permissions notes](#macos-permissions).

---

## 4. Verify your setup

```sh
cargo test --workspace                       # the test suite should pass
cargo run --bin ferail-gpui -- --doctor    # health check: config, storage, deps
```

`--doctor` prints a storage/environment report and exits — handy if the GUI
won't start, and the same report the in-app **Settings → Diagnostics** page
shows. (Sharing a report? The Diagnostics page redacts file names and paths by
default, so it's safe to send.)

---

## 5. macOS: the signed `.app` bundle

For the real per-folder access prompts (and a normal app you can launch from
Finder), build a bundle:

```sh
scripts/bundle-mac.sh && open target/Ferail.app
```

This ad-hoc-signs the app by default. For grants that persist across rebuilds,
sign with a real identity:

```sh
CODESIGN_IDENTITY="Developer ID Application: …" scripts/bundle-mac.sh
```

### macOS permissions

macOS gates the protected folders (Desktop, Documents, Downloads, removable
volumes, network volumes, cloud providers) behind its privacy system (TCC).
The *"Ferail would like to access files in your Documents folder"* prompt
only appears when **all** of these hold:

1. the path is in one of those promptable categories (arbitrary folders are
   never promptable — they need Full Disk Access);
2. Ferail runs as a code-signed `.app` bundle with a stable identity;
3. that bundle's `Info.plist` declares the matching `NS*UsageDescription`
   string ([packaging/macos/Info.plist](packaging/macos/Info.plist)).

Caveats:

- **Stale grants from terminal runs.** Earlier `cargo run` sessions attribute
  grants/denials to *Terminal*, not Ferail. If the bundle doesn't prompt,
  clear the stale state:

  ```sh
  tccutil reset SystemPolicyRemovableVolumes me.jkn.ferail
  tccutil reset SystemPolicyDocumentsFolder  me.jkn.ferail
  ```

- **Ad-hoc signing doesn't persist.** The default ad-hoc signature changes
  every build, so macOS treats each build as a new app and re-prompts. Sign
  with a real identity (above) for grants that stick.
- **After a denial, macOS never re-prompts.** Once you click "Don't Allow",
  the only recourse is the in-app "Open Full Disk Access settings" link
  (also the path for arbitrary, non-promptable folders).

---

## 6. Optional: enable the mpv video backend

With libmpv installed ([step 1](#optional-libmpv-for-the-any-format-video-player)):

```sh
cargo run --bin ferail-gpui --features mpv
```

Then open **Settings → Plugins**, set the video player to **mpv**, and (if it
isn't auto-found) point it at your libmpv. Now the viewer plays virtually any
container and unlocks live grading + the chroma-keyed transparent video windows.

---

## 7. Command-line tools

Ferail ships a few non-GUI utilities:

```sh
cargo run --bin ferail -- magic <path>...                    # identify file types by content
cargo run --bin ferail -- du [--top N] [--packages] <path>   # disk-usage summary
cargo run --bin ferail-gpui -- --doctor                      # health check
cargo run --bin ferail-gpui -- --reset-db <scope>            # reset local metadata
```

---

## Troubleshooting

- **First build takes forever.** Expected — it's compiling `gpui` and the git
  dependency tree from source the first time. Incremental builds afterward are
  quick. Don't delete `Cargo.lock`; it pins the reproducible set.
- **macOS: folders show "Access required."** You're running the loose binary,
  which can't prompt. Build and run the bundle
  ([step 5](#5-macos-the-signed-app-bundle)).
- **Linux: blank window or a panic on launch.** Almost always missing/broken
  Vulkan. Install `mesa-vulkan-drivers` (or vendor drivers) and confirm with
  `vulkaninfo`; in a VM, enable 3D acceleration or a software Vulkan (lavapipe).
  See [docs/features/linux-port.md → Day-one steps](docs/features/linux-port.md#8-day-one-steps-on-a-linux-machine).
- **mpv video doesn't play.** Confirm you built `--features mpv`, that libmpv is
  installed, and that **Settings → Plugins** has mpv selected with a valid
  library path. `--doctor` reports whether libmpv was found.

---

## Where to go next

- [README.md](README.md) — overview, features, and platform status.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how the code is organized and
  the one rule that shapes it (the UI must never stop).
- [CONTRIBUTING.md](CONTRIBUTING.md) — the contribution workflow (AI PRs
  welcome) and the pre-PR checklist.
- [CLAUDE.md](CLAUDE.md) — the operating manual if you're driving an AI agent.
- [TODO.md](TODO.md) — what's unfinished and where to help.
