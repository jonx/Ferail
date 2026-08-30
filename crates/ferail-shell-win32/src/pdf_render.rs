//! First-page PDF rendering through `Windows.Data.Pdf`: the WinRT
//! renderer that has shipped in the box since Windows 8.1, and the one
//! Microsoft provides for exactly this job.
//!
//! Why not the shell? `IShellItemImageFactory` only yields a PDF
//! thumbnail when some third-party `IThumbnailProvider` happens to be
//! installed (Adobe's is off by default), and the `IPreviewHandler`
//! route paints Edge's whole viewer: toolbar, scrollbars, chrome:
//! into the capture, which is what made PDF thumbnails look like
//! screenshots of an application. `PdfPage::RenderToStream` draws just
//! the page, off-screen, with no window and no third-party code, so it
//! needs neither a message pump nor the preview broker.
//!
//! Runs on its own short-lived MTA thread: the callers' worker threads
//! are `COINIT_APARTMENTTHREADED` for the shell APIs, and a WinRT async
//! completion is simplest to wait on from a multithreaded apartment
//! (nothing to pump). Every WinRT operation shares one hard deadline;
//! expiry calls `IAsyncInfo::Cancel` and returns a cacheable miss. A corrupt
//! document must never strand a thumbnail worker indefinitely.
//!
//! Caller must run this off the UI thread: it opens and decodes the
//! file.

#![cfg(windows)]

use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{Interface, RuntimeType, HSTRING};
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Foundation::{AsyncStatus, IAsyncAction, IAsyncOperation};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::FileAccessMode;
use windows::Storage::Streams::{DataReader, FileRandomAccessStream, InMemoryRandomAccessStream};

/// One budget for open + parse + first-page render + stream read. Healthy
/// local PDFs finish in a fraction of this; network/corrupt inputs degrade to
/// the normal icon instead of occupying the sole latest-request slot forever.
const RENDER_DEADLINE: Duration = Duration::from_secs(5);
const STATUS_POLL: Duration = Duration::from_millis(5);

fn debug() -> bool {
    std::env::var("FERAIL_THUMB_DEBUG").is_ok()
}

/// Extension gate, case-insensitive like every other one.
pub(crate) fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Render page 1 of `path` scaled to fit a `size_px` square, aspect
/// preserved. Returns straight RGBA8 bytes plus the actual dimensions;
/// `None` for an unreadable, empty, or password-protected document.
pub(crate) fn render_first_page(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    ferail_core::path_guard::assert_off_ui_thread("pdf_render::render_first_page");
    if size_px == 0 {
        return None;
    }

    // `Windows.Data.Pdf` wants an absolute path (the CLI hands us
    // relative ones). `std::path::absolute` is lexical, no disk access.
    let abs = std::path::absolute(path).ok()?;
    let worker = std::thread::Builder::new()
        .name("ferail-pdf-render".into())
        .spawn(move || unsafe {
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED,
            };
            let co_hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            let we_initialized = co_hr.is_ok();
            let result = match render(&abs, size_px) {
                Ok(frame) => frame,
                Err(e) => {
                    if debug() {
                        eprintln!("pdf_render: {e:?}");
                    }
                    None
                }
            };
            if we_initialized {
                CoUninitialize();
            }
            result
        })
        .ok()?;
    worker.join().ok().flatten()
}

