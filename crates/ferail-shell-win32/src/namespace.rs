//! Pathless Windows Shell namespace provider (This PC, Recycle Bin, MTP).
//!
//! Shell enumeration runs in a disposable copy of Ferail. The GUI process
//! retains only copied absolute PIDL bytes in this tab-owned arena; no COM
//! interface or borrowed PIDL crosses an apartment or survives the request.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{Read, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferail_core::platform_namespace::{
    LocationTarget, PlatformBreadcrumb, PlatformCapabilities, PlatformItem, PlatformItemFlags,
    PlatformItemId, PlatformItemKind, PlatformListingBatch, PlatformListingRequest,
    PlatformLocation, PlatformLocationErrorKind, PlatformNamespaceProvider, PlatformProviderId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsNamespaceRoot {
    ThisPc,
    RecycleBin,
}

#[derive(Clone)]
enum ShellIdentity {
    Root(WindowsNamespaceRoot),
    AbsolutePidl(Arc<[u8]>),
}

pub struct WindowsNamespaceProvider {
    id: PlatformProviderId,
    arena: Mutex<IdentityArena>,
}

struct IdentityArena {
    entries: Vec<ShellIdentity>,
    pidl_ids: HashMap<Arc<[u8]>, PlatformItemId>,
}

impl WindowsNamespaceProvider {
    pub fn new(root: WindowsNamespaceRoot) -> (Arc<Self>, PlatformLocation) {
        static NEXT_PROVIDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT_PROVIDER.fetch_add(1, Ordering::Relaxed);
        let provider = Arc::new(Self {
            id: PlatformProviderId::new(format!("windows-namespace-{serial}")),
            arena: Mutex::new(IdentityArena {
                entries: vec![ShellIdentity::Root(root)],
                pidl_ids: HashMap::new(),
            }),
        });
        let location = PlatformLocation::new(
            provider.id.clone(),
            PlatformItemId::from_raw(1).expect("one is non-zero"),
        );
        (provider, location)
    }

    fn identity(&self, id: PlatformItemId) -> Option<ShellIdentity> {
        self.arena
            .lock()
            .ok()?
            .entries
            .get(id.as_raw().checked_sub(1)? as usize)
            .cloned()
    }

    fn retain_identity(&self, identity: Arc<[u8]>) -> Option<PlatformItemId> {
        let mut arena = self.arena.lock().ok()?;
        if let Some(id) = arena.pidl_ids.get(&identity) {
            return Some(*id);
        }
        arena
            .entries
            .push(ShellIdentity::AbsolutePidl(identity.clone()));
        let id = PlatformItemId::from_raw(arena.entries.len() as u64)?;
        arena.pidl_ids.insert(identity, id);
        Some(id)
    }
}

impl PlatformNamespaceProvider for WindowsNamespaceProvider {
    fn id(&self) -> PlatformProviderId {
        self.id.clone()
    }

    fn enumerate(
        &self,
        request: PlatformListingRequest,
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(PlatformListingBatch) -> bool,
    ) -> Result<(), PlatformLocationErrorKind> {
        let identity = self
            .identity(request.token.location().item)
            .ok_or(PlatformLocationErrorKind::NotFound)?;
        let mut batch = Vec::with_capacity(request.suggested_batch_size.min(512));
        let mut breadcrumbs = None;
        let mut failed = false;
        let batch_size = request.suggested_batch_size.clamp(1, 512);
        run_broker(identity, cancel, |event| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let record = match event {
                BrokerEvent::Label(label) => {
                    breadcrumbs = Some(vec![PlatformBreadcrumb {
                        location: request.token.location().clone(),
                        label: label.into(),
                    }]);
                    return true;
                }
                BrokerEvent::Record(record) => record,
            };
            let Some(id) = self.retain_identity(record.pidl) else {
                failed = true;
                return false;
            };
            let folder = record.attributes & ATTR_FOLDER != 0;
            let filesystem = record.path.is_some();
            let target = record.path.map_or_else(
                || LocationTarget::Platform(PlatformLocation::new(self.id.clone(), id)),
                LocationTarget::FileSystem,
            );
            let mut capabilities = PlatformCapabilities::default();
            if filesystem || folder {
                capabilities = capabilities.union(PlatformCapabilities::OPEN);
            }
            if folder && !filesystem {
                capabilities = capabilities.union(PlatformCapabilities::ENUMERATE);
            }
            let mut flags = PlatformItemFlags::default();
            if record.attributes & ATTR_HIDDEN != 0 {
                flags = flags.union(PlatformItemFlags::HIDDEN);
            }
            if record.attributes & ATTR_SYSTEM != 0 {
                flags = flags.union(PlatformItemFlags::SYSTEM);
            }
            if record.attributes & ATTR_LINK != 0 {
                flags = flags.union(PlatformItemFlags::LINK);
            }
            batch.push(PlatformItem {
                id,
                label: record.label.into(),
                kind: if record.attributes & ATTR_LINK != 0 {
                    PlatformItemKind::Link
                } else if folder {
                    PlatformItemKind::Container
                } else {
                    PlatformItemKind::File
                },
                target,
                capabilities,
                flags,
                icon_key: None,
            });
            if batch.len() >= batch_size
                && !emit(PlatformListingBatch {
                    token: request.token.clone(),
                    breadcrumbs: breadcrumbs.take(),
                    items: std::mem::take(&mut batch),
                    is_last: false,
                })
            {
                return false;
            }
            true
        })?;
        if failed {
            return Err(PlatformLocationErrorKind::Failed);
        }
        if !emit(PlatformListingBatch {
            token: request.token,
            breadcrumbs,
            items: batch,
            is_last: true,
        }) {
            return Err(PlatformLocationErrorKind::Cancelled);
        }
        Ok(())
    }
}

