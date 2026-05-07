//! Headless disk-usage walker. Prints the top-N folders by aggregated
//! size under a given root, plus the top-N largest individual files. No
//! UI; this shares the model and worker with the GUI's Disk Usage
//! window so the two views stay in sync by construction.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use feraille_controls::TreemapColoring;
use feraille_core::NodeId;
use feraille_design::{FontWeight, Theme, Tokens};
use feraille_disk_usage::{
    build_layout_node, compute_treemap, DiskUsageFact, DiskUsageTree, NodeKind,
};
use feraille_fs_native::{NativeFs, DEFAULT_DU_BATCH};
use feraille_render::{Point, Rect, Renderer, SoftRenderer, TextStyle};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BIN: &str = "disk_usage_cli";

fn print_help() {
    println!(
        "{BIN} {VERSION} — Feraille's headless disk-usage walker.

USAGE:
    {BIN} <path> [OPTIONS]

OPTIONS:
    -n, --top <N>          Show top N entries in each section (default: 20)
        --packages         Descend into macOS packages (.app, .bundle, .framework,
                           .plugin, .kext, .xcodeproj). Default treats them as
                           opaque leaves, matching Finder.
        --png <path>       Also render a treemap PNG of the scan to <path>.
                           Useful for quick visual inspection without launching
                           the GUI.
        --width <DIPs>     PNG width (default 1400, ignored without --png).
        --height <DIPs>    PNG height (default 900, ignored without --png).
        --theme light|dark Theme for the PNG (default light).
        --du-depth <N>     Treemap recursion depth (default 4).
        --du-coloring <m>  'category' (default) or 'depth'.
    -h, --help             Print this message and exit
    -V, --version          Print version and exit

EXAMPLES:
    {BIN} ~/Documents
    {BIN} / --top 30
    {BIN} ~/Library/Application\\ Support --packages
    {BIN} ~/Source --png /tmp/source.png --width 1600 --height 1000 --theme dark

NOTES:
    Symlinks are walked via lstat() and counted as 0-byte leaves to keep
    the scan cycle-safe. Permission errors on subdirs are skipped silently;
    a partial tree is still reported.

    This CLI shares the scan/aggregate/layout pipeline used by Feraille's
    GUI Disk Usage window — the same fact stream, same DAG model, same
    squarified treemap layout. Run it on any folder you'd inspect there.

    Part of Feraille — macOS successor to the Windows Ferail file explorer.
    Both projects authored by John Knipper <code@jkn.me>."
    );
}