fn render(abs: &Path, size_px: u32) -> windows::core::Result<Option<(Vec<u8>, u32, u32)>> {
    let deadline = Instant::now() + RENDER_DEADLINE;
    let hpath = HSTRING::from(abs.as_os_str());
    // A plain file stream rather than `StorageFile`: the latter goes
    // through the RuntimeBroker and is noticeably slower per call.
    let Some(input) = wait_operation(
        &FileRandomAccessStream::OpenAsync(&hpath, FileAccessMode::Read)?,
        deadline,
    )?
    else {
        return Ok(None);
    };
    let Some(doc) = wait_operation(&PdfDocument::LoadFromStreamAsync(&input)?, deadline)? else {
        return Ok(None);
    };
    if doc.PageCount()? == 0 {
        return Ok(None);
    }
    let page = doc.GetPage(0)?;
    let size = page.Size()?;
    let (pw, ph) = (size.Width.max(1.0), size.Height.max(1.0));
    let scale = size_px as f32 / pw.max(ph);
    let dw = ((pw * scale).round() as u32).clamp(1, size_px);
    let dh = ((ph * scale).round() as u32).clamp(1, size_px);
    if debug() {
        eprintln!(
            "pdf_render: page 1 of {} is {pw}x{ph} pt → {dw}x{dh} px",
            doc.PageCount()?
        );
    }

    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(dw)?;
    options.SetDestinationHeight(dh)?;
    options.SetBitmapEncoderId(BitmapEncoder::PngEncoderId()?)?;

    let out = InMemoryRandomAccessStream::new()?;
    if !wait_action(
        &page.RenderWithOptionsToStreamAsync(&out, &options)?,
        deadline,
    )? {
        return Ok(None);
    }
    let len = out.Size()?;
    if len == 0 || len > u32::MAX as u64 {
        return Ok(None);
    }
    let reader = DataReader::CreateDataReader(&out.GetInputStreamAt(0)?)?;
    let load: IAsyncOperation<u32> = reader.LoadAsync(len as u32)?.cast()?;
    if wait_operation(&load, deadline)?.is_none() {
        return Ok(None);
    }
    let mut png = vec![0u8; len as usize];
    reader.ReadBytes(&mut png)?;
    Ok(decode_png(&png, size_px))
}

/// Poll instead of `IAsyncOperation::get()`: the latter waits forever and
/// offers the caller no deadline. `GetResults` is called only after Completed;
/// Error preserves the provider HRESULT and Canceled is a normal miss.
fn wait_operation<T: RuntimeType + 'static>(
    operation: &IAsyncOperation<T>,
    deadline: Instant,
) -> windows::core::Result<Option<T>> {
    loop {
        match operation.Status()? {
            AsyncStatus::Completed => return operation.GetResults().map(Some),
            AsyncStatus::Error => return operation.GetResults().map(Some),
            AsyncStatus::Canceled => return Ok(None),
            AsyncStatus::Started if Instant::now() < deadline => {
                std::thread::sleep(STATUS_POLL);
            }
            AsyncStatus::Started => {
                let _ = operation.Cancel();
                return Ok(None);
            }
            _ => {
                let _ = operation.Cancel();
                return Ok(None);
            }
        }
    }
}

fn wait_action(action: &IAsyncAction, deadline: Instant) -> windows::core::Result<bool> {
    loop {
        match action.Status()? {
            AsyncStatus::Completed => return action.GetResults().map(|()| true),
            AsyncStatus::Error => return action.GetResults().map(|()| true),
            AsyncStatus::Canceled => return Ok(false),
            AsyncStatus::Started if Instant::now() < deadline => {
                std::thread::sleep(STATUS_POLL);
            }
            AsyncStatus::Started => {
                let _ = action.Cancel();
                return Ok(false);
            }
            _ => {
                let _ = action.Cancel();
                return Ok(false);
            }
        }
    }
}

/// The renderer hands back an encoded PNG; unpack it to the RGBA8 the
/// thumbnail pipeline expects. `DestinationWidth`/`Height` are not taken
/// literally, on a 150 % display the renderer multiplies them by the
/// DPI scale (a 363×512 request came back 545×768), so fit the result
/// to `size_px` here; the extra resolution makes for a cleaner downscale.
fn decode_png(png: &[u8], size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let decoded = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let longest = w.max(h).max(1);
    let decoded = if longest > size_px {
        let scale = size_px as f64 / longest as f64;
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        decoded.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let img = decoded.into_rgba8();
    let (w, h) = (img.width(), img.height());
    Some((img.into_raw(), w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_pdf_fails_within_the_render_budget() {
        let path = std::env::temp_dir().join(format!(
            "ferail-corrupt-pdf-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, b"%PDF-1.7\nnot a valid document").unwrap();
        let started = Instant::now();
        let result = render_first_page(&path, 256);
        let _ = std::fs::remove_file(path);
        assert!(result.is_none());
        assert!(
            started.elapsed() <= RENDER_DEADLINE + Duration::from_secs(1),
            "corrupt PDF exceeded its hard render budget"
        );
    }

    #[test]
    fn zero_size_is_rejected_before_winrt() {
        assert!(render_first_page(Path::new("missing.pdf"), 0).is_none());
    }
}