const BROKER_ARG: &str = "--windows-namespace-broker";
const BROKER_TIMEOUT: Duration = Duration::from_secs(10);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
const MAX_RECORD_FIELD: usize = 1024 * 1024;

const ATTR_FOLDER: u32 = 0x2000_0000;
const ATTR_HIDDEN: u32 = 0x0008_0000;
const ATTR_SYSTEM: u32 = 0x0000_1000;
const ATTR_LINK: u32 = 0x0001_0000;

struct BrokerRecord {
    attributes: u32,
    pidl: Arc<[u8]>,
    label: String,
    path: Option<PathBuf>,
}

enum BrokerEvent {
    Label(String),
    Record(BrokerRecord),
}

fn run_broker(
    identity: ShellIdentity,
    cancel: &AtomicBool,
    mut consume: impl FnMut(BrokerEvent) -> bool,
) -> Result<(), PlatformLocationErrorKind> {
    use std::os::windows::process::CommandExt as _;

    let executable = std::env::current_exe().map_err(|_| PlatformLocationErrorKind::Failed)?;
    let mut child = Command::new(executable)
        .arg(BROKER_ARG)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PlatformLocationErrorKind::Unavailable)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or(PlatformLocationErrorKind::Failed)?;
    match identity {
        ShellIdentity::Root(WindowsNamespaceRoot::ThisPc) => stdin
            .write_all(&[1])
            .map_err(|_| PlatformLocationErrorKind::Failed)?,
        ShellIdentity::Root(WindowsNamespaceRoot::RecycleBin) => stdin
            .write_all(&[2])
            .map_err(|_| PlatformLocationErrorKind::Failed)?,
        ShellIdentity::AbsolutePidl(bytes) => {
            stdin
                .write_all(&[3])
                .and_then(|_| stdin.write_all(&(bytes.len() as u32).to_le_bytes()))
                .and_then(|_| stdin.write_all(&bytes))
                .map_err(|_| PlatformLocationErrorKind::Failed)?;
        }
    }
    drop(stdin);
    let stdout = child
        .stdout
        .take()
        .ok_or(PlatformLocationErrorKind::Failed)?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(512);
    let reader = std::thread::spawn(move || read_broker_stream(stdout, sender));
    let deadline = Instant::now() + BROKER_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            drop(receiver);
            let _ = reader.join();
            return Err(PlatformLocationErrorKind::Cancelled);
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(Ok(Some(event))) => {
                if !consume(event) {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(receiver);
                    let _ = reader.join();
                    return Err(PlatformLocationErrorKind::Cancelled);
                }
            }
            Ok(Ok(None)) => {
                let status = child
                    .wait()
                    .map_err(|_| PlatformLocationErrorKind::Failed)?;
                drop(receiver);
                let reader_ok = reader.join().unwrap_or(Err(())).is_ok();
                return if status.success() && reader_ok {
                    Ok(())
                } else {
                    Err(PlatformLocationErrorKind::Failed)
                };
            }
            Ok(Err(())) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(receiver);
                let _ = reader.join();
                return Err(PlatformLocationErrorKind::Failed);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            drop(receiver);
            let _ = reader.join();
            return Err(PlatformLocationErrorKind::TimedOut);
        }
    }
}

