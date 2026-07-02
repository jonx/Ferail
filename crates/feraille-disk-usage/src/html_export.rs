//! Static HTML export of a treemap — the shareable twin of the GPUI
//! Disk Usage view (docs/features/DISK_USAGE.md).
//!
//! Pure: same tree, same [`build_layout_node_with_mode`] +
//! [`compute_treemap`] pipeline the live view renders from, so the
//! exported picture matches what the user sees. The output is
//! self-contained HTML with inline styles and no JavaScript, so it can
//! be pasted into a document, a wiki, or an email as-is:
//!
//! - [`treemap_html_fragment`] — a single `<figure>` for embedding in
//!   an existing page/document (every style inline, no `<style>`).
//! - [`treemap_html_document`] — the fragment wrapped in a minimal
//!   full page, for saving as a standalone `.html` file.
//!
//! This module is also the canonical home of the category palette
//! ([`category_color_rgba`]) so the GPUI view and the export can't
//! drift apart.

use crate::file_category::FileCategory;
use crate::model::{DiskUsageTree, NodeKind, SizeMode};
use crate::{build_layout_node_with_mode, compute_treemap};
use feraille_core::NodeId;

/// Canonical category palette as `(r, g, b, a)` bytes. The GPUI view
/// converts these to its float color type; the HTML export emits them
/// as `rgba()`. One source so the two renderings agree.
pub fn category_color_rgba(cat: FileCategory) -> (u8, u8, u8, u8) {
    match cat {
        FileCategory::Image => (77, 153, 242, 217),
        FileCategory::Video => (217, 64, 115, 217),
        FileCategory::Audio => (179, 102, 217, 217),
        FileCategory::Document => (242, 191, 51, 217),
        FileCategory::Archive => (153, 128, 77, 217),
        FileCategory::Executable => (140, 140, 140, 217),
        FileCategory::Other => (166, 166, 166, 179),
    }
}

/// Human category name, shared with the view's filter chips.
pub fn category_label(cat: FileCategory) -> &'static str {
    match cat {
        FileCategory::Image => "Image",
        FileCategory::Video => "Video",
        FileCategory::Audio => "Audio",
        FileCategory::Archive => "Archive",
        FileCategory::Document => "Document",
        FileCategory::Executable => "Executable",
        FileCategory::Other => "Other",
    }
}

/// Standalone page around [`treemap_html_fragment`], for "Save as
/// HTML…". Minimal chrome: charset, title, a system font, and the
/// fragment.
pub fn treemap_html_document(
    tree: &DiskUsageTree,
    root: NodeId,
    mode: SizeMode,
    width: f32,
    height: f32,
    depth: u32,
) -> String {
    let title = tree
        .nodes
        .get(&root)
        .map(|n| n.display_name.clone())
        .unwrap_or_else(|| "Disk Usage".to_owned());
    let fragment = treemap_html_fragment(tree, root, mode, width, height, depth);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Disk Usage \u{2014} {}</title>\n</head>\n\
         <body style=\"margin:24px;background:#ffffff;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;\">\n\
         {}\n</body>\n</html>\n",
        escape_html(&title),
        fragment
    )
}

