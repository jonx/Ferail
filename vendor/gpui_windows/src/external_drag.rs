use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path};

use anyhow::{Context as _, Result, bail};
use gpui::ExternalDragPayload;
use windows::{
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CoTaskMemFree, IBindCtx, IDataObject},
            Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, IDropSource},
        },
        UI::Shell::{Common::ITEMIDLIST, SHCreateDataObject, SHDoDragDrop, SHParseDisplayName},
    },
    core::PCWSTR,
};

/// An absolute Shell item ID list allocated by `SHParseDisplayName`.
struct AbsolutePidl(*mut ITEMIDLIST);

impl Drop for AbsolutePidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
    }
}

fn parse_path(path: &Path) -> Result<AbsolutePidl> {
    // Shell parsing is more reliable with a normal DOS/UNC path than with a
    // verbatim `\\?\` spelling. `dunce` only removes that prefix when doing so
    // preserves the path's meaning; it performs no filesystem access here.
    let path = dunce::simplified(path);
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    let mut pidl = std::ptr::null_mut();
    unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }
        .with_context(|| format!("Shell could not parse drag path {}", path.display()))?;

    if pidl.is_null() {
        bail!(
            "Shell returned a null PIDL for drag path {}",
            path.display()
        );
    }
    Ok(AbsolutePidl(pidl))
}

fn shell_data_object(payload: &ExternalDragPayload) -> Result<IDataObject> {
    let ExternalDragPayload::Files(paths) = payload;
    if paths.entries().is_empty() {
        bail!("cannot start an external drag without paths");
    }

    let pidls = paths
        .entries()
        .iter()
        .map(|(path, _is_dir)| parse_path(path))
        .collect::<Result<Vec<_>>>()?;
    let raw_pidls = pidls
        .iter()
        .map(|pidl| pidl.0.cast_const())
        .collect::<Vec<_>>();

    // An empty PIDL is the desktop/root. Absolute child PIDLs under that root
    // let the Shell represent selections spanning more than one directory.
    // This is the same SHCreateDataObject shape used by ShellBat/ShellN.
    let desktop = ITEMIDLIST::default();
    unsafe { SHCreateDataObject(Some(&desktop), Some(&raw_pidls), None::<&IDataObject>) }
        .context("could not create the Shell data object for external drag")
}

/// Starts Explorer-compatible outbound file dragging on the GPUI UI thread.
///
/// `SHDoDragDrop` runs OLE's modal drag loop and returns after drop or cancel.
/// Passing no custom `IDropSource` deliberately asks the Shell for its standard
/// source implementation, including Escape/button handling and native cursors.
pub(crate) fn start_external_drag(hwnd: HWND, payload: &ExternalDragPayload) -> Result<()> {
    let data = shell_data_object(payload)?;
    unsafe {
        SHDoDragDrop(
            Some(hwnd),
            &data,
            None::<&IDropSource>,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
        )
    }
    .context("Windows Shell external drag failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::SystemTime};

    use gpui::FileDragPaths;
    use windows::Win32::{
        Foundation::S_OK,
        System::{
            Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL},
            Ole::{CF_HDROP, OleInitialize, OleUninitialize},
        },
    };

    use super::*;

    struct OleGuard;

    impl Drop for OleGuard {
        fn drop(&mut self) {
            unsafe { OleUninitialize() };
        }
    }

    #[test]
    fn shell_data_object_exposes_file_drop_format() {
        // A dedicated thread guarantees a fresh STA for this COM integration
        // test, independent of how the Rust test harness schedules its pool.
        thread::spawn(|| {
            unsafe { OleInitialize(None) }.expect("initialize OLE STA");
            let _ole = OleGuard;

            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock after Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "ferail-gpui-external-drag-{}-{nonce}.tmp",
                std::process::id()
            ));
            fs::write(&path, b"drag test").expect("create drag fixture");

            let payload = ExternalDragPayload::Files(FileDragPaths::new([(path.clone(), false)]));
            let data = shell_data_object(&payload).expect("create Shell data object");
            let format = FORMATETC {
                cfFormat: CF_HDROP.0,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            assert_eq!(unsafe { data.QueryGetData(&format) }, S_OK);

            drop(data);
            fs::remove_file(path).expect("remove drag fixture");
        })
        .join()
        .expect("OLE test thread");
    }
}
