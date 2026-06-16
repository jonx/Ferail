# Contributing to Feraille

Thanks for your interest in Feraille. This is a cross-platform native file
manager written in Rust on [GPUI](https://www.gpui.rs/) — macOS and Windows
today (macOS leads; the Windows port is in progress), with Linux possibly
later. The bar for changes is correctness, responsiveness, and respect for the
architecture's one rule.

**AI-generated pull requests are welcome.** Feraille is mostly "vibe-coded" —
written largely through AI pair-programming — and contributions made the same
way are encouraged. If you're driving an agent, point it at this file,
[CLAUDE.md](CLAUDE.md), and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before
it starts. Every PR still goes through human review.

## The Prime Directive

**The UI must never stop.** Render, hit-testing, scrolling, hover, and text
input must never perform filesystem, shell, database, network, thumbnail,
preview, or magic-sniffing I/O. Expensive work is scheduled off the hot path
and dropped if the user has already moved on. See
[docs/ARCHITECTURE.md → Prime Directive](docs/ARCHITECTURE.md#prime-directive).
A change that blocks the UI thread will not be merged, however small.

## Where code goes

- New product work belongs in `crates/feraille-gpui`.
- Domain logic belongs in `feraille-core`, `feraille-fs-native`,
  `feraille-meta`, or `feraille-disk-usage` whenever it can stay UI-free.
- `feraille-shell-mac` owns AppKit/Cocoa integration and does not paint UI.
- The old soft-rendered stack under `crates/_archive/` is reference only.

See the [crate boundary rules](docs/ARCHITECTURE.md#crate-boundaries) before
adding cross-crate dependencies.

## Before opening a pull request

- `cargo check` for the touched crate (or the workspace).
- `cargo test` unless the change is docs-only.
- For UI changes, attach a screenshot. Generate one headlessly:

  ```sh
  cargo run --bin feraille-gpui -- \
    --screenshot screenshots/my-change.png \
    --navigate ~/Source/Feraille --width 1400 --height 900
  ```

- Keep diffs focused. Do not run broad formatters across the tree — match the
  surrounding code's style instead.

## Building

Feraille depends on `gpui` / `gpui-component` as **git** dependencies (they
are not published to crates.io). Reproducible builds come from the committed
`Cargo.lock`. A normal `cargo build` / `cargo run --bin feraille-gpui` works
on a recent stable toolchain (see `rust-toolchain.toml`).

## License of contributions

Unless you state otherwise, any contribution you submit is licensed under the
same terms as the project: **MIT OR Apache-2.0**, at the user's option.