fn die_usage(msg: &str) -> ! {
    eprintln!("{BIN}: {msg}");
    eprintln!("Try `{BIN} --help` for more information.");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut top_n: usize = 20;
    let mut descend_packages = false;
    let mut png_path: Option<PathBuf> = None;
    let mut png_width: u32 = 1400;
    let mut png_height: u32 = 900;
    let mut png_theme: Theme = Theme::Light;
    let mut du_depth: u32 = 4;
    let mut du_coloring: TreemapColoring = TreemapColoring::Category;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--top" | "-n" => {
                let Some(raw) = args.next() else {
                    die_usage("--top requires a number");
                };
                top_n = match raw.parse() {
                    Ok(n) => n,
                    Err(_) => die_usage(&format!("--top: not a number: {raw}")),
                };
            }
            "--packages" => descend_packages = true,
            "--png" => {
                let Some(p) = args.next() else {
                    die_usage("--png requires a path");
                };
                png_path = Some(PathBuf::from(p));
            }
            "--width" => {
                png_width = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| die_usage("--width requires a positive integer"));
            }
            "--height" => {
                png_height = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| die_usage("--height requires a positive integer"));
            }
            "--theme" => {
                png_theme = match args.next().as_deref() {
                    Some("light") => Theme::Light,
                    Some("dark") => Theme::Dark,
                    other => die_usage(&format!("--theme: expected light|dark, got {:?}", other)),
                };
            }
            "--du-depth" => {
                du_depth = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| die_usage("--du-depth requires a non-negative integer"));
            }
            "--du-coloring" => {
                du_coloring = match args.next().as_deref() {
                    Some("depth") => TreemapColoring::DepthOnly,
                    Some("category") | None => TreemapColoring::Category,
                    Some(other) => die_usage(&format!("--du-coloring: expected category|depth, got {other}")),
                };
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("{BIN} {VERSION}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => die_usage(&format!("unknown option: {other}")),
            other => {
                if path.is_none() {
                    path = Some(PathBuf::from(other));
                } else {
                    die_usage(&format!("unexpected positional arg: {other}"));
                }
            }
        }
    }
    let Some(path) = path else {
        die_usage("missing <path>");
    };

    eprintln!(
        "{BIN} {VERSION} — Feraille (by John Knipper <code@jkn.me>). \
         Run with --help for usage."
    );

    let fs = NativeFs::new();
    let cancel = AtomicBool::new(false);
    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
    let root_id = fs.id_for_path(&canonical);
    let mut tree = DiskUsageTree::new(root_id);
    let mut total_facts = 0usize;
    let started = Instant::now();

    let err = fs.scan_disk_usage(
        &canonical,
        DEFAULT_DU_BATCH,
        &cancel,
        descend_packages,
        |batch| {
            total_facts += batch.len();
            tree.apply_facts(&batch);
        },
        |stats| {
            eprintln!(
                "[+{:6.2}s] {:>9} files | {:>7} dirs | {} scanned",
                started.elapsed().as_secs_f64(),
                stats.files_scanned,
                stats.dirs_scanned,
                humanize(stats.bytes_scanned)
            );
        },
    );
    tree.complete = true;

    let elapsed = started.elapsed().as_secs_f64();

    if let Some(e) = err {
        eprintln!("scan failed: {e:?}");
        std::process::exit(1);
    }

    let layout = build_layout_node(&tree, root_id, 4);
    let total = layout.size_bytes;

    println!();
    println!(
        "Scanned {} in {:.2}s — {} facts, {} total",
        canonical.display(),
        elapsed,
        total_facts,
        humanize(total)
    );
    println!();

    println!("Top {top_n} children of root by size:");
    for (i, child) in layout.children.iter().take(top_n).enumerate() {
        let pct = if total > 0 {
            (child.size_bytes as f64) * 100.0 / (total as f64)
        } else {
            0.0
        };
        let kind_marker = match child.kind {
            NodeKind::Container => "/",
            NodeKind::File => " ",
        };
        let name = display_name(&tree, child.node_id);
        println!(
            "  {:>2}. {:>10}  {:>5.1}%  {}{}",
            i + 1,
            humanize(child.size_bytes),
            pct,
            name,
            kind_marker
        );
    }

    println!();
    println!("Top {top_n} largest individual files anywhere in the tree:");
    let mut files: Vec<(NodeId, u64)> = tree
        .nodes
        .iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::File))
        .map(|(id, n)| (*id, n.size_bytes))
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    for (i, (id, size)) in files.iter().take(top_n).enumerate() {
        let name = display_name(&tree, *id);
        println!("  {:>2}. {:>10}  {}", i + 1, humanize(*size), name);
    }

    if let Some(out) = png_path {
        match render_png(
            &tree,
            &layout,
            &canonical,
            &out,
            png_width,
            png_height,
            png_theme,
            du_depth,
            du_coloring,
        ) {
            Ok(()) => eprintln!("\nwrote {}", out.display()),
            Err(e) => {
                eprintln!("\nPNG render failed: {e}");
                std::process::exit(1);
            }
        }
    }
    let _ = du_depth; // referenced via render_png; suppress unused if no --png
    let _ = du_coloring;
}

