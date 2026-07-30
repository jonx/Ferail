//! Reproduce the GUI's repeated-fetch-in-one-process bug.
//!
//! Usage:
//!     cargo run -p ferail-shell-win32 --example thumb_repeat -- <p1> [p2] [p3] ...
//!
//! Calls `fetch_quick_look_thumbnail` once per argument in the same
//! process and reports whether each call returned a real preview.

#[cfg(windows)]
fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: thumb_repeat <p1> [p2] ...");
        std::process::exit(2);
    }
    for (i, p) in paths.iter().enumerate() {
        let path = std::path::PathBuf::from(p);
        let started = std::time::Instant::now();
        match ferail_shell_win32::fetch_quick_look_thumbnail(&path, 256) {
            Some((rgba, w, h)) => {
                let mean_rgb: f64 = {
                    let n = (w as u64) * (h as u64);
                    let sum: u64 = rgba
                        .chunks_exact(4)
                        .map(|px| px[0] as u64 + px[1] as u64 + px[2] as u64)
                        .sum();
                    sum as f64 / (n as f64 * 3.0)
                };
                println!(
                    "[{}] {}  ok ({}x{}, mean RGB {:.0}, {}ms)",
                    i,
                    path.display(),
                    w,
                    h,
                    mean_rgb,
                    started.elapsed().as_millis()
                );
            }
            None => {
                println!(
                    "[{}] {}  None ({}ms)",
                    i,
                    path.display(),
                    started.elapsed().as_millis()
                );
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows-only.");
    std::process::exit(2);
}
