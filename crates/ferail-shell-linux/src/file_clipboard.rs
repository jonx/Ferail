use super::ClipboardFileOperation;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};
use wl_clipboard_rs::{copy, paste};

const URI_LIST: &str = "text/uri-list";
const GNOME_FILES: &str = "x-special/gnome-copied-files";
const MAX_BYTES: usize = 2 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
type Job = Box<dyn FnOnce() + Send>;

pub struct Request {
    sequence: u64,
    sender: mpsc::Sender<Job>,
}

pub fn reserve() -> Result<Request, String> {
    static WORKER: OnceLock<Result<mpsc::Sender<Job>, String>> = OnceLock::new();
    let sender = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("file-clipboard".into())
            .spawn(move || {
                for job in receiver {
                    job();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(sender)
    });
    Ok(Request {
        sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
        sender: sender.as_ref().map_err(Clone::clone)?.clone(),
    })
}

pub fn accept_result(sequence: u64) -> bool {
    SEQUENCE.load(Ordering::Relaxed) == sequence
}

impl Request {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    fn submit<T: Send + 'static>(
        self,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<impl std::future::Future<Output = Result<T, String>>, String> {
        let (sender, receiver) = async_channel::bounded(1);
        self.sender
            .send(Box::new(move || {
                let _ = sender.send_blocking(work());
            }))
            .map_err(|e| e.to_string())?;
        Ok(async move { receiver.recv().await.map_err(|e| e.to_string())? })
    }

    pub fn begin_write(
        self,
        items: Vec<(PathBuf, bool)>,
        operation: ClipboardFileOperation,
    ) -> Result<impl std::future::Future<Output = Result<(), String>>, String> {
        self.submit(move || write_files(&items, operation))
    }

    pub fn begin_read(
        self,
    ) -> Result<
        impl std::future::Future<Output = Result<(Vec<PathBuf>, ClipboardFileOperation), String>>,
        String,
    > {
        self.submit(read_files)
    }
}

fn encode(items: &[(PathBuf, bool)]) -> Result<String, String> {
    if items.is_empty() || items.len() > 20_000 {
        return Err("Invalid file clipboard item count".into());
    }
    let mut list = String::new();
    for (path, _) in items {
        let uri =
            url::Url::from_file_path(path).map_err(|_| "File clipboard requires absolute paths")?;
        list.push_str(uri.as_str());
        list.push_str("\r\n");
        if list.len() > MAX_BYTES {
            return Err("File clipboard is too large".into());
        }
    }
    Ok(list)
}

fn decode(bytes: &[u8], gnome: bool) -> Result<(Vec<PathBuf>, ClipboardFileOperation), String> {
    if bytes.len() > MAX_BYTES {
        return Err("File clipboard is too large".into());
    }
    let mut lines = std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())?
        .lines();
    let operation = if gnome {
        match lines.next() {
            Some("copy") => ClipboardFileOperation::Copy,
            Some("cut") => ClipboardFileOperation::Move,
            _ => return Err("Invalid file clipboard operation".into()),
        }
    } else {
        ClipboardFileOperation::Copy
    };
    let mut paths = Vec::new();
    for line in lines.filter(|line| !line.is_empty() && !line.starts_with('#')) {
        if line.bytes().any(|byte| byte.is_ascii_control()) {
            return Err("Invalid file clipboard URI".into());
        }
        let uri = url::Url::parse(line).map_err(|e| e.to_string())?;
        if uri.query().is_some() || uri.fragment().is_some() || uri.path().contains("%00") {
            return Err("Invalid file clipboard URI".into());
        }
        let path = uri
            .to_file_path()
            .map_err(|_| "File clipboard contains a non-local URI")?;
        if paths.len() == 20_000 {
            return Err("Too many file clipboard items".into());
        }
        paths.push(path);
    }
    Ok((paths, operation))
}