/// One self-contained `<figure>` holding the treemap: caption (root
/// name + total), the absolutely-positioned rects with name/size
/// labels and full-detail `title` tooltips, and a category legend.
/// Every style is inline so pasting into an existing document needs
/// no stylesheet.
pub fn treemap_html_fragment(
    tree: &DiskUsageTree,
    root: NodeId,
    mode: SizeMode,
    width: f32,
    height: f32,
    depth: u32,
) -> String {
    let (width, height) = (width.max(260.0).round(), height.max(220.0).round());
    let layout = build_layout_node_with_mode(tree, root, depth, mode);
    let rects = compute_treemap(&layout, (0.0, 0.0, width, height), depth);

    let root_name = tree
        .nodes
        .get(&root)
        .map(|n| n.display_name.clone())
        .unwrap_or_else(|| "Disk Usage".to_owned());
    let total = humanize_bytes(layout.size_bytes);
    let mode_label = match mode {
        SizeMode::Apparent => "apparent size",
        SizeMode::Allocated => "size on disk",
    };

    let mut out = String::with_capacity(rects.len() * 256 + 2048);
    out.push_str("<figure style=\"margin:0;display:inline-block;\">\n");
    out.push_str(&format!(
        "<figcaption style=\"font-size:14px;margin:0 0 8px 0;color:#333;\">\
         <strong>{}</strong> \u{2014} {} ({})</figcaption>\n",
        escape_html(&root_name),
        escape_html(&total),
        mode_label
    ));
    out.push_str(&format!(
        "<div style=\"position:relative;width:{width}px;height:{height}px;\
         background:#f4f4f4;border-radius:4px;overflow:hidden;\">\n"
    ));

    let mut seen_categories: Vec<FileCategory> = Vec::new();
    for r in &rects {
        if r.width < 1.0 || r.height < 1.0 {
            continue;
        }
        if !seen_categories.contains(&r.file_category) {
            seen_categories.push(r.file_category);
        }
        let (cr, cg, cb, ca) = category_color_rgba(r.file_category);
        let name = tree
            .nodes
            .get(&r.node_id)
            .map(|n| n.display_name.clone())
            .unwrap_or_default();
        let size = humanize_bytes(r.size_bytes);
        let kind = match r.kind {
            NodeKind::Container => "folder",
            NodeKind::File => category_label(r.file_category),
        };
        let tooltip = format!("{name} \u{2014} {size} \u{2014} {kind}");
        // Same visibility thresholds as the live view, so the export
        // labels exactly what the window labeled.
        let show_label = r.width >= 60.0 && r.height >= 24.0;
        let show_size = r.width >= 80.0 && r.height >= 40.0;

        out.push_str(&format!(
            "<div title=\"{}\" style=\"position:absolute;left:{}px;top:{}px;\
             width:{}px;height:{}px;background:rgba({},{},{},{:.3});\
             border:1px solid rgba(0,0,0,0.20);box-sizing:border-box;\
             overflow:hidden;\">",
            escape_html(&tooltip),
            r.x,
            r.y,
            r.width,
            r.height,
            cr,
            cg,
            cb,
            ca as f32 / 255.0,
        ));
        if show_label {
            out.push_str(&format!(
                "<div style=\"padding:2px 4px;font-size:11px;font-weight:600;\
                 color:rgba(255,255,255,0.93);white-space:nowrap;\
                 overflow:hidden;text-overflow:ellipsis;\">{}</div>",
                escape_html(&name)
            ));
            if show_size {
                out.push_str(&format!(
                    "<div style=\"padding:0 4px;font-size:11px;\
                     color:rgba(255,255,255,0.67);white-space:nowrap;\">{}</div>",
                    escape_html(&size)
                ));
            }
        }
        out.push_str("</div>\n");
    }
    out.push_str("</div>\n");

    // Legend for the categories that actually appear.
    if !seen_categories.is_empty() {
        out.push_str(
            "<div style=\"margin-top:8px;font-size:12px;color:#555;\
             display:flex;gap:14px;flex-wrap:wrap;\">\n",
        );
        for cat in &seen_categories {
            let (cr, cg, cb, _) = category_color_rgba(*cat);
            out.push_str(&format!(
                "<span style=\"display:inline-flex;align-items:center;gap:5px;\">\
                 <span style=\"display:inline-block;width:10px;height:10px;\
                 border-radius:2px;background:rgb({},{},{});\"></span>{}</span>\n",
                cr,
                cg,
                cb,
                category_label(*cat)
            ));
        }
        out.push_str("</div>\n");
    }

    out.push_str(
        "<figcaption style=\"font-size:11px;color:#999;margin-top:6px;\">\
         Generated by Feraille</figcaption>\n</figure>",
    );
    out
}

