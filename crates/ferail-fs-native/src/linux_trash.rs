use std::path::{Path, PathBuf};

pub fn home_files() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| crate::paths::home_dir().join(".local/share"))
        .join("Trash/files")
}

pub fn directories() -> std::io::Result<Vec<PathBuf>> {
    ferail_core::path_guard::assert_off_ui_thread("linux_trash::directories");
    trash::os_limited::trash_folders()
        .map(|dirs| dirs.into_iter().map(|dir| dir.join("files")).collect())
        .map_err(|error| std::io::Error::other(error.to_string()))
}

fn info_path(path: &Path) -> Option<PathBuf> {
    let files = path.parent()?;
    let root = crate::trash_root_of(path)?;
    if files != root.join("files") {
        return None;
    }
    let mut name = path.file_name()?.to_os_string();
    name.push(".trashinfo");
    Some(root.join("info").join(name))
}

pub fn forget(path: &Path) -> std::io::Result<()> {
    ferail_core::path_guard::assert_off_ui_thread("linux_trash::forget");
    if let Some(info) = info_path(path) {
        match std::fs::remove_file(info) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            result => result?,
        }
    }
    Ok(())
}

pub fn restore(path: &Path) -> std::io::Result<PathBuf> {
    ferail_core::path_guard::assert_off_ui_thread("linux_trash::restore");
    let info = info_path(path).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "select a top-level trash item",
        )
    })?;
    let item = trash::os_limited::list()
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .into_iter()
        .find(|item| Path::new(&item.id) == info)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "trash metadata not found")
        })?;
    let target = item.original_path();
    std::fs::create_dir_all(&item.original_parent)?;
    crate::rename::rename_no_replace(path, &target)?;
    forget(path)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        if std::env::var_os("FERAIL_TRASH_TEST_CHILD").is_none() {
            let home = std::env::temp_dir().join(format!("ferail-trash-{}", std::process::id()));
            std::fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "linux_trash::tests::round_trip", "--nocapture"])
                .env("FERAIL_TRASH_TEST_CHILD", "1")
                .env("HOME", &home)
                .env("XDG_DATA_HOME", home.join("data"))
                .status()
                .unwrap();
            std::fs::remove_dir_all(home).unwrap();
            assert!(status.success());
            return;
        }
        let home = crate::paths::home_dir();
        let source = home.join("documento con accento è.txt");
        let payload = home_files().join(source.file_name().unwrap());
        let fs = crate::NativeFs::new();
        assert!(fs
            .enumerate_streaming(
                &home_files(),
                32,
                &std::sync::atomic::AtomicBool::new(false),
                |_| {}
            )
            .is_none());
        assert!(crate::paths::well_known_locations()
            .iter()
            .any(|location| location.label == "Trash" && location.path == home_files()));
        std::fs::write(&source, b"original").unwrap();
        trash::delete(&source).unwrap();
        std::fs::write(&source, b"replacement").unwrap();
        assert_eq!(
            restore(&payload).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"replacement");
        assert_eq!(std::fs::read(&payload).unwrap(), b"original");
        std::fs::remove_file(&source).unwrap();
        assert_eq!(restore(&payload).unwrap(), source);
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        assert!(!info_path(&payload).unwrap().exists());
        let directory = home.join("directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("nested"), b"nested").unwrap();
        std::os::unix::fs::symlink(&source, directory.join("link")).unwrap();
        trash::delete(&directory).unwrap();
        let dirs = directories().unwrap();
        assert!(dirs.contains(&home_files()));
        let outcome = crate::trash_empty::empty_dirs(&[home_files()], &mut |_| {});
        assert!(outcome.failures.is_empty());
        assert_eq!(outcome.items_deleted, 1);
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(home_files()).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(home_files().parent().unwrap().join("info"))
                .unwrap()
                .count(),
            0
        );
        let uid = unsafe { libc::getuid() };
        let shared = PathBuf::from(format!("/volume/.Trash/{uid}/files/item"));
        assert!(crate::is_in_trash(&shared));
        assert_eq!(
            info_path(&shared),
            Some(PathBuf::from(format!(
                "/volume/.Trash/{uid}/info/item.trashinfo"
            )))
        );
        assert!(!crate::is_in_trash(Path::new(&format!(
            "/volume/.Trash/{}/files/item",
            uid + 1
        ))));
    }
}
