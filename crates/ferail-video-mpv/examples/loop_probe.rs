//! Headless probe for the viewer's loop contract: play a short clip,
//! expect `on_ended` at its natural end, then do what the viewer's loop
//! does (seek back + unpause) and expect `on_ended` again on the next
//! end. Exercises the `keep-open`/`eof-reached` path that
//! `MPV_EVENT_END_FILE` never covers.
//!
//! Usage: cargo run -p ferail-video-mpv --example loop_probe -- \
//!            <libmpv-path-or-hint> <video>

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferail_core::video::VideoEnhance;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(lib), Some(video)) = (args.next(), args.next()) else {
        eprintln!("usage: loop_probe <libmpv-path> <video>");
        std::process::exit(2);
    };
    let backend = ferail_video_mpv::backend(Path::new(&lib)).expect("libmpv failed to load");

    let ends = Arc::new(AtomicUsize::new(0));
    let ends_cb = ends.clone();
    let mut stream = backend
        .open(
            Path::new(&video),
            Box::new(move || {
                let n = ends_cb.fetch_add(1, Ordering::SeqCst) + 1;
                println!("[probe] on_ended fired (#{n})");
            }),
            VideoEnhance::default(),
        )
        .expect("stream failed to open");

    // The viewer's ~60 Hz poll: pull frames (which pumps mpv events) and,
    // like `on_video_ended` with the loop checkbox on, seek back to the In
    // cue and resume every time the end callback fires.
    let start = Instant::now();
    let mut handled = 0usize;
    let mut frames = 0usize;
    while start.elapsed() < Duration::from_secs(7) {
        if stream.copy_frame().is_some() {
            frames += 1;
        }
        let fired = ends.load(Ordering::SeqCst);
        if fired > handled {
            handled = fired;
            let (pos, dur) = stream.time();
            println!("[probe] looping: seek(0)+play at pos={pos:.2}/{dur:.2}");
            stream.seek(0.0);
            stream.set_paused(false);
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    let total = ends.load(Ordering::SeqCst);
    println!("[probe] {frames} frames, {total} natural ends in 7s");
    // A 2 s clip looped for 7 s must end at least twice; before the
    // eof-reached fix the count is 0 (END_FILE never fires with keep-open).
    if total >= 2 && frames > 0 {
        println!("[probe] PASS");
    } else {
        println!("[probe] FAIL");
        std::process::exit(1);
    }
}
