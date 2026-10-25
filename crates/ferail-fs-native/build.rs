// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper

//! Compiles the AROS volume and archive glue when targeting AROS
//! with a native SDK named by `AROS_SDK`; see the same arrangement in
//! `ferail-shell-aros`. Nothing is compiled for any other target.

use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=c/aros_volumes.c");
    println!("cargo:rerun-if-changed=c/aros_archive.c");
    println!("cargo:rerun-if-env-changed=AROS_SDK");
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "aros" {
        return;
    }
    let Ok(sdk) = env::var("AROS_SDK") else {
        println!(
            "cargo:warning=ferail-fs-native AROS glue deferred to link time (AROS_SDK not set)"
        );
        return;
    };
    if !Path::new(&sdk).join("include/proto/dos.h").exists() {
        println!(
            "cargo:warning=ferail-fs-native AROS glue deferred to link time (proto/dos.h absent under {sdk}/include)"
        );
        return;
    }
    cc::Build::new()
        .file("c/aros_volumes.c")
        .file("c/aros_archive.c")
        .pic(false)
        .flag("-Wno-pointer-sign")
        .compile("ferail_fs_native_aros_glue");
}
