use std::io::Read as _;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferail_core::platform_namespace::LocationTarget;
use ferail_core::platform_properties::{
    PlatformProperties, PlatformPropertiesErrorKind, PlatformPropertiesProvider,
    PlatformPropertiesRequest, PlatformProperty, PlatformPropertySection, PlatformPropertyValue,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{E_ACCESSDENIED, E_INVALIDARG};
use windows::Win32::System::Com::StructuredStorage::PropVariantToString;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName, GPS_BESTEFFORT,
    GPS_OPENSLOWITEM, PROPERTYKEY,
};

/// Deliberate allow-list: arbitrary property handlers may expose private or
/// huge data. Ferail asks only for scalar values useful in Get Info and never
/// enumerates a handler's complete schema.
const APPROVED_PROPERTIES: &[(&str, &str, &str)] = &[
    ("System.ItemTypeText", "Windows", "Type"),
    ("System.FileOwner", "Windows", "Owner"),
    ("System.Author", "Document", "Authors"),
    ("System.Title", "Document", "Title"),
    ("System.Subject", "Document", "Subject"),
    ("System.Comment", "Document", "Comments"),
    ("System.Keywords", "Document", "Tags"),
    ("System.Rating", "Document", "Rating"),
    ("System.Media.Duration", "Media", "Duration"),
    ("System.Media.Year", "Media", "Year"),
    ("System.Photo.DateTaken", "Windows image", "Date taken"),
    (
        "System.Photo.CameraManufacturer",
        "Windows image",
        "Camera manufacturer",
    ),
    ("System.Photo.CameraModel", "Windows image", "Camera model"),
    ("System.Photo.LensModel", "Windows image", "Lens"),
    ("System.Photo.Orientation", "Windows image", "Orientation"),
];

pub struct WindowsPropertiesProvider;

impl PlatformPropertiesProvider for WindowsPropertiesProvider {
    fn read_properties(
        &self,
        request: PlatformPropertiesRequest,
        cancel: &AtomicBool,
    ) -> Result<PlatformProperties, PlatformPropertiesErrorKind> {
        if cancel.load(Ordering::Relaxed) {
            return Err(PlatformPropertiesErrorKind::Cancelled);
        }
        read_via_broker(request, cancel)
    }
}

const BROKER_ARG: &str = "--windows-properties-broker";
const BROKER_TIMEOUT: Duration = Duration::from_secs(8);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_OUTPUT: usize = 1024 * 1024;

fn read_via_broker(
    request: PlatformPropertiesRequest,
    cancel: &AtomicBool,
) -> Result<PlatformProperties, PlatformPropertiesErrorKind> {
    let LocationTarget::FileSystem(path) = request.target else {
        return Err(PlatformPropertiesErrorKind::Unsupported);
    };
    let exe = std::env::current_exe().map_err(|_| PlatformPropertiesErrorKind::Failed)?;
    let mut child = Command::new(exe)
        .arg(BROKER_ARG)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PlatformPropertiesErrorKind::Failed)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or(PlatformPropertiesErrorKind::Failed)?;
    if crate::private_wire::write_paths(&mut stdin, &[path], 1).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(PlatformPropertiesErrorKind::Failed);
    }
    drop(stdin);
    let mut stdout = child
        .stdout
        .take()
        .ok_or(PlatformPropertiesErrorKind::Failed)?;
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
            return Err(PlatformPropertiesErrorKind::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(PlatformPropertiesErrorKind::Failed);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(PlatformPropertiesErrorKind::Failed);
            }
        }
    };
    let bytes = reader
        .join()
        .ok()
        .and_then(Result::ok)
        .ok_or(PlatformPropertiesErrorKind::Failed)?;
    if !status.success() || bytes.len() > MAX_OUTPUT {
        return Err(PlatformPropertiesErrorKind::Failed);
    }
    decode_properties(&bytes).ok_or(PlatformPropertiesErrorKind::Failed)
}

pub fn properties_broker_main() -> i32 {
    let Ok(mut paths) = crate::private_wire::read_paths(&mut std::io::stdin().lock(), 1) else {
        return 2;
    };
    let Some(path) = paths.pop() else { return 2 };
    let request = PlatformPropertiesRequest {
        target: LocationTarget::FileSystem(path),
    };
    let cancel = AtomicBool::new(false);
    match read_on_sta(request, &cancel).and_then(|properties| {
        encode_properties(&properties, &mut std::io::stdout().lock())
            .map_err(|_| PlatformPropertiesErrorKind::Failed)
    }) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn encode_properties(
    properties: &PlatformProperties,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    out.write_all(&(properties.sections.len() as u32).to_le_bytes())?;
    for section in &properties.sections {
        write_text(out, &section.title)?;
        out.write_all(&(section.properties.len() as u32).to_le_bytes())?;
        for property in &section.properties {
            write_text(out, &property.canonical_key)?;
            write_text(out, &property.display_name)?;
            let PlatformPropertyValue::Text(value) = &property.value else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unsupported property wire value",
                ));
            };
            write_text(out, value)?;
        }
    }
    Ok(())
}

fn write_text(out: &mut impl std::io::Write, value: &str) -> std::io::Result<()> {
    let bytes = value.as_bytes();
    out.write_all(&(bytes.len() as u32).to_le_bytes())?;
    out.write_all(bytes)
}

