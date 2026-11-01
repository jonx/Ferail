use std::path::{Path, PathBuf};
use std::{io::Read, os::unix::ffi::OsStringExt, os::unix::fs::OpenOptionsExt};

pub fn home_files() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| crate::paths::home_dir().join(".local/share"))
        .join("Trash/files")
}

pub fn directories() -> std::io::Result<Vec<PathBuf>> {
    ferail_core::path_guard::assert_off_ui_thread("linux_trash::directories");
    let mut files = Vec::new();
    for root in trash::os_limited::trash_folders()
        .map_err(|error| std::io::Error::other(error.to_string()))?
    {
        if check_directory(&root)? && check_directory(&root.join("files"))? {
            check_directory(&root.join("info"))?;
            files.push(root.join("files"));
        }
    }
    Ok(files)
}

fn check_directory(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "trash directory is not a real directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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
    if let Some(info) = checked_info_path(path)? {
        match std::fs::remove_file(info) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            result => result?,
        }
    }
    Ok(())
}

fn checked_info_path(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let Some(info) = info_path(path) else {
        return Ok(None);
    };
    let root = info.parent().unwrap().parent().unwrap();
    if root != home_files().parent().unwrap()
        && !trash::os_limited::trash_folders()
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .contains(root)
    {
        return Ok(None);
    }
    check_directory(root)?;
    check_directory(&root.join("files"))?;
    check_directory(&root.join("info"))?;
    Ok(Some(info))
}

pub fn restore(path: &Path) -> std::io::Result<PathBuf> {
    ferail_core::path_guard::assert_off_ui_thread("linux_trash::restore");
    let info = checked_info_path(path)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "select a top-level trash item",
        )
    })?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&info)?;
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid trash metadata");
    if !file.metadata()?.is_file() {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    const MAX_INFO_BYTES: u64 = 64 * 1024;
    file.take(MAX_INFO_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INFO_BYTES as usize {
        return Err(invalid());
    }
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    if lines.next() != Some(b"[Trash Info]".as_slice()) {
        return Err(invalid());
    }
    let mut encoded = lines
        .find_map(|line| line.strip_prefix(b"Path="))
        .ok_or_else(invalid)?
        .iter();
    let mut decoded = Vec::new();
    while let Some(&byte) = encoded.next() {
        decoded.push(if byte == b'%' {
            let high = encoded
                .next()
                .and_then(|&b| (b as char).to_digit(16))
                .ok_or_else(invalid)?;
            let low = encoded
                .next()
                .and_then(|&b| (b as char).to_digit(16))
                .ok_or_else(invalid)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    if decoded.is_empty() || decoded.contains(&0) {
        return Err(invalid());
    }
    let mut target = PathBuf::from(std::ffi::OsString::from_vec(decoded));
    if target.is_relative() {
        if target
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(invalid());
        }
        let root = info.parent().unwrap().parent().unwrap();
        let mut base = root.parent().ok_or_else(invalid)?;
        if root != home_files().parent().unwrap()
            && base.file_name().is_some_and(|name| name == ".Trash")
        {
            base = base.parent().ok_or_else(invalid)?;
        }
        target = base.join(target);
    }
    std::fs::create_dir_all(target.parent().ok_or_else(invalid)?)?;
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

        let info = info_path(&payload).unwrap();
        let other = info.with_file_name("other.trashinfo");
        std::fs::write(&other, b"[Trash Info]\n# unrelated metadata\n").unwrap();
        for value in [
            "relative/space%20and%FF".to_owned(),
            format!("{}/absolute%FF", home.display()),
        ] {
            std::fs::write(&payload, b"restore me").unwrap();
            std::fs::write(
                &info,
                format!("[Trash Info]\n# extra line\nPath={value}\nPath=/wrong\n"),
            )
            .unwrap();
            let restored = restore(&payload).unwrap();
            assert_eq!(std::fs::read(&restored).unwrap(), b"restore me");
            if value.starts_with("relative/") {
                assert_eq!(restored.parent().unwrap(), home.join("data/relative"));
            }
            assert_eq!(restored.as_os_str().as_encoded_bytes().last(), Some(&0xff));
            std::fs::remove_file(restored).unwrap();
        }
        std::fs::remove_file(other).unwrap();
        for value in ["../escape", "%2E%2E/escape", "%00", "%GG", ""] {
            std::fs::write(&payload, b"keep me").unwrap();
            std::fs::write(&info, format!("[Trash Info]\nPath={value}\n")).unwrap();
            assert_eq!(
                restore(&payload).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
            assert_eq!(std::fs::read(&payload).unwrap(), b"keep me");
            assert!(info.exists());
        }
        std::fs::write(
            &info,
            format!("[Trash Info]\nPath={}/oversized\n", home.display()),
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&info)
            .unwrap()
            .set_len(65 * 1024)
            .unwrap();
        assert_eq!(
            restore(&payload).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(&payload).unwrap(), b"keep me");
        std::fs::remove_file(&payload).unwrap();
        std::fs::remove_file(&info).unwrap();

        let files = home_files();
        let saved = files.with_file_name("saved-files");
        std::fs::rename(&files, &saved).unwrap();
        std::os::unix::fs::symlink(&home, &files).unwrap();
        assert!(directories().is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        std::fs::remove_file(&files).unwrap();
        std::fs::rename(&saved, &files).unwrap();

        let uid = unsafe { libc::getuid() };
        let ordinary = home.join(format!("project/.Trash/{uid}"));
        std::fs::create_dir_all(ordinary.join("info")).unwrap();
        let ordinary_info = ordinary.join("info/item.trashinfo");
        std::fs::write(&ordinary_info, b"not trash metadata").unwrap();
        forget(&ordinary.join("files/item")).unwrap();
        assert!(ordinary_info.exists());
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