fn read_broker_stream(
    stdout: impl Read,
    sender: std::sync::mpsc::SyncSender<Result<Option<BrokerEvent>, ()>>,
) -> Result<(), ()> {
    let mut input = std::io::BufReader::new(stdout).take((MAX_OUTPUT + 1) as u64);
    loop {
        let mut kind = [0u8; 1];
        input.read_exact(&mut kind).map_err(|_| ())?;
        let event = match kind[0] {
            0 => {
                sender.send(Ok(None)).map_err(|_| ())?;
                return Ok(());
            }
            1 => {
                let attributes = read_u32(&mut input)?;
                let pidl = Arc::from(read_field(&mut input)?);
                let label = decode_utf16(&read_field(&mut input)?).ok_or(())?;
                let path_len = read_u32(&mut input)? as usize;
                let path = if path_len == u32::MAX as usize {
                    None
                } else {
                    if path_len > MAX_RECORD_FIELD {
                        return Err(());
                    }
                    let mut bytes = vec![0u8; path_len];
                    input.read_exact(&mut bytes).map_err(|_| ())?;
                    Some(PathBuf::from(std::ffi::OsString::from_wide(
                        &bytes_to_utf16(&bytes).ok_or(())?,
                    )))
                };
                BrokerEvent::Record(BrokerRecord {
                    attributes,
                    pidl,
                    label,
                    path,
                })
            }
            2 => BrokerEvent::Label(decode_utf16(&read_field(&mut input)?).ok_or(())?),
            _ => return Err(()),
        };
        sender.send(Ok(Some(event))).map_err(|_| ())?;
    }
}

fn read_u32(input: &mut impl Read) -> Result<u32, ()> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes).map_err(|_| ())?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_field(input: &mut impl Read) -> Result<Vec<u8>, ()> {
    let len = read_u32(input)? as usize;
    if len > MAX_RECORD_FIELD {
        return Err(());
    }
    let mut bytes = vec![0u8; len];
    input.read_exact(&mut bytes).map_err(|_| ())?;
    Ok(bytes)
}

fn bytes_to_utf16(bytes: &[u8]) -> Option<Vec<u16>> {
    (bytes.len() % 2 == 0).then(|| {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    })
}

fn decode_utf16(bytes: &[u8]) -> Option<String> {
    Some(String::from_utf16_lossy(&bytes_to_utf16(bytes)?))
}

#[cfg(windows)]
use std::os::windows::ffi::OsStringExt as _;

