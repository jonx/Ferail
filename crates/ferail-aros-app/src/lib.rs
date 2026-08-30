//! Ferail as an AROS `C:` command.
//!
//! A staticlib whose C harness (`c/ferail_main.c`) owns AROS startup:
//! the rust-aros std reads argc/argv from the harness globals, so CLI
//! flags work (`C:Ferail --theme dark --width 780 --height 560`). The
//! harness calls [`ferail_aros_main`], which parses args and hands off
//! to the shared GUI boot in [`ferail_gpui::boot`]: the exact same
//! path the desktop `main()` takes.
//!
//! **Launch with `Stack 16000000`** (see `ferail.startup`): AROS shells
//! hand commands tens-of-KB stacks, GPUI needs megabytes, and in AROS's
//! single address space an overflow corrupts *other* tasks. Field-diagnosed
//! on the gpui smoke (zed-aros/crates/gpui_aros_smoke).
//!
//! Build + link + run: `crates/ferail-aros-app/link-aros.sh`, then boot
//! with `AROS_CTL_STARTUP_FILE=.../ferail.startup graft/aros-ctl run`.

/// Entry point called by the C harness after AROS startup. Returns the
/// magic on clean run-loop exit so the harness can print PASS.
#[unsafe(no_mangle)]
pub extern "C" fn ferail_aros_main() -> u32 {
    // Panic forensics: panic=abort tears the whole hosted OS down before
    // stderr reaches anything durable, so persist the report to MacRW:
    // (the host-shared volume: readable as ~/AROS/Shared on macOS)
    // before the previous hook prints and the abort fires.
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
        let _ = std::fs::write("MacRW:ferail-panic.txt", &msg);
        previous(info);
    }));
    ferail_gpui::obs::init();

    let args = ferail_gpui::screenshot::parse_args();
    ferail_gpui::boot::run_gui(args);
    0x46455241 // "FERA"
}

/// getrandom v0.3 custom backend (`getrandom_backend="custom"` in the
/// AROS target rustflags): posixc provides a host-backed arc4random_buf
/// CSPRNG. AROS-gated: on hosts without `arc4random_buf` in libc (MSVC)
/// the extern would otherwise fail the *test-harness* link even though
/// nothing calls it: the getrandom crate only references this symbol
/// under the custom-backend cfg the AROS target sets.
#[cfg(target_os = "aros")]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    unsafe extern "C" {
        fn arc4random_buf(buf: *mut core::ffi::c_void, nbytes: usize);
    }
    unsafe { arc4random_buf(dest.cast(), len) };
    Ok(())
}
