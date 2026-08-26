use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
        std::thread::scope(|scope| {
            scope
                .spawn(|| read_on_sta(request, cancel))
                .join()
                .unwrap_or(Err(PlatformPropertiesErrorKind::Failed))
        })
    }
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
}