pub fn namespace_broker_main() -> i32 {
    match namespace_broker_run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn namespace_broker_run() -> Result<(), ()> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::SystemServices::{
        SFGAO_FILESYSTEM, SFGAO_FOLDER, SFGAO_HIDDEN, SFGAO_LINK, SFGAO_SYSTEM,
    };
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, Common::ITEMIDLIST, FOLDERID_ComputerFolder, FOLDERID_RecycleBinFolder,
        IEnumShellItems, ILGetSize, IShellItem, SHCreateItemFromIDList, SHGetIDListFromObject,
        SHGetKnownFolderItem, KF_FLAG_DEFAULT, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
    };

    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|_| ())?;
    let result = (|| unsafe {
        let mut input = std::io::stdin().lock();
        let mut kind = [0u8; 1];
        input.read_exact(&mut kind).map_err(|_| ())?;
        let parent: IShellItem = match kind[0] {
            1 => SHGetKnownFolderItem(&FOLDERID_ComputerFolder, KF_FLAG_DEFAULT, HANDLE::default())
                .map_err(|_| ())?,
            2 => SHGetKnownFolderItem(
                &FOLDERID_RecycleBinFolder,
                KF_FLAG_DEFAULT,
                HANDLE::default(),
            )
            .map_err(|_| ())?,
            3 => {
                let mut len = [0u8; 4];
                input.read_exact(&mut len).map_err(|_| ())?;
                let len = u32::from_le_bytes(len) as usize;
                if len == 0 || len > MAX_RECORD_FIELD {
                    return Err(());
                }
                let mut pidl = vec![0u8; len];
                input.read_exact(&mut pidl).map_err(|_| ())?;
                SHCreateItemFromIDList(pidl.as_ptr().cast::<ITEMIDLIST>()).map_err(|_| ())?
            }
            _ => return Err(()),
        };
        let parent_label = shell_item_name(&parent, SIGDN_NORMALDISPLAY).ok_or(())?;
        let enumerator: IEnumShellItems = parent
            .BindToHandler(None, &BHID_EnumItems)
            .map_err(|_| ())?;
        let mask = SFGAO_FOLDER | SFGAO_FILESYSTEM | SFGAO_HIDDEN | SFGAO_SYSTEM | SFGAO_LINK;
        let mut output = std::io::BufWriter::new(std::io::stdout().lock());
        output.write_all(&[2]).map_err(|_| ())?;
        write_field(&mut output, &utf16_bytes(&parent_label))?;
        let mut count = 0usize;
        loop {
            let mut fetched = 0u32;
            let mut item = [None];
            enumerator
                .Next(&mut item, Some(&mut fetched))
                .map_err(|_| ())?;
            if fetched == 0 {
                break;
            }
            let item = item[0].take().ok_or(())?;
            let attributes = item.GetAttributes(mask).map_err(|_| ())?.0;
            let absolute = SHGetIDListFromObject(&item).map_err(|_| ())?;
            let size = ILGetSize(Some(absolute)) as usize;
            if size == 0 || size > MAX_RECORD_FIELD {
                CoTaskMemFree(Some(absolute.cast::<c_void>()));
                return Err(());
            }
            let pidl = std::slice::from_raw_parts(absolute.cast::<u8>(), size).to_vec();
            CoTaskMemFree(Some(absolute.cast::<c_void>()));
            let label = shell_item_name(&item, SIGDN_NORMALDISPLAY).ok_or(())?;
            let path = shell_item_name(&item, SIGDN_FILESYSPATH);
            output.write_all(&[1]).map_err(|_| ())?;
            output
                .write_all(&attributes.to_le_bytes())
                .map_err(|_| ())?;
            write_field(&mut output, &pidl)?;
            write_field(&mut output, &utf16_bytes(&label))?;
            if let Some(path) = path {
                write_field(&mut output, &utf16_bytes(&path))?;
            } else {
                output.write_all(&u32::MAX.to_le_bytes()).map_err(|_| ())?;
            }
            count += 1;
            if count > 1_000_000 {
                return Err(());
            }
            if count % 128 == 0 {
                output.flush().map_err(|_| ())?;
            }
        }
        output.write_all(&[0]).map_err(|_| ())?;
        output.flush().map_err(|_| ())
    })();
    unsafe { CoUninitialize() };
    result
}

fn write_field(output: &mut impl std::io::Write, bytes: &[u8]) -> Result<(), ()> {
    if bytes.len() > MAX_RECORD_FIELD {
        return Err(());
    }
    output
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| output.write_all(bytes))
        .map_err(|_| ())
}

fn utf16_bytes(value: &[u16]) -> Vec<u8> {
    value.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

fn shell_item_name(
    item: &windows::Win32::UI::Shell::IShellItem,
    kind: windows::Win32::UI::Shell::SIGDN,
) -> Option<Vec<u16>> {
    use windows::Win32::System::Com::CoTaskMemFree;
    let value = unsafe { item.GetDisplayName(kind) }.ok()?;
    let mut len = 0usize;
    unsafe {
        while *value.0.add(len) != 0 {
            len += 1;
            if len > MAX_RECORD_FIELD / 2 {
                CoTaskMemFree(Some(value.0.cast::<c_void>()));
                return None;
            }
        }
        let owned = std::slice::from_raw_parts(value.0, len).to_vec();
        CoTaskMemFree(Some(value.0.cast::<c_void>()));
        Some(owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_reader_rejects_truncation_and_oversized_fields() {
        assert!(read_field(&mut &1u32.to_le_bytes()[..]).is_err());
        assert!(read_field(&mut &((MAX_RECORD_FIELD + 1) as u32).to_le_bytes()[..]).is_err());
    }

    #[test]
    fn provider_identity_is_session_local_and_redacted() {
        let (left, location) = WindowsNamespaceProvider::new(WindowsNamespaceRoot::ThisPc);
        let (right, _) = WindowsNamespaceProvider::new(WindowsNamespaceRoot::ThisPc);
        assert_ne!(left.id(), right.id());
        assert!(format!("{location:?}").contains("<opaque>"));
    }
}