fn decode_properties(mut bytes: &[u8]) -> Option<PlatformProperties> {
    let count = read_word(&mut bytes)? as usize;
    if count > 64 {
        return None;
    }
    let mut sections = Vec::with_capacity(count);
    for _ in 0..count {
        let title = read_text(&mut bytes)?;
        let property_count = read_word(&mut bytes)? as usize;
        if property_count > 128 {
            return None;
        }
        let mut properties = Vec::with_capacity(property_count);
        for _ in 0..property_count {
            properties.push(PlatformProperty {
                canonical_key: Arc::from(read_text(&mut bytes)?),
                display_name: Arc::from(read_text(&mut bytes)?),
                value: PlatformPropertyValue::Text(Arc::from(read_text(&mut bytes)?)),
            });
        }
        sections.push(PlatformPropertySection {
            title: Arc::from(title),
            properties,
        });
    }
    bytes.is_empty().then_some(PlatformProperties { sections })
}

fn read_word(bytes: &mut &[u8]) -> Option<u32> {
    let head = bytes.get(..4)?;
    *bytes = &bytes[4..];
    Some(u32::from_le_bytes(head.try_into().ok()?))
}

fn read_text(bytes: &mut &[u8]) -> Option<String> {
    let len = read_word(bytes)? as usize;
    if len > 64 * 1024 {
        return None;
    }
    let value = std::str::from_utf8(bytes.get(..len)?).ok()?.to_owned();
    *bytes = &bytes[len..];
    Some(value)
}

fn read_on_sta(
    request: PlatformPropertiesRequest,
    cancel: &AtomicBool,
) -> Result<PlatformProperties, PlatformPropertiesErrorKind> {
    let LocationTarget::FileSystem(path) = request.target else {
        return Err(PlatformPropertiesErrorKind::Unsupported);
    };
    let path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| PlatformPropertiesErrorKind::Failed)?;
        let result = read_store(PCWSTR(path.as_ptr()), cancel);
        CoUninitialize();
        result
    }
}

unsafe fn read_store(
    path: PCWSTR,
    cancel: &AtomicBool,
) -> Result<PlatformProperties, PlatformPropertiesErrorKind> {
    let store: IPropertyStore = unsafe {
        SHGetPropertyStoreFromParsingName(
            path,
            None::<&windows::Win32::System::Com::IBindCtx>,
            GPS_BESTEFFORT | GPS_OPENSLOWITEM,
        )
    }
    .map_err(map_error)?;
    let mut sections: Vec<PlatformPropertySection> = Vec::new();
    for (canonical, section_title, display_name) in APPROVED_PROPERTIES {
        if cancel.load(Ordering::Relaxed) {
            return Err(PlatformPropertiesErrorKind::Cancelled);
        }
        let canonical_wide: Vec<u16> = canonical.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key = PROPERTYKEY::default();
        if unsafe { PSGetPropertyKeyFromName(PCWSTR(canonical_wide.as_ptr()), &mut key) }.is_err() {
            continue;
        }
        let Ok(value) = (unsafe { store.GetValue(&key) }) else {
            continue;
        };
        let mut text = vec![0u16; 4096];
        if unsafe { PropVariantToString(&value, &mut text) }.is_err() {
            continue;
        }
        let end = text
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(text.len());
        if end == 0 {
            continue;
        }
        let value = String::from_utf16_lossy(&text[..end]);
        let section = if let Some(section) = sections
            .iter_mut()
            .find(|section| section.title.as_ref() == *section_title)
        {
            section
        } else {
            sections.push(PlatformPropertySection {
                title: Arc::from(*section_title),
                properties: Vec::new(),
            });
            sections.last_mut().expect("section was just inserted")
        };
        section.properties.push(PlatformProperty {
            canonical_key: Arc::from(*canonical),
            display_name: Arc::from(*display_name),
            value: PlatformPropertyValue::Text(Arc::from(value)),
        });
    }
    Ok(PlatformProperties { sections })
}

fn map_error(error: windows::core::Error) -> PlatformPropertiesErrorKind {
    match error.code() {
        E_ACCESSDENIED => PlatformPropertiesErrorKind::PermissionDenied,
        E_INVALIDARG => PlatformPropertiesErrorKind::NotFound,
        _ => PlatformPropertiesErrorKind::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_allow_list_never_includes_gps_coordinates() {
        assert!(APPROVED_PROPERTIES.iter().all(|(key, _, _)| {
            !key.contains("Latitude") && !key.contains("Longitude") && !key.contains("GPS")
        }));
    }

    #[test]
    fn broker_wire_roundtrip_is_bounded_and_owned() {
        let properties = PlatformProperties {
            sections: vec![PlatformPropertySection {
                title: Arc::from("Windows"),
                properties: vec![PlatformProperty {
                    canonical_key: Arc::from("System.Title"),
                    display_name: Arc::from("Title"),
                    value: PlatformPropertyValue::Text(Arc::from("Canary")),
                }],
            }],
        };
        let mut bytes = Vec::new();
        encode_properties(&properties, &mut bytes).unwrap();
        assert_eq!(decode_properties(&bytes), Some(properties));
        assert!(decode_properties(&bytes[..bytes.len() - 1]).is_none());
    }
}
