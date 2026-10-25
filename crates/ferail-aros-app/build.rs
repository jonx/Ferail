// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! Compiles the process start-up glue (`c/ferail_startup.c`) when targeting
//! AROS with a native SDK named by `AROS_SDK`. See `ferail-shell-aros`.

use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=c/ferail_startup.c");
    println!("cargo:rerun-if-env-changed=AROS_SDK");
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "aros" {
        return;
    }
    let Ok(sdk) = env::var("AROS_SDK") else {
        println!(
            "cargo:warning=ferail-aros-app start-up glue deferred to link time (AROS_SDK not set)"
        );
        return;
    };
    if !Path::new(&sdk).join("include/proto/dos.h").exists() {
        println!(
            "cargo:warning=ferail-aros-app start-up glue deferred to link time (proto/dos.h absent under {sdk}/include)"
        );
        return;
    }
    cc::Build::new()
        .file("c/ferail_startup.c")
        .pic(false)
        .flag("-Wno-pointer-sign")
        .compile("ferail_aros_startup");
}