/// Read a TTF the OS provides for free, so the bin doesn't need to
/// ship a font asset. macOS uses Arial; on other platforms the
/// caller must supply their own — which the CLI doesn't currently
/// expose, but the GUI handles independently.
#[cfg(target_os = "macos")]
fn load_default_font() -> std::io::Result<Vec<u8>> {
    std::fs::read("/System/Library/Fonts/Supplemental/Arial.ttf")
}
#[cfg(target_os = "windows")]
fn load_default_font() -> std::io::Result<Vec<u8>> {
    std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf")
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn load_default_font() -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no default font path on this OS",
    ))
}

#[allow(clippy::too_many_arguments)]
fn render_png(
    tree: &DiskUsageTree,
    layout: &feraille_disk_usage::DiskUsageLayoutNode,
    root_path: &Path,
    out: &Path,
    width_dips: u32,
    height_dips: u32,
    theme: Theme,
    du_depth: u32,
    du_coloring: TreemapColoring,
) -> Result<(), String> {
    let scale = 2.0;
    let width_px = (width_dips as f32 * scale).round() as u32;
    let height_px = (height_dips as f32 * scale).round() as u32;
    let width_dips_f = width_dips as f32;
    let height_dips_f = height_dips as f32;

    let tokens = Tokens::for_theme(theme);

    let font_bytes = load_default_font().map_err(|e| format!("load font: {e}"))?;
    let mut renderer = SoftRenderer::new(width_px, height_px, scale, font_bytes);

    // Background.
    let viewport = Rect::new(0.0, 0.0, width_dips_f, height_dips_f);
    renderer.fill_rect(viewport, tokens.bg.base);

    // Header strip with path + total.
    let header_h = 28.0_f32;
    renderer.fill_rect(
        Rect::new(0.0, 0.0, width_dips_f, header_h),
        tokens.bg.layer2,
    );
    renderer.stroke_rect(
        Rect::new(0.0, header_h - 1.0, width_dips_f, 1.0),
        1.0,
        tokens.border.subtle,
    );
    let header_text = format!(
        "Disk Usage  ·  {}  ·  {}",
        root_path.display(),
        humanize(layout.size_bytes)
    );
    let style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    renderer.draw_text(
        Point::new(tokens.space.md, (header_h - tokens.text.sm) / 2.0 - 1.0),
        &header_text,
        style,
    );

    // Treemap pane.
    let pane = Rect::new(0.0, header_h, width_dips_f, height_dips_f - header_h);
    let rects = compute_treemap(
        layout,
        (pane.left(), pane.top(), pane.size.width, pane.size.height),
        du_depth,
    );
    let selected: HashSet<NodeId> = HashSet::new();
    feraille_controls::treemap::paint(
        &rects,
        pane,
        None,
        &selected,
        du_coloring,
        &tokens,
        &mut renderer,
        |id| display_name(tree, id),
    );

    write_png(out, renderer.pixels(), width_px, height_px)
}

fn write_png(
    path: &Path,
    pixels: &[u32],
    width: u32,
    height: u32,
) -> Result<(), String> {
    use std::fs::File;
    use std::io::BufWriter;
    let file = File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("write header: {e}"))?;
    let mut bytes: Vec<u8> = Vec::with_capacity(pixels.len() * 3);
    for p in pixels {
        bytes.push(((p >> 16) & 0xFF) as u8);
        bytes.push(((p >> 8) & 0xFF) as u8);
        bytes.push((p & 0xFF) as u8);
    }
    writer
        .write_image_data(&bytes)
        .map_err(|e| format!("write data: {e}"))?;
    Ok(())
}

fn display_name(tree: &DiskUsageTree, id: NodeId) -> String {
    tree.nodes
        .get(&id)
        .map(|n| {
            if n.display_name.is_empty() {
                format!("<node {}>", id.as_raw())
            } else {
                n.display_name.clone()
            }
        })
        .unwrap_or_else(|| format!("<node {}>", id.as_raw()))
}

fn humanize(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// Kept here so the bin keeps building if `DiskUsageFact` is moved or
// renamed — proves we can construct one outside the worker if needed.
#[allow(dead_code)]
fn _check_fact_export(_: DiskUsageFact) {}
