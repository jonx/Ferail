//! UDisks2 filesystems, including media that has not been mounted yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::blocking::{Connection, Proxy};
use zbus::fdo::ManagedObjects;
use zbus::zvariant::{OwnedValue, Value};

const SERVICE: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";

#[derive(Clone, Debug)]
pub struct Filesystem {
    pub object_path: String,
    pub name: String,
    pub mount_point: Option<PathBuf>,
    pub ejectable: bool,
}

/// Blocking: call from a worker. UDisks owns visibility and authorization.
pub fn list_filesystems() -> Result<Vec<Filesystem>, String> {
    ferail_core::path_guard::assert_off_ui_thread("list_filesystems");
    let connection = Connection::system().map_err(|error| error.to_string())?;
    let proxy = Proxy::new(
        &connection,
        SERVICE,
        ROOT,
        "org.freedesktop.DBus.ObjectManager",
    )
    .map_err(|error| error.to_string())?;
    let objects: ManagedObjects = proxy
        .call("GetManagedObjects", &())
        .map_err(|error| error.to_string())?;
    Ok(filesystems(&objects))
}

fn filesystems(objects: &ManagedObjects) -> Vec<Filesystem> {
    let mut filesystems = Vec::new();
    for (path, interfaces) in objects {
        let (Some(block), Some(filesystem)) = (interfaces.get(BLOCK), interfaces.get(FILESYSTEM))
        else {
            continue;
        };
        if block.get("HintIgnore").and_then(|v| bool::try_from(v).ok()) == Some(true) {
            continue;
        }
        let Some(device) = block.get("Device").and_then(byte_path) else {
            continue;
        };
        let mount_point = filesystem
            .get("MountPoints")
            .and_then(|value| <Vec<Vec<u8>>>::try_from(value.try_clone().ok()?).ok())
            .and_then(|mounts| mounts.into_iter().next())
            .and_then(path_bytes);
        let name = ["HintName", "IdLabel", "IdUUID"]
            .into_iter()
            .filter_map(|key| block.get(key).and_then(|v| <&str>::try_from(v).ok()))
            .find(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| device.display().to_string());
        filesystems.push(Filesystem {
            object_path: path.to_string(),
            name,
            mount_point,
            ejectable: block.get("HintSystem").and_then(|v| bool::try_from(v).ok()) == Some(false),
        });
    }
    filesystems.sort_by_key(|filesystem| filesystem.name.to_lowercase());
    filesystems
}

fn byte_path(value: &OwnedValue) -> Option<PathBuf> {
    path_bytes(Vec::<u8>::try_from(value.try_clone().ok()?).ok()?)
}

