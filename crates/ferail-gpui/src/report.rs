//! Issue reporter — bundle the diagnostics report, the activity trail, and a
//! screenshot of the current window into a `.zip` the user can attach to a bug
//! report, optionally blacking out sensitive regions of the screenshot first.
//!
//! Two layers:
//! - **Bundle core** (this is pure logic, unit-tested): composite redaction
//!   rectangles onto the screenshot, scrub the account name out of report text,
//!   and assemble the zip.
//! - **Flow** ([`open_reporter`]): capture the window, run the redaction modal
//!   if a screenshot is available, then save + reveal the bundle.
//!
//! Screenshot capture uses `Window::render_to_image` (the same path as the
//! `--screenshot` harness). On Windows that needs the gpui_windows patch, so a
//! capture failure is non-fatal — the bundle still carries diagnostics + trail.

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use image::RgbaImage;

/// A rectangle, in **screenshot pixels**, to paint solid black before bundling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redaction {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Paint each redaction rectangle solid black on the screenshot (clamped to
/// the image bounds).
pub fn apply_redactions(img: &mut RgbaImage, boxes: &[Redaction]) {
    let (iw, ih) = (img.width(), img.height());
    for b in boxes {
        let x2 = b.x.saturating_add(b.w).min(iw);
        let y2 = b.y.saturating_add(b.h).min(ih);
        for y in b.y.min(ih)..y2 {
            for x in b.x.min(iw)..x2 {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
}

/// Replace the user's home-directory prefix in `text` with a placeholder so a
/// pasted report or a path in the trail doesn't leak the account name.
pub fn redact_username(text: &str) -> String {
    let mut out = text.to_string();
    // Longest first so `%USERPROFILE%` wins over a shorter overlapping match.
    for (var, repl) in [("USERPROFILE", "%USERPROFILE%"), ("HOME", "~")] {
        if let Some(home) = std::env::var_os(var) {
            let home = home.to_string_lossy();
            if !home.is_empty() {
                out = out.replace(home.as_ref(), repl);
            }
        }
    }
    out
}

/// Encode an `RgbaImage` to PNG bytes.
fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    use image::ImageEncoder as _;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .context("encode screenshot png")?;
    Ok(png)
}

/// Build the report `.zip` in memory: a README, the diagnostics report, the
/// activity trail, the user's note, and the (already-redacted) screenshot if
/// one was captured. Report/trail/note text has the account name scrubbed.
pub fn build_bundle(screenshot: Option<&RgbaImage>, note: &str) -> Result<Vec<u8>> {
    use zip::write::SimpleFileOptions;

    // Diagnostics carry only app-owned paths (config dir, DB) — username-scrub
    // is enough. The trail carries the user's *own* folders, so it is path-
    // redacted at the source via `render_lines_sanitized`. The note is free
    // text, so it gets the best-effort path scrub on top.
    let report =
        redact_username(&crate::diagnostics::render_text(&crate::diagnostics::run_checks()));
    let trail = redact_username(&crate::trail::render_lines_sanitized().join("\n"));
    let note = crate::redact::scrub_text(&redact_username(note));

    let privacy = if crate::redact::enabled() {
        "Privacy: file and folder names have been replaced with \u{2026} \u{2014} this report \
         reveals nothing about your files, so it is safe to share. (Toggle under \
         Settings \u{203a} Diagnostics.)\n"
    } else {
        "Privacy: path redaction is OFF \u{2014} this report contains real file and folder \
         names. Turn on \u{201c}Redact file names & paths\u{201d} under Settings \u{203a} \
         Diagnostics to hide them.\n"
    };

    let readme = format!(
        "Ferail issue report\n\n\
         Contents:\n\
         \x20 diagnostics.txt     health check (storage, deps, environment)\n\
         \x20 activity-trail.txt  the most recent actions before the report\n\
         \x20 note.txt            your description of the problem\n{}\n\
         {privacy}\
         Account names in paths are replaced with ~ / %USERPROFILE%.\n\
         If the screenshot shows anything sensitive, black it out before sending.\n",
        if screenshot.is_some() {
            "\x20 screenshot.png      capture of the window when the report was made\n"
        } else {
            ""
        }
    );

    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        for (name, body) in [
            ("README.txt", readme.as_str()),
            ("diagnostics.txt", report.as_str()),
            ("activity-trail.txt", trail.as_str()),
            ("note.txt", note.as_str()),
        ] {
            zip.start_file(name, opts)
                .with_context(|| format!("zip start {name}"))?;
            zip.write_all(body.as_bytes())?;
        }

        if let Some(shot) = screenshot {
            let png = encode_png(shot)?;
            zip.start_file("screenshot.png", opts)?;
            zip.write_all(&png)?;
        }
        zip.finish()?;
    }
    Ok(cursor.into_inner())
}

/// Write a bundle to a uniquely-named file under the config dir's `reports/`
/// folder and return its path. `seq` distinguishes bundles within a session
/// (no clock dependency).
pub fn save_bundle(bytes: &[u8], seq: u32) -> Result<PathBuf> {
    let dir = crate::app_state::config_dir()
        .map(|d| d.join("reports"))
        .context("no config directory to write the report into")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("ferail-report-{}-{seq}.zip", std::process::id()));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

use std::sync::atomic::{AtomicU32, Ordering};
static REPORT_SEQ: AtomicU32 = AtomicU32::new(0);

/// Assemble + save a bundle for `screenshot`/`note`, then reveal it in the file
/// manager. Logs and returns the path (or the error) — never panics.
pub fn finish_bundle(screenshot: Option<&RgbaImage>, note: &str) -> Result<PathBuf> {
    let bytes = build_bundle(screenshot, note)?;
    let seq = REPORT_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = save_bundle(&bytes, seq)?;
    crate::log_info!(90, "issue report saved: {}", path.display());
    crate::platform_shell::reveal_in_finder(&path);
    Ok(path)
}

/// One-click issue report: capture the current window (best-effort — capture
/// is unsupported on a Windows build without the gpui_windows patch, in which
/// case the bundle simply omits the screenshot), bundle it with the diagnostics
/// report + activity trail, and reveal the resulting zip.
///
/// In-app redaction (a modal to black out regions before bundling) is the next
/// iteration; until then the bundled README asks the user to redact the PNG
/// before sending.
pub fn open_reporter(window: &mut gpui::Window) {
    let shot = window.render_to_image().ok();
    // Only the capture needs the window/UI thread. Bundle assembly is
    // zip + disk I/O and the reveal can block on a D-Bus round-trip
    // (Linux) — run them on their own thread (Prime Directive).
    let spawned = std::thread::Builder::new()
        .name("issue-report".into())
        .spawn(move || match finish_bundle(shot.as_ref(), "") {
            Ok(path) => crate::log_info!(90, "issue report ready: {}", path.display()),
            Err(e) => crate::log_warn!(90, "issue report failed: {e}"),
        })
        .is_ok();
    if !spawned {
        crate::log_warn!(90, "issue report: worker spawn failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redactions_blacken_pixels_within_bounds() {
        let mut img = RgbaImage::from_pixel(10, 10, image::Rgba([200, 100, 50, 255]));
        // A box that overruns the edge must clamp, not panic.
        apply_redactions(&mut img, &[Redaction { x: 8, y: 8, w: 100, h: 100 }]);
        assert_eq!(*img.get_pixel(9, 9), image::Rgba([0, 0, 0, 255]));
        assert_eq!(*img.get_pixel(0, 0), image::Rgba([200, 100, 50, 255]));
    }

    #[test]
    fn bundle_contains_expected_entries() {
        let shot = RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
        let bytes = build_bundle(Some(&shot), "it broke").unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        for expected in [
            "README.txt",
            "diagnostics.txt",
            "activity-trail.txt",
            "note.txt",
            "screenshot.png",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn bundle_without_screenshot_omits_png() {
        let bytes = build_bundle(None, "").unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(!names.contains(&"screenshot.png".to_string()));
        assert!(names.contains(&"diagnostics.txt".to_string()));
    }
}
