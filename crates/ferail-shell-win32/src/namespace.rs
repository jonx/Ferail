//! Pathless Windows Shell namespace provider (This PC, Recycle Bin, MTP).
//!
//! Shell enumeration runs in a disposable copy of Ferail. The GUI process
//! retains only copied absolute PIDL bytes in this tab-owned arena; no COM
//! interface or borrowed PIDL crosses an apartment or survives the request.

use std::ffi::c_void;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferail_core::platform_namespace::{
    LocationTarget, PlatformCapabilities, PlatformItem, PlatformItemFlags, PlatformItemId,
    PlatformItemKind, PlatformListingBatch, PlatformListingRequest, PlatformLocation,
    PlatformLocationErrorKind, PlatformNamespaceProvider, PlatformProviderId,
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
    arena: Mutex<Vec<ShellIdentity>>,
}

impl WindowsNamespaceProvider {
    pub fn new(root: WindowsNamespaceRoot) -> (Arc<Self>, PlatformLocation) {
        static NEXT_PROVIDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT_PROVIDER.fetch_add(1, Ordering::Relaxed);
        let provider = Arc::new(Self {
            id: PlatformProviderId::new(format!("windows-namespace-{serial}")),
            arena: Mutex::new(vec![ShellIdentity::Root(root)]),
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
            .get(id.as_raw().checked_sub(1)? as usize)
            .cloned()
    }

    fn retain_identity(&self, identity: Arc<[u8]>) -> Option<PlatformItemId> {
        let mut arena = self.arena.lock().ok()?;
        arena.push(ShellIdentity::AbsolutePidl(identity));
        PlatformItemId::from_raw(arena.len() as u64)
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
        let records = run_broker(identity, cancel)?;
        let mut batch = Vec::with_capacity(request.suggested_batch_size.min(512));
        for record in records {
            if cancel.load(Ordering::Relaxed) {
                return Err(PlatformLocationErrorKind::Cancelled);
            }
            let Some(id) = self.retain_identity(record.pidl) else {
                return Err(PlatformLocationErrorKind::Failed);
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
            if batch.len() >= request.suggested_batch_size.clamp(1, 512)
                && !emit(PlatformListingBatch {
                    token: request.token.clone(),
                    breadcrumbs: None,
                    items: std::mem::take(&mut batch),
                    is_last: false,
                })
            {
                return Err(PlatformLocationErrorKind::Cancelled);
            }
        }
        if !emit(PlatformListingBatch {
            token: request.token,
            breadcrumbs: None,
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

fn run_broker(
    identity: ShellIdentity,
    cancel: &AtomicBool,
) -> Result<Vec<BrokerRecord>, PlatformLocationErrorKind> {
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
    let mut stdout = child
        .stdout
        .take()
        .ok_or(PlatformLocationErrorKind::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + BROKER_TIMEOUT;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(PlatformLocationErrorKind::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(PlatformLocationErrorKind::TimedOut);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(PlatformLocationErrorKind::Failed);
            }
        }
    };
    let bytes = reader
        .join()
        .ok()
        .and_then(Result::ok)
        .ok_or(PlatformLocationErrorKind::Failed)?;
    if !status.success() || bytes.len() > MAX_OUTPUT {
        return Err(PlatformLocationErrorKind::Failed);
    }
    decode_records(&bytes).ok_or(PlatformLocationErrorKind::Failed)
}

fn decode_records(mut bytes: &[u8]) -> Option<Vec<BrokerRecord>> {
    let count = take_u32(&mut bytes)? as usize;
    if count > 1_000_000 {
        return None;
    }
    let mut records = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let attributes = take_u32(&mut bytes)?;
        let pidl_len = take_u32_len(&mut bytes)?;
        let pidl = take_bytes(&mut bytes, pidl_len)?;
        let label_len = take_u32_len(&mut bytes)?;
        let label = decode_utf16(take_bytes(&mut bytes, label_len)?)?;
        let path_len = take_u32(&mut bytes)?;
        let path = if path_len == u32::MAX {
            None
        } else {
            Some(PathBuf::from(std::ffi::OsString::from_wide(
                &bytes_to_utf16(take_bytes(&mut bytes, path_len as usize)?)?,
            )))
        };
        records.push(BrokerRecord {
            attributes,
            pidl: Arc::from(pidl),
            label,
            path,
        });
    }
    bytes.is_empty().then_some(records)
}

fn take_u32(bytes: &mut &[u8]) -> Option<u32> {
    let (head, tail) = bytes.split_at_checked(4)?;
    *bytes = tail;
    Some(u32::from_le_bytes(head.try_into().ok()?))
}

fn take_u32_len(bytes: &mut &[u8]) -> Option<usize> {
    let len = take_u32(bytes)? as usize;
    (len <= MAX_RECORD_FIELD).then_some(len)
}

fn take_bytes<'a>(bytes: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    let (head, tail) = bytes.split_at_checked(len)?;
    *bytes = tail;
    Some(head)
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
        let enumerator: IEnumShellItems = parent
            .BindToHandler(None, &BHID_EnumItems)
            .map_err(|_| ())?;
        let mask = SFGAO_FOLDER | SFGAO_FILESYSTEM | SFGAO_HIDDEN | SFGAO_SYSTEM | SFGAO_LINK;
        type WireRecord = (u32, Vec<u8>, Vec<u8>, Option<Vec<u8>>);
        let mut records: Vec<WireRecord> = Vec::new();
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
            records.push((
                attributes,
                pidl,
                utf16_bytes(&label),
                path.map(|p| utf16_bytes(&p)),
            ));
            if records.len() > 1_000_000 {
                return Err(());
            }
        }
        let mut output = std::io::stdout().lock();
        output
            .write_all(&(records.len() as u32).to_le_bytes())
            .map_err(|_| ())?;
        for (attributes, pidl, label, path) in records {
            output
                .write_all(&attributes.to_le_bytes())
                .map_err(|_| ())?;
            write_field(&mut output, &pidl)?;
            write_field(&mut output, &label)?;
            if let Some(path) = path {
                write_field(&mut output, &path)?;
            } else {
                output.write_all(&u32::MAX.to_le_bytes()).map_err(|_| ())?;
            }
        }
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
    fn decoder_rejects_truncation_and_oversized_fields() {
        assert!(decode_records(&1u32.to_le_bytes()).is_none());
        let mut oversized = Vec::new();
        oversized.extend(1u32.to_le_bytes());
        oversized.extend(0u32.to_le_bytes());
        oversized.extend(((MAX_RECORD_FIELD + 1) as u32).to_le_bytes());
        assert!(decode_records(&oversized).is_none());
    }

    #[test]
    fn provider_identity_is_session_local_and_redacted() {
        let (left, location) = WindowsNamespaceProvider::new(WindowsNamespaceRoot::ThisPc);
        let (right, _) = WindowsNamespaceProvider::new(WindowsNamespaceRoot::ThisPc);
        assert_ne!(left.id(), right.id());
        assert!(format!("{location:?}").contains("<opaque>"));
    }
}
