// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! Ferail as an AROS command.
//!
//! A Rust `bin`: AROS's C start-up hands argc/argv to std and runs the
//! `.init_array` constructors (GPUI's action and settings registries), then
//! `main` secures the process (stack, requesters; `c/ferail_startup.c`) and
//! hands off to the shared GUI boot in `ferail_gpui::boot`, the exact path the
//! desktop `main()` takes.
//!
//! Launch with `Stack 16000000` (see `ferail.startup`); without it the
//! command re-runs itself on a 16 MB stack first.

#[cfg(target_os = "aros")]
unsafe extern "C" {
    fn ferail_aros_relaunch_if_small_stack() -> i32;
    fn ferail_aros_prepare_process();
}

/// Small allocations from user-space slabs: the C library's malloc is
/// exec's pooled allocator behind a semaphore and Forbid/Permit, and on
/// AArch64 AROS every Permit pays a supervisor check and a scheduler pass;
/// GPUI's layout makes thousands of small allocations per frame.
#[global_allocator]
static ALLOCATOR: aros_alloc::ArosAlloc = aros_alloc::ArosAlloc::new();

fn main() {
    #[cfg(target_os = "aros")]
    {
        // SAFETY: plain DOS calls on the calling process; see the C file.
        let rc = unsafe { ferail_aros_relaunch_if_small_stack() };
        if rc >= 0 {
            std::process::exit(rc);
        }
        unsafe { ferail_aros_prepare_process() };
    }
    // The relaunch parent above cannot have submitted clipboard work. The
    // actual native command owns this guard until boot::run_gui returns, so
    // no clipboard task can execute code after AROS unloads this module.
    #[cfg(target_os = "aros")]
    let _clipboard_session = ferail_gpui::platform_shell::FileClipboardSession::default();

    // Panic forensics: with `panic = "abort"` the report must reach a file
    // before the abort; the ordinary hook prints afterwards.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!(
            "thread panicked\nlocation: {}\nmessage : {}\nbacktrace:\n{}\n",
            info.location()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "<unknown>".into()),
            info.payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string payload>".into()),
            std::backtrace::Backtrace::force_capture(),
        );
        #[cfg(target_os = "aros")]
        let _ = std::fs::write(ferail_gpui::obs::aros_diag_path("ferail-panic.txt"), &msg);
        #[cfg(not(target_os = "aros"))]
        let _ = std::fs::write("ferail-panic.txt", &msg);
        previous(info);
    }));
    ferail_gpui::obs::init();
    let args = ferail_gpui::screenshot::parse_args();
    ferail_gpui::boot::run_gui(args);
}
