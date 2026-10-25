// SPDX-License-Identifier: MIT
// Copyright (c) 2026 John Knipper
//! Native device qualification. Exclusively own clipboard unit 0 while running.
#[cfg(target_os = "aros")]
fn main() {
    let _clipboard_session = ferail_shell_aros::FileClipboardSession::default();
    fn exit(code: i32) -> ! {
        if let Err(error) = ferail_shell_aros::shutdown_file_clipboard() {
            eprintln!("{error}");
        }
        std::process::exit(code)
    }
    use ferail_shell_aros::{ClipboardFileOperation as Op, reserve_file_clipboard as reserve};
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
    unsafe extern "C" {
        fn fsa_clip_read_blob(data: *mut u8, capacity: u32, length: *mut u32) -> i32;
        fn fsa_clip_write_blob(data: *const u8, length: u32) -> i32;
    }
    fn check(condition: bool, stage: &str) -> Result<(), String> {
        if condition {
            println!("PASS {stage}");
            Ok(())
        } else {
            Err(stage.into())
        }
    }
    fn wait<T>(future: impl std::future::Future<Output = T>) -> T {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(std::sync::Arc::new(Wake(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(value) => return value,
                std::task::Poll::Pending => std::thread::park(),
            }
        }
    }
    if std::env::args().nth(1).as_deref() != Some("--exclusive-primary-clipboard") {
        eprintln!(
            "Requires --exclusive-primary-clipboard; temporarily replaces clipboard unit 0 and restores original raw bytes."
        );
        exit(2);
    }
    let mut original = vec![0; 2 * 1024 * 1024 + 1];
    let mut length = 0;
    let rc =
        unsafe { fsa_clip_read_blob(original.as_mut_ptr(), original.len() as u32, &mut length) };
    if rc != 0 {
        eprintln!("Cannot snapshot clipboard: {rc}; no writes attempted");
        exit(1);
    }
    original.truncate(length as usize);
    let result = (|| -> Result<(), String> {
        let items = vec![
            (
                PathBuf::from(OsString::from_vec(b"RAM:caf\xe9.txt".to_vec())),
                false,
            ),
            (
                PathBuf::from(OsString::from_vec(b"WORK:dir/nested\nname".to_vec())),
                true,
            ),
            (PathBuf::from("SYS:"), true),
        ];
        let paths = items.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
        for op in [Op::Copy, Op::Move, Op::Copy] {
            reserve()?.write(&items, op)?;
            let got = reserve()?.read()?;
            check(
                got == (paths.clone(), op),
                &format!("device raw DOS roundtrip {op:?}"),
            )?;
        }
        wait(reserve()?.begin_write(items.clone(), Op::Move)?)?;
        check(
            wait(reserve()?.begin_read()?)? == (paths.clone(), Op::Move),
            "async device worker roundtrip",
        )?;
        let invalid = vec![(PathBuf::from("relative"), false)];
        check(
            reserve()?.write(&invalid, Op::Copy).is_err(),
            "invalid Copy fails without replacing clipboard",
        )?;
        check(
            reserve()?
                .read()
                .is_err_and(|error| error.contains("Previous file clipboard write failed")),
            "Paste refuses stale Move after failed Copy",
        )?;
        reserve()?.write(&items, Op::Copy)?;
        check(
            reserve()?.read()? == (paths.clone(), Op::Copy),
            "successful write clears failure barrier",
        )?;
        // Foreign plain text must replace a previous Move, never resurrect it.
        let text = b"FORM\0\0\0\x18FTXTCHRS\0\0\0\x0bRAM:foreign\0";
        check(
            unsafe { fsa_clip_write_blob(text.as_ptr(), text.len() as u32) } == 0,
            "foreign text write",
        )?;
        check(
            reserve()?.read()? == (vec![], Op::Copy),
            "foreign text is not files",
        )?;
        let bad = b"FORM\0\0\0\x04FTXTbad";
        check(
            unsafe { fsa_clip_write_blob(bad.as_ptr(), bad.len() as u32) } == 0,
            "malformed stream write",
        )?;
        check(reserve()?.read().is_err(), "malformed stream refused")?;
        // A subsequent writer/read must still work after every error/read-end.
        reserve()?.write(&items, Op::Move)?;
        check(
            reserve()?.read()? == (paths, Op::Move),
            "device usable after malformed read",
        )?;
        Ok(())
    })();
    let rc = unsafe { fsa_clip_write_blob(original.as_ptr(), original.len() as u32) };
    if rc != 0 {
        eprintln!("FAIL restoring original clipboard bytes: {rc}");
        exit(1);
    }
    let mut restored = vec![0; 2 * 1024 * 1024 + 1];
    let mut restored_len = 0;
    let rc = unsafe {
        fsa_clip_read_blob(
            restored.as_mut_ptr(),
            restored.len() as u32,
            &mut restored_len,
        )
    };
    restored.truncate(restored_len as usize);
    if rc != 0 || restored != original {
        eprintln!("FAIL clipboard restoration verification");
        exit(1);
    }
    println!("PASS original clipboard bytes restored");
    if let Err(error) = result {
        eprintln!("FAIL {error}");
        exit(1);
    }
    if let Err(error) = ferail_shell_aros::shutdown_file_clipboard() {
        eprintln!("FAIL {error}");
        exit(1);
    }
    if reserve().is_ok() {
        eprintln!("FAIL admission after shutdown");
        exit(1);
    }
    println!("PASS worker joined and subsequent admission refused");
    println!("PASS native clipboard.device file metadata qualification");
}
#[cfg(not(target_os = "aros"))]
fn main() {
    eprintln!("This probe requires native AROS.");
    std::process::exit(2);
}