/// Minimal HTML entity escaping for text and attribute values.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn humanize_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut s = b as f64;
    let mut u = 0;
    while s >= 1024.0 && u + 1 < UNITS.len() {
        s /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", b, UNITS[u])
    } else {
        format!("{:.1} {}", s, UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiskUsageFact;

    /// root(1) { "a & b"(2): 600KB file, sub(3) { c.png(4): 400KB } }
    fn fixture() -> (DiskUsageTree, NodeId) {
        let root = NodeId::from(10u64);
        let a = NodeId::from(11u64);
        let sub = NodeId::from(12u64);
        let c = NodeId::from(13u64);
        let mut tree = DiskUsageTree::new(root);
        let facts = vec![
            DiskUsageFact::NodeDiscovered {
                node: root,
                kind: NodeKind::Container,
                file_category: FileCategory::Other,
                mtime: None,
                name: "Projects".into(),
                is_cloud: false,
            },
            DiskUsageFact::ContainerScanStarted { container: root },
            DiskUsageFact::NodeDiscovered {
                node: a,
                kind: NodeKind::File,
                file_category: FileCategory::Document,
                mtime: None,
                name: "a & b.txt".into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: root,
                node: a,
            },
            DiskUsageFact::NodeSizeAdded {
                node: a,
                size_bytes: 600 * 1024,
            },
            DiskUsageFact::NodeDiscovered {
                node: sub,
                kind: NodeKind::Container,
                file_category: FileCategory::Other,
                mtime: None,
                name: "sub".into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: root,
                node: sub,
            },
            DiskUsageFact::ContainerScanStarted { container: sub },
            DiskUsageFact::NodeDiscovered {
                node: c,
                kind: NodeKind::File,
                file_category: FileCategory::Image,
                mtime: None,
                name: "c.png".into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: sub,
                node: c,
            },
            DiskUsageFact::NodeSizeAdded {
                node: c,
                size_bytes: 400 * 1024,
            },
            DiskUsageFact::ContainerScanCompleted { container: sub },
            DiskUsageFact::ContainerScanCompleted { container: root },
        ];
        tree.apply_facts(&facts);
        (tree, root)
    }

    #[test]
    fn fragment_contains_escaped_names_sizes_and_legend() {
        let (tree, root) = fixture();
        let html = treemap_html_fragment(&tree, root, SizeMode::Apparent, 800.0, 600.0, 4);
        // Name with `&` is escaped, never raw.
        assert!(html.contains("a &amp; b.txt"));
        assert!(!html.contains("a & b.txt"));
        // Total in the caption; per-rect sizes in tooltips/labels.
        assert!(html.contains("<strong>Projects</strong>"));
        assert!(html.contains("1000.0 KB"));
        assert!(html.contains("600.0 KB"));
        assert!(html.contains("400.0 KB"));
        // Legend rows for the categories present.
        assert!(html.contains(">Document</span>"));
        assert!(html.contains(">Image</span>"));
        // Self-contained: inline styles only, no <style> or scripts.
        assert!(!html.contains("<style"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn document_wraps_fragment_with_title() {
        let (tree, root) = fixture();
        let html = treemap_html_document(&tree, root, SizeMode::Apparent, 800.0, 600.0, 4);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>Disk Usage \u{2014} Projects</title>"));
        assert!(html.contains("Generated by Feraille"));
    }

    #[test]
    fn subtree_export_roots_at_the_chosen_folder() {
        let (tree, root) = fixture();
        let sub = NodeId::from(12u64);
        let html = treemap_html_fragment(&tree, sub, SizeMode::Apparent, 800.0, 600.0, 4);
        assert!(html.contains("<strong>sub</strong>"));
        assert!(html.contains("c.png"));
        // The sibling outside the chosen subtree is absent.
        assert!(!html.contains("a &amp; b.txt"));
        let _ = root;
    }

    #[test]
    fn degenerate_sizes_are_clamped_not_panicking() {
        let (tree, root) = fixture();
        let html = treemap_html_fragment(&tree, root, SizeMode::Allocated, 0.0, 0.0, 4);
        assert!(html.contains("position:relative;width:260px;height:220px"));
    }
}
