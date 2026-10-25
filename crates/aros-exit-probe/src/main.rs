// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! ExitProbe MODE: a process-exit conformance probe for the AROS runtime.
//!
//! Every mode prints `[exitprobe] MODE: main done` on stdout right before
//! the program is supposed to end. Whether the process actually ended is
//! what the caller observes (the Shell's `$RC`, `Status`): a line without
//! an exit code after it is the finding.
//!
//! - `plain`: no helper thread; `main` returns.
//! - `parked`: a helper thread blocked forever on a channel; `main` returns.
//!   (AROS: a `pthread_cond_wait`, a cancellation point.)
//! - `sleeper`: a helper thread looping on `thread::sleep`; `main` returns.
//!   (AROS: `nanosleep` is a `DoIO` on timer.device, never a cancellation
//!   point.)
//! - `thread-exit`: `main` blocks forever; a helper thread calls
//!   `std::process::exit(3)` after one second.

use std::time::Duration;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "plain".into());
    match mode.as_str() {
        "plain" => {}
        "parked" => {
            let (_tx, rx) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                let _ = rx.recv();
            });
        }
        "sleeper" => {
            std::thread::spawn(|| loop {
                std::thread::sleep(Duration::from_secs(1));
            });
        }
        "thread-exit" => {
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_secs(1));
                println!("[exitprobe] thread-exit: helper calls exit(3)");
                std::process::exit(3);
            });
            std::thread::sleep(Duration::from_millis(200));
            println!("[exitprobe] thread-exit: main parks");
            let (_tx, rx) = std::sync::mpsc::channel::<()>();
            let _ = rx.recv();
            return;
        }
        other => {
            eprintln!("ExitProbe: unknown mode {other}");
            std::process::exit(2);
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    println!("[exitprobe] {mode}: main done");
}
