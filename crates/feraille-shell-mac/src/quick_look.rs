//! Quick Look integration.
//!
//! Two surfaces:
//!
//! - [`show`] pops a standalone Quick Look window via
//!   `/usr/bin/qlmanage -p` — same renderer as Finder's inline panel,
//!   in its own process so we don't have to thread `QLPreviewPanel`
//!   through our responder chain.
//! - [`fetch_thumbnail`] runs `qlmanage -t -s <size> -o <tmp>` on a
//!   worker thread, decodes the resulting PNG, and returns RGBA bytes
//!   for the in-app preview pane. Synchronous (waits for qlmanage to
//!   finish), so callers must run it off the UI thread.
//!
//! A future iter can replace the shell-out with `QLThumbnailGenerator`
//! via `objc2-quick-look-thumbnailing` for lower latency.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Maximum wall-clock time to give `qlmanage` before we kill it and
/// return `None`. Quick Look usually finishes in well under a second;
/// this is a backstop for the cases where it hangs (some `.zip` /
/// `.dmg` payloads, malformed media, etc.).
const QL_TIMEOUT: Duration = Duration::from_secs(8);
const QL_POLL: Duration = Duration::from_millis(100);

/// Show Quick Look for `paths`. Multiple paths render as a strip
/// the user can step through. No-op if `paths` is empty.
pub fn show(paths: &[&Path]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("/usr/bin/qlmanage");
    cmd.arg("-p");
    for p in paths {
        cmd.arg(p);
    }
    // qlmanage prints chatty status to stderr — mute it so the
    // launching terminal stays clean.
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn()
        .map_err(|e| format!("failed to spawn qlmanage: {e}"))?;
    Ok(())
}

/// RGBA8888 thumbnail for `path` at `size_px` (longest edge). Returns
/// `(rgba, width, height)` on success. Synchronous — callers run
/// this on a worker thread per the UI-nonblocking contract.
///
/// Implementation: `qlmanage -t -s <size> -o <tmpdir> <path>` writes
/// `<basename>.png` (or `<basename>.<ext>.png` for some types). We
/// decode the PNG and clean up the temp dir before returning.
pub fn fetch_thumbnail(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let tmpdir = match make_temp_dir() {
        Some(d) => d,
        None => return None,
    };
    let _guard = TempDirGuard(tmpdir.clone());

    let mut child = Command::new("/usr/bin/qlmanage")
        .arg("-t")
        .arg("-s")
        .arg(size_px.to_string())
        .arg("-o")
        .arg(&tmpdir)
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if started.elapsed() >= QL_TIMEOUT {
                    // Hung — kill, drain, and treat as failure.
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(QL_POLL);
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }

    // qlmanage names the output after the input's basename and adds
    // a `.png` extension. For "foo.txt" → "foo.txt.png"; for "Bar"
    // (no extension) → "Bar.png". Don't assume which — scan the
    // tmpdir for the first PNG we can find.
    let entry = std::fs::read_dir(&tmpdir).ok()?.flatten().find(|e| {
        e.path()
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("png"))
            .unwrap_or(false)
    })?;
    let png_bytes = std::fs::read(entry.path()).ok()?;
    decode_png_to_rgba(&png_bytes)
}

fn make_temp_dir() -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = base.join(format!("feraille-ql-{pid}-{nanos}-{n}"));
    std::fs::create_dir_all(&p).ok()?;
    Some(p)
}

struct TempDirGuard(PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn decode_png_to_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(bytes);
    // Normalize unusual PNG variants to 8-bit-per-channel before the
    // match below sees them:
    // - `STRIP_16` downsamples 16-bit-per-channel PNGs (modern macOS
    //   screenshots on wide-gamut / HDR displays sometimes ship as
    //   RGBA16) to 8-bit. Without this the raw `buf` is twice as
    //   wide, the wrong half gets read as colour data, and the
    //   preview renders as blue-cast stripes.
    // - `EXPAND` expands paletted / sub-byte grayscale to a regular
    //   8-bit-per-channel layout so the match arms below see only
    //   `Rgb`, `Rgba`, `Grayscale`, or `GrayscaleAlpha` at most.
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    // Convert to RGBA8888 if the source is something else. `png`
    // decodes most types directly; we promote the common ones.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for chunk in buf[..info.buffer_size()].chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(0xFF);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for chunk in buf[..info.buffer_size()].chunks_exact(2) {
                let g = chunk[0];
                out.extend_from_slice(&[g, g, g, chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for &g in &buf[..info.buffer_size()] {
                out.extend_from_slice(&[g, g, g, 0xFF]);
            }
            out
        }
        png::ColorType::Indexed => {
            // qlmanage rarely emits indexed PNGs; defer support.
            return None;
        }
    };
    Some((rgba, w, h))
}