fn write_files(items: &[(PathBuf, bool)], operation: ClipboardFileOperation) -> Result<(), String> {
    let list = encode(items)?;
    let action = if operation == ClipboardFileOperation::Move {
        "cut"
    } else {
        "copy"
    };
    let gnome = format!("{action}\n{}", list.replace("\r\n", "\n"));
    let mut options = copy::Options::new();
    options.omit_additional_text_mime_types(true);
    options
        .copy_multi(vec![
            copy::MimeSource {
                source: copy::Source::Bytes(list.into_bytes().into_boxed_slice()),
                mime_type: copy::MimeType::Specific(URI_LIST.into()),
            },
            copy::MimeSource {
                source: copy::Source::Bytes(gnome.into_bytes().into_boxed_slice()),
                mime_type: copy::MimeType::Specific(GNOME_FILES.into()),
            },
        ])
        .map_err(|e| e.to_string())
}

fn read_files() -> Result<(Vec<PathBuf>, ClipboardFileOperation), String> {
    let mut contents = None;
    for mime in [GNOME_FILES, URI_LIST] {
        match paste::get_contents(
            paste::ClipboardType::Regular,
            paste::Seat::Unspecified,
            paste::MimeType::Specific(mime),
        ) {
            Ok((pipe, _)) => {
                contents = Some((pipe, mime == GNOME_FILES));
                break;
            }
            Err(paste::Error::NoMimeType | paste::Error::ClipboardEmpty) => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    let Some((mut pipe, gnome)) = contents else {
        return Ok((Vec::new(), ClipboardFileOperation::Copy));
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut fd = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, remaining.as_millis() as i32) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.to_string());
        }
        if ready == 0 || remaining.is_zero() {
            return Err("File clipboard read timed out".into());
        }
        let mut chunk = [0; 8192];
        let count = pipe.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_BYTES {
            return Err("File clipboard is too large".into());
        }
    }
    decode(&bytes, gnome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn uri_roundtrip_preserves_unresolved_paths_and_raw_names() {
        let paths = vec![
            (PathBuf::from("/tmp/link/space # %\nè"), false),
            (
                PathBuf::from(OsString::from_vec(b"/tmp/raw\xff".to_vec())),
                false,
            ),
        ];
        let list = encode(&paths).unwrap();
        assert_eq!(
            decode(list.as_bytes(), false).unwrap().0,
            paths.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>()
        );
        assert_eq!(
            decode(format!("cut\n{list}").as_bytes(), true).unwrap().1,
            ClipboardFileOperation::Move
        );
    }

    #[test]
    fn rejects_remote_or_invalid_file_lists() {
        for data in [
            "file://server/tmp/a",
            "sftp://server/tmp/a",
            "file:///tmp/%00",
            "file:///tmp/a?query",
            "file:///tmp/a\tb",
        ] {
            assert!(decode(data.as_bytes(), false).is_err(), "{data}");
        }
        assert!(decode(b"move\nfile:///tmp/a", true).is_err());
        assert!(encode(&[(PathBuf::from("relative"), false)]).is_err());
    }

    #[test]
    #[ignore = "requires an isolated Wayland clipboard"]
    fn wayland_copy_and_cut_roundtrip() {
        let items = vec![(PathBuf::from("/tmp/ferail clipboard # è"), false)];
        for operation in [ClipboardFileOperation::Copy, ClipboardFileOperation::Move] {
            write_files(&items, operation).unwrap();
            let (paths, actual) = read_files().unwrap();
            assert_eq!(paths, vec![items[0].0.clone()]);
            assert_eq!(actual, operation);
        }
        let mut options = copy::Options::new();
        options.omit_additional_text_mime_types(true);
        options
            .copy(
                copy::Source::Bytes(
                    b"# file list\r\nfile:///tmp/uri%20only\r\n"
                        .to_vec()
                        .into_boxed_slice(),
                ),
                copy::MimeType::Specific(URI_LIST.into()),
            )
            .unwrap();
        assert_eq!(
            read_files().unwrap(),
            (
                vec![PathBuf::from("/tmp/uri only")],
                ClipboardFileOperation::Copy
            )
        );
        copy::Options::new()
            .copy(
                copy::Source::Bytes(b"plain text".to_vec().into_boxed_slice()),
                copy::MimeType::Text,
            )
            .unwrap();
        assert!(read_files().unwrap().0.is_empty());
    }
}
