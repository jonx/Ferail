//! Headless mpv-backend probe: the diagnostic harness for video decode/render
//! bugs that don't need the GUI.
//!
//! It drives the *same* pull path the viewer uses (`backend → open →
//! copy_frame`) against any file, with no window and no clicking, so a crash
//! deep in libmpv (e.g. the rotated-video `mp_image_crop` assert) reproduces
//! here and can be captured non-interactively.
//!
//! Usage:
//!   cargo run -p ferail-video-mpv --example probe -- <video> [frames]
//!
//! Self-service debugging loop (no human in the loop):
//!   # 1. make a rotated fixture
//!   ffmpeg -f lavfi -i testsrc=duration=2:size=1280x720:rate=24 \
//!          -metadata:s:v rotate=90 -pix_fmt yuv420p /tmp/rot90.mp4
//!   # 2. build the probe, then capture any crash backtrace in batch mode
//!   cargo build -p ferail-video-mpv --example probe
//!   FERAIL_MPV_LOG=v lldb --batch -o run -k 'bt' -k 'quit' -- \
//!          target/debug/examples/probe /tmp/rot90.mp4
//!
//! `FERAIL_MPV_LOG=v` (handled in imp.rs) prints mpv's decoder/VO setup:
//! the lines that name the frame geometry, hwdec, and rotation.

use std::path::Path;
use std::time::{Duration, Instant};

use ferail_core::video::VideoEnhance;
use ferail_video_mpv::backend;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: probe <video> [frames]   (env: MPV_HOME, FERAIL_MPV_LOG=v)");
        std::process::exit(2);
    };
    // Optional: dump the first frame's raw BGRA to a file for a visual / diff
    // check (`probe <video> --raw out.bgra`). Convert with:
    //   ffmpeg -f rawvideo -pix_fmt bgra -s WxH -i out.bgra out.png
    let rest: Vec<String> = args.collect();
    let raw_out = rest
        .iter()
        .position(|a| a == "--raw")
        .and_then(|i| rest.get(i + 1))
        .cloned();
    let want: usize = rest
        .first()
        .filter(|s| !s.starts_with("--"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    // libmpv resolution: an explicit MPV_HOME wins; otherwise the loader falls
    // back to the platform's usual install paths (see resolve_lib in imp.rs).
    let hint = std::env::var("MPV_HOME").unwrap_or_default();

    let Some(backend) = backend(Path::new(&hint)) else {
        eprintln!("probe: could not load libmpv (set MPV_HOME to its dylib/dir)");
        std::process::exit(1);
    };
    let Some(mut stream) = backend.open(Path::new(&path), Box::new(|| {}), VideoEnhance::default())
    else {
        eprintln!("probe: backend declined to open {path:?}");
        std::process::exit(1);
    };

    println!("probe: opened {path:?}, pulling up to {want} frames…");
    let start = Instant::now();
    let mut got = 0usize;
    while got < want && start.elapsed() < Duration::from_secs(15) {
        if let Some((w, h, bytes)) = stream.copy_frame() {
            if got == 0 {
                let (nw, nh) = stream.natural_size();
                let (_, dur) = stream.time();
                println!(
                    "  first frame: {w}x{h} ({} bytes), natural_size={nw}x{nh}, duration={dur:.2}s",
                    bytes.len()
                );
                assert_eq!(
                    bytes.len(),
                    (w * h * 4) as usize,
                    "BGRA buffer matches dims"
                );
                if let Some(out) = &raw_out {
                    std::fs::write(out, &bytes).expect("write raw frame");
                    println!("  wrote raw BGRA {w}x{h} -> {out}");
                }
            }
            got += 1;
        } else {
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    if got == 0 {
        eprintln!("probe: FAIL, no frame within 15s");
        std::process::exit(1);
    }
    println!("probe: OK: pulled {got} frame(s) without crashing");
}
