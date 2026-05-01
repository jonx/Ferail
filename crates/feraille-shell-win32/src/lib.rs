//! Windows shell integration. Compiled to a no-op on non-Windows so the
//! workspace builds end-to-end on macOS for dev mode.
//!
//! Real implementation will port `crates/ferail-win32/{shell,shell_pump,
//! drag_drop,popup_menu,menu_*,enumerate,wsl}.rs` from the predecessor
//! Ferail project. The split between this crate (Win32-glue, owns COM,
//! HWND-aware) and `feraille-fs-native` (std::fs, portable) is the
//! decoupling line.

#[cfg(windows)]
pub mod shell {
    //! Real implementation lands here. For now an empty placeholder so the
    //! crate compiles on Windows targets too.
}

#[cfg(not(windows))]
pub mod shell {
    //! No-op on non-Windows targets. Exists so that `feraille-app` can
    //! unconditionally import the crate without `cfg` everywhere.
}
