//! Headless disk-usage walker. Prints the top-N folders by aggregated
//! size under a given root, plus the top-N largest individual files. No
//! UI; this shares the model and worker with the GUI's Disk Usage
//! window so the two views stay in sync by construction.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use feraille_core::NodeId;
use feraille_disk_usage::{
    build_layout_node, DiskUsageFact, DiskUsageTree, NodeKind,
};
use feraille_fs_native::{NativeFs, DEFAULT_DU_BATCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BIN: &str = "disk_usage_cli";

fn print_help() {
    println!(
        "{BIN} {VERSION} — Feraille's headless disk-usage walker.

USAGE:
    {BIN} <path> [OPTIONS]

OPTIONS:
    -n, --top <N>      Show top N entries in each section (default: 20)
        --packages     Descend into macOS packages (.app, .bundle, .framework,
                       .plugin, .kext, .xcodeproj). Default treats them as
                       opaque leaves, matching Finder.
    -h, --help         Print this message and exit
    -V, --version      Print version and exit

EXAMPLES:
    {BIN} ~/Documents
    {BIN} / --top 30
    {BIN} ~/Library/Application\\ Support --packages

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
