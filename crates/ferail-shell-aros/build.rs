// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! Compiles the AROS shell glue (`c/shell_glue.c`) when targeting AROS with
//! a native SDK named by `AROS_SDK` (the `Developer` directory). Without the
//! SDK a plain `cargo check` still passes; the AROS command links the glue
//! object at its final link instead. The compiler and flags come from the cc
//! crate's target-suffixed environment, set by the AROS cargo configuration
//! (scripts/aros-cargo.sh).

use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=c/shell_glue.c");
    println!("cargo:rerun-if-changed=c/file_clipboard.c");
    println!("cargo:rerun-if-env-changed=AROS_SDK");
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "aros" {
        return;
    }
    let Ok(sdk) = env::var("AROS_SDK") else {
        println!("cargo:warning=ferail-shell-aros glue deferred to link time (AROS_SDK not set)");
        return;
    };
    let include = format!("{sdk}/include");
    for probe in [
        "proto/workbench.h",
        "proto/openurl.h",
        "proto/asl.h",
        "proto/iffparse.h",
    ] {
        if !Path::new(&include).join(probe).exists() {
            println!(
                "cargo:warning=ferail-shell-aros glue deferred to link time ({probe} absent under {include})"
            );
            return;
        }
    }
    cc::Build::new()
        .file("c/shell_glue.c")
        .file("c/file_clipboard.c")
        .pic(false)
        .flag("-Wno-pointer-sign")
        .compile("ferail_shell_aros_glue");
    // openurl.library's link library is not in the driver's default set.
    println!("cargo:rustc-link-lib=openurl");
}
