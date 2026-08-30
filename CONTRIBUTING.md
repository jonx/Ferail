# Contributing to Ferail

Thanks for your interest in Ferail. This is a cross-platform native file
manager written in Rust on [GPUI](https://www.gpui.rs/). macOS is the primary
platform; Windows has broad native parity, Linux builds and runs as an early
port, and AROS is a research port: the per-platform breakdown is in the
[Current status](README.md#current-status) table. The bar for changes is
correctness, responsiveness, and respect for the architecture's one rule.

**AI-generated pull requests are welcome.** Ferail is mostly "vibe-coded"
(written largely through AI pair-programming) and contributions made the same
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

- New product work belongs in `crates/ferail-gpui`.
- Domain logic belongs in `ferail-core`, `ferail-fs-native`, `ferail-meta`,
  `ferail-disk-usage`, or `ferail-archive` whenever it can stay UI-free.
- Shared visual tokens (color, spacing, typography) belong in `ferail-design`,
  not at call sites.
- `ferail-shell-mac` owns AppKit/Cocoa integration and does not paint UI.

See the [crate boundary rules](docs/ARCHITECTURE.md#crate-boundaries) before
adding cross-crate dependencies.

## Before opening a pull request

- `cargo check` for the touched crate (or the workspace).
- `cargo test` unless the change is docs-only.
- For UI changes, attach a screenshot. Generate one headlessly:

  ```sh
  cargo run --bin ferail-gpui -- \
    --screenshot screenshots/my-change.png \
    --navigate ~/Source/Ferail --width 1400 --height 900
  ```

- Keep diffs focused. Do not run broad formatters across the tree: match the
  surrounding code's style instead.

## Building

Ferail depends on `gpui` / `gpui-component` as **git** dependencies (they
are not published to crates.io). Reproducible builds come from the committed
`Cargo.lock`. A normal `cargo build` / `cargo run --bin ferail-gpui` works
on a recent stable toolchain (see `rust-toolchain.toml`).

Setting up from a clean machine (prerequisites, per-platform system tools,
first-build expectations, troubleshooting) is covered end-to-end in
[GETTING_STARTED.md](GETTING_STARTED.md).

## License of contributions

Unless you state otherwise, any contribution you submit is licensed under the
same terms as the project: **MIT OR Apache-2.0**, at the user's option.