fn path_bytes(mut bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

/// Mount only in response to the user's activation. Polkit handles permission.
pub fn mount(object_path: &str) -> Result<PathBuf, String> {
    ferail_core::path_guard::assert_off_ui_thread("mount_filesystem");
    let connection = Connection::system().map_err(|error| error.to_string())?;
    let proxy = Proxy::new(&connection, SERVICE, object_path, FILESYSTEM)
        .map_err(|error| error.to_string())?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let path: String = proxy
        .call("Mount", &(options,))
        .map_err(|error| error.to_string())?;
    if !Path::new(&path).is_absolute() {
        return Err("UDisks returned a relative mount point".into());
    }
    Ok(PathBuf::from(path))
}

/// Process-lifetime signal listener; registration and callbacks stay off UI.
pub fn observe(callback: Box<dyn Fn() + Send + 'static>) {
    std::thread::spawn(move || {
        let watch = || -> zbus::Result<()> {
            let connection = Connection::system()?;
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(SERVICE)?
                .path_namespace(ROOT)?
                .build();
            let messages =
                zbus::blocking::MessageIterator::for_match_rule(rule, &connection, Some(64))?;
            callback();
            for message in messages {
                let message = message?;
                // Job progress does not change the device list.
                if message.header().path().is_some_and(|path| {
                    path.as_str() == ROOT
                        || path
                            .as_str()
                            .starts_with("/org/freedesktop/UDisks2/block_devices/")
                }) {
                    callback();
                }
            }
            Ok(())
        };
        if let Err(error) = watch() {
            eprintln!("UDisks volume observer: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "/org/freedesktop/UDisks2/block_devices/test1";

    struct Block {
        ignored: bool,
    }

    #[zbus::interface(name = "org.freedesktop.UDisks2.Block")]
    impl Block {
        #[zbus(property)]
        fn device(&self) -> Vec<u8> {
            b"/dev/test1\0".to_vec()
        }
        #[zbus(property)]
        fn id_label(&self) -> &str {
            "Test disk"
        }
        #[zbus(property)]
        fn hint_ignore(&self) -> bool {
            self.ignored
        }
        #[zbus(property)]
        fn hint_system(&self) -> bool {
            false
        }
    }

    struct TestFilesystem {
        path: String,
        mounted: std::sync::atomic::AtomicBool,
    }

    #[zbus::interface(name = "org.freedesktop.UDisks2.Filesystem")]
    impl TestFilesystem {
        fn mount(&self, _options: HashMap<String, OwnedValue>) -> String {
            self.mounted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.path.clone()
        }

        #[zbus(property)]
        fn mount_points(&self) -> Vec<Vec<u8>> {
            if self.mounted.load(std::sync::atomic::Ordering::SeqCst) {
                vec![self.path.as_bytes().iter().copied().chain([0]).collect()]
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn device_paths_preserve_bytes() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            path_bytes(b"/media/caf\xe9\0".to_vec())
                .unwrap()
                .as_os_str()
                .as_bytes(),
            b"/media/caf\xe9"
        );
        assert!(path_bytes(b"relative\0".to_vec()).is_none());
        assert!(path_bytes(vec![0]).is_none());
    }

    #[test]
    #[ignore = "requires a private DBUS_SYSTEM_BUS_ADDRESS and FERAIL_TEST_PRIVATE_BUS=1"]
    fn isolated_udisks_mount_and_signals() {
        let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap();
        assert_eq!(
            std::env::var("FERAIL_TEST_PRIVATE_BUS").as_deref(),
            Ok("1"),
            "private test bus required"
        );
        let filesystem = TestFilesystem {
            path: "/media/Test disk".into(),
            mounted: std::sync::atomic::AtomicBool::new(false),
        };
        let server = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(ROOT, zbus::fdo::ObjectManager)
            .unwrap()
            .serve_at(DEVICE, Block { ignored: false })
            .unwrap()
            .serve_at(DEVICE, filesystem)
            .unwrap()
            .serve_at(
                "/org/freedesktop/UDisks2/block_devices/hidden",
                Block { ignored: true },
            )
            .unwrap()
            .serve_at(
                "/org/freedesktop/UDisks2/block_devices/hidden",
                TestFilesystem {
                    path: "relative".into(),
                    mounted: std::sync::atomic::AtomicBool::new(false),
                },
            )
            .unwrap()
            .serve_at(
                "/org/freedesktop/UDisks2/block_devices/raw",
                Block { ignored: false },
            )
            .unwrap()
            .build()
            .unwrap();
        let before = list_filesystems().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].name, "Test disk");
        assert!(before[0].ejectable);
        assert!(before[0].mount_point.is_none());
        assert!(mount("/org/freedesktop/UDisks2/block_devices/hidden").is_err());

        let (tx, rx) = std::sync::mpsc::channel();
        observe(Box::new(move || {
            let _ = tx.send(());
        }));
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(mount(DEVICE).unwrap(), Path::new("/media/Test disk"));
        assert_eq!(
            list_filesystems().unwrap()[0].mount_point.as_deref(),
            Some(Path::new("/media/Test disk"))
        );
        server
            .emit_signal(
                None::<&str>,
                DEVICE,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    FILESYSTEM,
                    HashMap::<String, OwnedValue>::new(),
                    vec!["MountPoints"],
                ),
            )
            .unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let second = "/org/freedesktop/UDisks2/block_devices/test2";
        server
            .object_server()
            .at(second, Block { ignored: false })
            .unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        server
            .object_server()
            .at(
                second,
                TestFilesystem {
                    path: "/media/Second disk".into(),
                    mounted: std::sync::atomic::AtomicBool::new(false),
                },
            )
            .unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(list_filesystems().unwrap().len(), 2);
        server
            .object_server()
            .remove::<TestFilesystem, _>(second)
            .unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(list_filesystems().unwrap().len(), 1);
    }
}
