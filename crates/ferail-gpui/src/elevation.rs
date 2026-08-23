//! Privileged retry for file operations.
//!
//! When a copy/move fails because the process lacks rights
//! ([`FileOpErrorKind::PermissionDenied`]), the user can re-run *just the
//! failed items* with administrator privileges. We don't (can't, on Windows)
//! elevate the running process — instead we re-launch this same binary
//! elevated with `--elevated-op <descriptor>`, which performs the op as
//! root/admin and writes a result file the parent reads back.
//!
//! The platform primitive ([`crate::platform_shell::run_elevated_self`]) only
//! knows how to "re-launch self elevated and wait" — it never sees the op
//! type. Everything about *what* to run lives here, so the shell crates stay
//! free of any dependency on this descriptor.
//!
//! No serde in this workspace (settings use a hand-rolled `key=value` format),
//! so the descriptor and result use a small NUL-separated encoding — robust
//! against any path on unix, where the bytes round-trip losslessly.
//!
//! ## Handshake hardening
//!
//! The descriptor tells a **root** process what to move/copy/delete, so the
//! files it travels through must not be plantable or swappable by another
//! local user (macOS's per-user mode-700 `$TMPDIR` masks this, but a shared
//! `/tmp` — the future Linux pkexec path — does not):
//!
//! - Each op gets a fresh private directory `ferail-elev-<random>` in
//!   `temp_dir()`, created **exclusively** with mode `0o700` on unix
//!   ([`ElevFiles::create`]). The random token comes from `/dev/urandom`
//!   when available, else a documented std-only hash fallback.
//! - The descriptor inside it is created with `O_EXCL` + mode `0o600`
//!   *before* any content is written.
//! - The elevated worker is told the invoking user's uid
//!   (`--elevated-uid`, read off the directory the parent just created) and
//!   refuses to read the descriptor or create the result unless the private
//!   directory is a real (non-symlink) directory owned by that uid with no
//!   group/other access, and the descriptor itself — opened `O_NOFOLLOW`,
//!   then re-checked on the open fd — is a regular file owned by that uid
//!   and not group/other writable.
//! - The worker creates the result file with `O_EXCL` (which never follows
//!   a planted symlink), so root's write cannot be redirected.
//! - Result records are line-delimited; path fields are escaped
//!   ([`escape_field`]) so a filename containing `\n` cannot split or spoof
//!   records.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferail_fs_native::file_ops::{
    self as engine, CollisionPolicy, FileOpErrorKind, TransferProgress,
};

/// A copy or move of specific items into a destination, to be re-run elevated.
/// Built from the *failed top-level sources* of an op the unprivileged process
/// couldn't finish.
#[derive(Clone, Debug)]
pub struct ElevatedOp {
    pub is_move: bool,
    pub dest_dir: PathBuf,
    pub sources: Vec<PathBuf>,
}

/// What the elevated worker reported back: how many items it completed, and
/// the ones that still failed (so the parent can show "2 of 3 done as admin,
/// 1 still failed — locked").
#[derive(Clone, Debug, Default)]
pub struct ElevatedResult {
    pub ok: usize,
    pub failures: Vec<(FileOpErrorKind, PathBuf)>,
}

/// A privileged trash-or-delete of specific items, re-run elevated after the
/// unprivileged process hit a permission denial (a root-owned app, a
/// root-owned item in the Trash). Built from just the *permission-denied*
/// failures — the only class elevation can fix.
///
/// `delete == false`: **move** each item into `trash_dir` (the user's `~/.Trash`)
/// so it lands where they expect, recoverable. The worker runs as root, so this
/// is an explicit move — root's own `trashItemAtURL` would target *root's*
/// Trash. `delete == true`: **remove** each item permanently (Shift+Delete /
/// Empty Trash on protected items); `trash_dir` is then unused.
#[derive(Clone, Debug)]
pub struct ElevatedTrashOp {
    pub delete: bool,
    pub trash_dir: PathBuf,
    pub sources: Vec<PathBuf>,
}

/// What the elevated trash worker reported: the `(original, landed)` pairs it
/// moved into the Trash (empty when `delete == true`) and the items that still
/// failed even as root.
#[derive(Clone, Debug, Default)]
pub struct ElevatedTrashResult {
    pub trashed: Vec<(PathBuf, PathBuf)>,
    pub failed: Vec<PathBuf>,
}

// ---- path <-> bytes (lossless on unix; near-lossless on Windows) ----------

#[cfg(unix)]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}
#[cfg(unix)]
fn bytes_to_path(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}
#[cfg(not(unix))]
fn path_to_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}
#[cfg(not(unix))]
fn bytes_to_path(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}

impl ElevatedOp {
    /// `<MODE>\0<DEST>\0<SRC>\0<SRC>…` — MODE is `MOVE` or `COPY`.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(if self.is_move { b"MOVE" } else { b"COPY" });
        out.push(0);
        out.extend_from_slice(&path_to_bytes(&self.dest_dir));
        for s in &self.sources {
            out.push(0);
            out.extend_from_slice(&path_to_bytes(s));
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<ElevatedOp, String> {
        let mut parts = bytes.split(|b| *b == 0);
        let mode = parts.next().ok_or("empty descriptor")?;
        let is_move = match mode {
            b"MOVE" => true,
            b"COPY" => false,
            _ => return Err("descriptor: unknown mode".into()),
        };
        let dest = parts.next().ok_or("descriptor: missing dest")?;
        let dest_dir = bytes_to_path(dest);
        let sources: Vec<PathBuf> = parts.filter(|p| !p.is_empty()).map(bytes_to_path).collect();
        if sources.is_empty() {
            return Err("descriptor: no sources".into());
        }
        Ok(ElevatedOp {
            is_move,
            dest_dir,
            sources,
        })
    }
}

// ---- field escaping for the line-delimited result files --------------------
//
// Result files are one record per line, fields NUL-separated. A filename
// containing `\n` (legal on unix) would otherwise split a record — letting a
// crafted name truncate the report or spoof extra records when the parent
// parses what root wrote. Path fields are therefore escaped on encode:
// `\` → `\\`, newline → `\n`, NUL → `\0` (the kernel forbids NUL in real
// path bytes, but escaping it too makes the framing injection-proof even for
// synthetic paths). Descriptors are purely NUL-separated with no line
// framing and stay unescaped.

fn escape_field(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    for &c in b {
        match c {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0 => out.extend_from_slice(b"\\0"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape_field(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut it = b.iter().copied();
    while let Some(c) = it.next() {
        if c == b'\\' {
            match it.next() {
                Some(b'n') => out.push(b'\n'),
                Some(b'0') => out.push(0),
                Some(other) => out.push(other), // `\\`; unknown escapes pass through
                None => out.push(b'\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn kind_name(k: FileOpErrorKind) -> &'static str {
    match k {
        FileOpErrorKind::PermissionDenied => "PermissionDenied",
        FileOpErrorKind::Locked => "Locked",
        FileOpErrorKind::NotFound => "NotFound",
        FileOpErrorKind::NoSpace => "NoSpace",
        FileOpErrorKind::ReadOnly => "ReadOnly",
        FileOpErrorKind::NameTooLong => "NameTooLong",
        FileOpErrorKind::AlreadyExists => "AlreadyExists",
        FileOpErrorKind::Other => "Other",
    }
}
fn kind_from_name(s: &str) -> FileOpErrorKind {
    match s {
        "PermissionDenied" => FileOpErrorKind::PermissionDenied,
        "Locked" => FileOpErrorKind::Locked,
        "NotFound" => FileOpErrorKind::NotFound,
        "NoSpace" => FileOpErrorKind::NoSpace,
        "ReadOnly" => FileOpErrorKind::ReadOnly,
        "NameTooLong" => FileOpErrorKind::NameTooLong,
        "AlreadyExists" => FileOpErrorKind::AlreadyExists,
        _ => FileOpErrorKind::Other,
    }
}

impl ElevatedResult {
    /// One record per line; fields NUL-separated. `ok\0<n>` then
    /// `fail\0<KindName>\0<path>` per remaining failure. Path fields are
    /// escaped ([`escape_field`]) so a `\n` in a filename can't break framing.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"ok\0");
        out.extend_from_slice(self.ok.to_string().as_bytes());
        for (kind, path) in &self.failures {
            out.push(b'\n');
            out.extend_from_slice(b"fail\0");
            out.extend_from_slice(kind_name(*kind).as_bytes());
            out.push(0);
            out.extend_from_slice(&escape_field(&path_to_bytes(path)));
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<ElevatedResult, String> {
        let mut result = ElevatedResult::default();
        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut f = line.split(|b| *b == 0);
            match f.next() {
                Some(b"ok") => {
                    let n = f.next().ok_or("result: ok missing count")?;
                    result.ok = String::from_utf8_lossy(n).trim().parse().unwrap_or(0);
                }
                Some(b"fail") => {
                    let kind = f.next().ok_or("result: fail missing kind")?;
                    let path = f.next().ok_or("result: fail missing path")?;
                    result.failures.push((
                        kind_from_name(&String::from_utf8_lossy(kind)),
                        bytes_to_path(&unescape_field(path)),
                    ));
                }
                _ => {}
            }
        }
        Ok(result)
    }
}

impl ElevatedTrashOp {
    /// `<MODE>\0<TRASH_DIR>\0<SRC>\0<SRC>…` — MODE is `TRASH` or `DELETE`.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(if self.delete { b"DELETE" } else { b"TRASH" });
        out.push(0);
        out.extend_from_slice(&path_to_bytes(&self.trash_dir));
        for s in &self.sources {
            out.push(0);
            out.extend_from_slice(&path_to_bytes(s));
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<ElevatedTrashOp, String> {
        let mut parts = bytes.split(|b| *b == 0);
        let mode = parts.next().ok_or("empty trash descriptor")?;
        let delete = match mode {
            b"TRASH" => false,
            b"DELETE" => true,
            _ => return Err("trash descriptor: unknown mode".into()),
        };
        let trash_dir = bytes_to_path(parts.next().ok_or("trash descriptor: missing dir")?);
        let sources: Vec<PathBuf> = parts.filter(|p| !p.is_empty()).map(bytes_to_path).collect();
        if sources.is_empty() {
            return Err("trash descriptor: no sources".into());
        }
        Ok(ElevatedTrashOp {
            delete,
            trash_dir,
            sources,
        })
    }
}

impl ElevatedTrashResult {
    /// One record per line; fields NUL-separated. `trash\0<orig>\0<landed>` per
    /// moved item, `fail\0<path>` per remaining failure. Path fields are
    /// escaped ([`escape_field`]) so a `\n` in a filename can't break framing.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut first = true;
        let mut sep = |out: &mut Vec<u8>| {
            if first {
                first = false;
            } else {
                out.push(b'\n');
            }
        };
        for (orig, landed) in &self.trashed {
            sep(&mut out);
            out.extend_from_slice(b"trash\0");
            out.extend_from_slice(&escape_field(&path_to_bytes(orig)));
            out.push(0);
            out.extend_from_slice(&escape_field(&path_to_bytes(landed)));
        }
        for path in &self.failed {
            sep(&mut out);
            out.extend_from_slice(b"fail\0");
            out.extend_from_slice(&escape_field(&path_to_bytes(path)));
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<ElevatedTrashResult, String> {
        let mut result = ElevatedTrashResult::default();
        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut f = line.split(|b| *b == 0);
            match f.next() {
                Some(b"trash") => {
                    let orig = f.next().ok_or("result: trash missing original")?;
                    let landed = f.next().ok_or("result: trash missing landed")?;
                    result.trashed.push((
                        bytes_to_path(&unescape_field(orig)),
                        bytes_to_path(&unescape_field(landed)),
                    ));
                }
                Some(b"fail") => {
                    let path = f.next().ok_or("result: fail missing path")?;
                    result.failed.push(bytes_to_path(&unescape_field(path)));
                }
                _ => {}
            }
        }
        Ok(result)
    }
}

// ---- private handshake directory -------------------------------------------

/// Random hex token for the private directory name. Prefers real OS
/// randomness — 16 bytes of `/dev/urandom` via `std::fs` (this workspace
/// avoids new deps, so no `rand`/`getrandom` crate). The fallback hashes
/// `SystemTime` + pid + a process-local counter + a stack address (ASLR)
/// through `DefaultHasher`; it is *not* cryptographic, but the name only
/// needs to be unguessable in practice, and the directory create below is
/// exclusive, so a guessed/pre-created name fails closed instead of being
/// silently adopted.
fn random_token() -> String {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let mut buf = [0u8; 16];
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut buf))
            .is_ok()
        {
            use std::fmt::Write as _;
            let mut out = String::with_capacity(32);
            for b in buf {
                let _ = write!(out, "{b:02x}");
            }
            return out;
        }
    }
    use std::hash::{Hash as _, Hasher as _};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(32);
    let mut seed = COUNTER.fetch_add(1, Ordering::Relaxed);
    for _ in 0..2 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::process::id().hash(&mut h);
        seed.hash(&mut h);
        (&seed as *const u64 as usize).hash(&mut h);
        seed = h.finish();
        use std::fmt::Write as _;
        let _ = write!(out, "{seed:016x}");
    }
    out
}

/// The user-side handshake area: a freshly created private directory in
/// `temp_dir()` holding the descriptor and (after the worker runs) the
/// result. See the module docs ("Handshake hardening") for the threat model;
/// in short, the directory name is unpredictable, its creation is exclusive
/// with mode `0o700` on unix, and the descriptor is `O_EXCL` + `0o600` before
/// content lands — so on a shared `/tmp` no other user can pre-create, read,
/// or swap any part of the handshake.
struct ElevFiles {
    dir: PathBuf,
    desc: PathBuf,
    result: PathBuf,
}

impl ElevFiles {
    fn create(descriptor: &[u8]) -> Result<ElevFiles, String> {
        let mut last_err = "create private dir: exhausted retries".to_string();
        for _ in 0..8 {
            let dir = std::env::temp_dir().join(format!("ferail-elev-{}", random_token()));
            // `mut` feeds the unix-only mode(0o700) below.
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&dir) {
                Ok(()) => {
                    let desc = dir.join("op.desc");
                    let result = dir.join("op.result");
                    let mut opts = std::fs::OpenOptions::new();
                    // O_EXCL: fail rather than adopt anything pre-created.
                    opts.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt as _;
                        // Owner-only *at creation*, before content is written.
                        opts.mode(0o600);
                    }
                    let write = opts
                        .open(&desc)
                        .and_then(|mut f| std::io::Write::write_all(&mut f, descriptor));
                    return match write {
                        Ok(()) => Ok(ElevFiles { dir, desc, result }),
                        Err(e) => {
                            let _ = std::fs::remove_file(&desc);
                            let _ = std::fs::remove_dir(&dir);
                            Err(format!("write descriptor: {e}"))
                        }
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Collision (astronomically unlikely with urandom; possible
                    // under the hash fallback) — try a fresh token.
                    last_err = format!("create private dir: {e}");
                }
                Err(e) => return Err(format!("create private dir: {e}")),
            }
        }
        Err(last_err)
    }

    /// The uid the elevated worker must see as owner of the handshake dir —
    /// i.e. OUR uid, read back from the directory we just created (std
    /// exposes no `getuid` without libc, and this crate has no libc dep).
    #[cfg(unix)]
    fn owner_uid(&self) -> Result<u32, String> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(&self.dir)
            .map(|m| m.uid())
            .map_err(|e| format!("stat private dir: {e}"))
    }

    /// Worker CLI for this handshake: `<flag> <desc> --elevated-result <res>`
    /// plus, on unix, `--elevated-uid <uid>` so the root side can verify who
    /// staged the descriptor.
    fn worker_args(&self, flag: &str) -> Result<Vec<String>, String> {
        #[allow(unused_mut)]
        let mut args = vec![
            flag.to_string(),
            self.desc.to_string_lossy().into_owned(),
            "--elevated-result".to_string(),
            self.result.to_string_lossy().into_owned(),
        ];
        #[cfg(unix)]
        {
            args.push("--elevated-uid".to_string());
            args.push(self.owner_uid()?.to_string());
        }
        Ok(args)
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.desc);
        let _ = std::fs::remove_file(&self.result);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

// ---- worker-side verification -----------------------------------------------

/// `O_NOFOLLOW` for `OpenOptionsExt::custom_flags`, spelled per-OS because
/// this crate deliberately links no libc. Values match the platform ABI
/// headers (`<fcntl.h>`): `0x0100` across the BSD family incl. macOS/iOS,
/// `0o400000` on Linux/Android.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_NOFOLLOW: i32 = 0o400000;
#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "linux",
        target_os = "android"
    ))
))]
const O_NOFOLLOW: i32 = 0; // unknown unix: the metadata checks still apply

/// Root side: refuse to trust the handshake dir unless it is a real
/// (non-symlink) directory owned by the invoking user with zero group/other
/// access — exactly what [`ElevFiles::create`] made. On a shared `/tmp` this
/// is what stops another local user from staging a descriptor for root to
/// execute, or from redirecting where root writes the result.
#[cfg(unix)]
fn verify_private_dir(dir: &Path, expected_uid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let md = std::fs::symlink_metadata(dir).map_err(|e| format!("stat {}: {e}", dir.display()))?;
    if !md.file_type().is_dir() {
        return Err(format!("{}: not a directory", dir.display()));
    }
    if md.uid() != expected_uid {
        return Err(format!(
            "{}: owned by uid {}, expected {}",
            dir.display(),
            md.uid(),
            expected_uid
        ));
    }
    if md.mode() & 0o077 != 0 {
        return Err(format!(
            "{}: group/other permissions present (mode {:o})",
            dir.display(),
            md.mode() & 0o777
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn expected_uid_from_args(args: &[String]) -> Result<u32, String> {
    flag_value(args, "--elevated-uid")
        .ok_or_else(|| "missing --elevated-uid".to_string())?
        .parse()
        .map_err(|_| "--elevated-uid: not a uid".to_string())
}

/// Read the descriptor as the (possibly root) worker. On unix the file is
/// trusted only after verifying the private dir (ownership + `0o700`) and the
/// descriptor itself: opened with `O_NOFOLLOW` so a symlink swap fails, then
/// re-checked on the *open fd* (regular file, owned by the invoking uid, not
/// group/other writable) so there is no check-then-open race outside the
/// invoking user's own control. On non-unix (Windows elevation is stubbed)
/// this is a plain read; the per-user ACL'd temp dir covers it.
fn read_descriptor_verified(desc_path: &str, args: &[String]) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let expected_uid = expected_uid_from_args(args)?;
        let path = Path::new(desc_path);
        let dir = path
            .parent()
            .ok_or_else(|| "descriptor path has no parent".to_string())?;
        verify_private_dir(dir, expected_uid)?;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(path)
            .map_err(|e| format!("open descriptor: {e}"))?;
        let md = file
            .metadata()
            .map_err(|e| format!("stat descriptor: {e}"))?;
        if !md.file_type().is_file() {
            return Err("descriptor is not a regular file".into());
        }
        if md.uid() != expected_uid {
            return Err(format!(
                "descriptor owned by uid {}, expected {}",
                md.uid(),
                expected_uid
            ));
        }
        if md.mode() & 0o022 != 0 {
            return Err("descriptor is group/other writable".into());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("read descriptor: {e}"))?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        std::fs::read(desc_path).map_err(|e| format!("read descriptor: {e}"))
    }
}

/// Write the result as the (possibly root) worker. Exclusive create —
/// `O_CREAT|O_EXCL` never follows a planted symlink, so root's write cannot
/// be redirected — after re-verifying the private dir on unix. Mode `0o644`
/// (set explicitly on the fd, immune to the root shell's umask) so the
/// root-owned file is readable back by the invoking user; the `0o700`
/// directory keeps it private from everyone else.
fn write_result_verified(result_path: &str, bytes: &[u8], args: &[String]) -> Result<(), String> {
    let path = Path::new(result_path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let expected_uid = expected_uid_from_args(args)?;
        let dir = path
            .parent()
            .ok_or_else(|| "result path has no parent".to_string())?;
        verify_private_dir(dir, expected_uid)?;
        opts.mode(0o644);
    }
    #[cfg(not(unix))]
    let _ = args;
    let mut file = opts.open(path).map_err(|e| format!("create result: {e}"))?;
    std::io::Write::write_all(&mut file, bytes).map_err(|e| format!("write result: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // create_new's mode is masked by umask; force user readability so the
        // parent can read the root-owned result back.
        file.set_permissions(std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("chmod result: {e}"))?;
    }
    Ok(())
}

/// Parent side: serialise `op` to a temp descriptor, run the elevated worker
/// (blocks on the OS auth prompt — call from a background thread), and read
/// back which items still failed. An empty `failures` means everything landed.
pub fn run_elevated_op(op: &ElevatedOp) -> Result<ElevatedResult, String> {
    let files = ElevFiles::create(&op.encode())?;
    let args = match files.worker_args("--elevated-op") {
        Ok(args) => args,
        Err(e) => {
            files.cleanup();
            return Err(e);
        }
    };
    let run = crate::platform_shell::run_elevated_self(&args);
    // The worker exits 0 even when some items fail (per-item status is in the
    // result file); a hard error here means it couldn't be launched/elevated.
    let outcome = run.and_then(|_code| {
        std::fs::read(&files.result)
            .map_err(|e| format!("read result: {e}"))
            .and_then(|b| ElevatedResult::decode(&b))
    });

    files.cleanup();
    outcome
}

/// Worker side: `--elevated-op <descriptor> --elevated-result <result>`. Runs
/// the op via the same engine the GUI uses, writes the per-item result, and
/// returns a process exit code. Always exits 0 when it *ran* (item failures
/// live in the result file); non-zero only when it could not run at all, so
/// the macOS osascript wrapper treats "ran" as success.
pub fn run_elevated_op_worker(args: &[String]) -> i32 {
    let Some(desc_path) = flag_value(args, "--elevated-op") else {
        eprintln!("--elevated-op: missing descriptor path");
        return 2;
    };
    let Some(result_path) = flag_value(args, "--elevated-result") else {
        eprintln!("--elevated-op: missing --elevated-result path");
        return 2;
    };
    let bytes = match read_descriptor_verified(&desc_path, args) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("--elevated-op: {e}");
            return 2;
        }
    };
    let op = match ElevatedOp::decode(&bytes) {
        Ok(op) => op,
        Err(e) => {
            eprintln!("--elevated-op: {e}");
            return 2;
        }
    };

    let prog = TransferProgress::new();
    let cancel = AtomicBool::new(false);
    let result = match engine::plan_transfer(&op.sources, &op.dest_dir, &prog, &cancel) {
        Ok(plan) => {
            // Retry overwrites any partial left by the failed unprivileged run.
            let policy = |_: &Path| CollisionPolicy::Replace;
            let outcome = if op.is_move {
                engine::run_move(&plan, &policy, &prog, &cancel)
            } else {
                engine::run_copy(&plan, &policy, &prog, &cancel)
            };
            match outcome {
                Ok(o) => ElevatedResult {
                    ok: o.created.len(),
                    failures: o.failed.iter().map(|e| (e.kind, e.path.clone())).collect(),
                },
                Err(e) => fatal_result(&op, &e),
            }
        }
        Err(e) => fatal_result(&op, &e),
    };

    if let Err(e) = write_result_verified(&result_path, &result.encode(), args) {
        eprintln!("--elevated-op: {e}");
        return 1;
    }
    0
}

/// Parent side: serialise a trash/delete `op`, run the elevated worker (blocks
/// on the OS auth prompt — call from a background thread), and read back which
/// items landed in the Trash and which still failed.
pub fn run_elevated_trash_op(op: &ElevatedTrashOp) -> Result<ElevatedTrashResult, String> {
    let files = ElevFiles::create(&op.encode())?;
    let args = match files.worker_args("--elevated-trash") {
        Ok(args) => args,
        Err(e) => {
            files.cleanup();
            return Err(e);
        }
    };
    let run = crate::platform_shell::run_elevated_self(&args);
    let outcome = run.and_then(|_code| {
        std::fs::read(&files.result)
            .map_err(|e| format!("read result: {e}"))
            .and_then(|b| ElevatedTrashResult::decode(&b))
    });

    files.cleanup();
    outcome
}

/// Worker side: `--elevated-trash <descriptor> --elevated-result <result>`.
/// Runs as root: moves each item into the user's Trash (or removes it outright
/// when `delete`), writes the per-item result, and returns a process exit code.
/// Always exits 0 when it *ran* (item failures live in the result file), so the
/// macOS osascript wrapper treats "ran" as success.
pub fn run_elevated_trash_op_worker(args: &[String]) -> i32 {
    let Some(desc_path) = flag_value(args, "--elevated-trash") else {
        eprintln!("--elevated-trash: missing descriptor path");
        return 2;
    };
    let Some(result_path) = flag_value(args, "--elevated-result") else {
        eprintln!("--elevated-trash: missing --elevated-result path");
        return 2;
    };
    let bytes = match read_descriptor_verified(&desc_path, args) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("--elevated-trash: {e}");
            return 2;
        }
    };
    let op = match ElevatedTrashOp::decode(&bytes) {
        Ok(op) => op,
        Err(e) => {
            eprintln!("--elevated-trash: {e}");
            return 2;
        }
    };

    let mut result = ElevatedTrashResult::default();
    for src in &op.sources {
        if op.delete {
            match remove_recursively(src) {
                Ok(()) => {}
                Err(_) => result.failed.push(src.clone()),
            }
        } else {
            match move_into_trash(src, &op.trash_dir) {
                Ok(landed) => result.trashed.push((src.clone(), landed)),
                Err(_) => result.failed.push(src.clone()),
            }
        }
    }

    if let Err(e) = write_result_verified(&result_path, &result.encode(), args) {
        eprintln!("--elevated-trash: {e}");
        return 1;
    }
    0
}

/// Remove a file or directory tree, mirroring Empty Trash's own loop.
fn remove_recursively(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() && !m.is_symlink() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) => Err(e),
    }
}

/// Move `src` into `trash_dir` under a collision-free name (`name`, `name 2`,
/// `name 3`, …), returning where it landed. Same-volume → an instant rename;
/// the user's Trash and a protected app under `/Applications` share the Data
/// volume in practice, so a cross-device fallback isn't needed here (an EXDEV
/// rename surfaces as a per-item failure rather than a silent half-copy).
fn move_into_trash(src: &Path, trash_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(trash_dir)?;
    let name = src.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no file name")
    })?;
    let mut dest = trash_dir.join(name);
    let mut n = 2;
    while dest.exists() {
        dest = trash_dir.join(format!("{} {n}", name.to_string_lossy()));
        n += 1;
    }
    std::fs::rename(src, &dest)?;
    Ok(dest)
}

/// Turn a whole-op failure (planning failed) into per-source failures so the
/// parent still gets a coherent "all still failed" report.
fn fatal_result(op: &ElevatedOp, raw: &str) -> ElevatedResult {
    ElevatedResult {
        ok: 0,
        failures: op
            .sources
            .iter()
            .map(|s| (kind_from_raw(raw), s.clone()))
            .collect(),
    }
}

/// Coarse classification of the planning error string (mirrors the GPUI
/// `classify_error_text`, kept tiny here to avoid a cross-crate dep).
fn kind_from_raw(raw: &str) -> FileOpErrorKind {
    let l = raw.to_ascii_lowercase();
    if l.contains("permission") || l.contains("denied") {
        FileOpErrorKind::PermissionDenied
    } else if l.contains("not found") || l.contains("no such") {
        FileOpErrorKind::NotFound
    } else {
        FileOpErrorKind::Other
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_descriptor_round_trips() {
        let op = ElevatedOp {
            is_move: true,
            dest_dir: PathBuf::from("/dest dir/with space"),
            sources: vec![PathBuf::from("/a/one.txt"), PathBuf::from("/b/two.bin")],
        };
        let decoded = ElevatedOp::decode(&op.encode()).unwrap();
        assert!(decoded.is_move);
        assert_eq!(decoded.dest_dir, op.dest_dir);
        assert_eq!(decoded.sources, op.sources);
    }

    #[test]
    fn result_round_trips() {
        let r = ElevatedResult {
            ok: 2,
            failures: vec![
                (FileOpErrorKind::Locked, PathBuf::from("/x/locked.db")),
                (
                    FileOpErrorKind::PermissionDenied,
                    PathBuf::from("/y/root.cfg"),
                ),
            ],
        };
        let decoded = ElevatedResult::decode(&r.encode()).unwrap();
        assert_eq!(decoded.ok, 2);
        assert_eq!(decoded.failures.len(), 2);
        assert_eq!(decoded.failures[0].0, FileOpErrorKind::Locked);
        assert_eq!(decoded.failures[1].1, PathBuf::from("/y/root.cfg"));
    }

    #[test]
    fn trash_op_descriptor_round_trips() {
        let op = ElevatedTrashOp {
            delete: false,
            trash_dir: PathBuf::from("/Users/jk/.Trash"),
            sources: vec![
                PathBuf::from("/Applications/iMovie.app"),
                PathBuf::from("/Applications/Some App.app"),
            ],
        };
        let decoded = ElevatedTrashOp::decode(&op.encode()).unwrap();
        assert!(!decoded.delete);
        assert_eq!(decoded.trash_dir, op.trash_dir);
        assert_eq!(decoded.sources, op.sources);

        let del = ElevatedTrashOp {
            delete: true,
            trash_dir: PathBuf::from("/unused"),
            sources: vec![PathBuf::from("/Volumes/x/.Trashes/501/old")],
        };
        assert!(ElevatedTrashOp::decode(&del.encode()).unwrap().delete);
    }

    #[test]
    fn trash_result_round_trips() {
        let r = ElevatedTrashResult {
            trashed: vec![(
                PathBuf::from("/Applications/iMovie.app"),
                PathBuf::from("/Users/jk/.Trash/iMovie.app"),
            )],
            failed: vec![PathBuf::from("/Applications/Locked.app")],
        };
        let decoded = ElevatedTrashResult::decode(&r.encode()).unwrap();
        assert_eq!(decoded.trashed.len(), 1);
        assert_eq!(
            decoded.trashed[0].1,
            PathBuf::from("/Users/jk/.Trash/iMovie.app")
        );
        assert_eq!(
            decoded.failed,
            vec![PathBuf::from("/Applications/Locked.app")]
        );

        // An all-success delete result (no trashed pairs, no failures) must
        // round-trip to an empty result, not a decode error.
        let empty = ElevatedTrashResult::default();
        let decoded = ElevatedTrashResult::decode(&empty.encode()).unwrap();
        assert!(decoded.trashed.is_empty() && decoded.failed.is_empty());
    }

    #[test]
    fn field_escaping_round_trips() {
        for raw in [
            &b"plain"[..],
            b"with\nnewline",
            b"back\\slash",
            b"\\n literal, then \n real",
            b"trailing backslash\\",
            b"nul\0inside",
            b"\n",
            b"",
        ] {
            assert_eq!(unescape_field(&escape_field(raw)), raw, "raw: {raw:?}");
            // Framing bytes must never survive escaping.
            let escaped = escape_field(raw);
            assert!(!escaped.contains(&b'\n') && !escaped.contains(&0));
        }
    }

    /// A `\n` in a filename must not split, truncate, or spoof result
    /// records — the class of bug that would let the elevated worker's
    /// report be forged by a crafted name.
    #[test]
    fn newline_in_paths_cannot_spoof_result_records() {
        let evil = PathBuf::from("/x/evil\nok\u{0}9999");
        let r = ElevatedResult {
            ok: 1,
            failures: vec![(FileOpErrorKind::Locked, evil.clone())],
        };
        let decoded = ElevatedResult::decode(&r.encode()).unwrap();
        assert_eq!(decoded.ok, 1, "embedded newline spoofed the ok record");
        assert_eq!(
            decoded.failures,
            vec![(FileOpErrorKind::Locked, evil.clone())]
        );

        let t = ElevatedTrashResult {
            trashed: vec![(evil.clone(), PathBuf::from("/t/landed\nfail\u{0}/forged"))],
            failed: vec![evil.clone()],
        };
        let decoded = ElevatedTrashResult::decode(&t.encode()).unwrap();
        assert_eq!(decoded.trashed, t.trashed);
        assert_eq!(decoded.failed, t.failed);
    }

    /// End-to-end over the private handshake dir: what the parent stages is
    /// exactly what a verifying worker reads back — and tampered ownership /
    /// permissions / symlinked descriptors are refused.
    #[cfg(unix)]
    #[test]
    fn private_dir_handshake_verifies() {
        use std::os::unix::fs::PermissionsExt as _;

        let payload = b"MOVE\0/dest\0/src/a".to_vec();
        let files = ElevFiles::create(&payload).unwrap();
        assert!(
            files.dir.file_name().unwrap().len() > "ferail-elev-".len() + 16,
            "dir name should carry a random token: {:?}",
            files.dir
        );
        let uid = files.owner_uid().unwrap();
        let desc_str = files.desc.to_string_lossy().into_owned();
        let args = vec!["--elevated-uid".to_string(), uid.to_string()];

        // Happy path: verified read returns the staged bytes.
        assert_eq!(read_descriptor_verified(&desc_str, &args).unwrap(), payload);

        // Missing / wrong uid is refused.
        assert!(read_descriptor_verified(&desc_str, &[]).is_err());
        let wrong = vec!["--elevated-uid".to_string(), (uid ^ 1).to_string()];
        assert!(read_descriptor_verified(&desc_str, &wrong).is_err());

        // A group/other-accessible handshake dir is refused.
        std::fs::set_permissions(&files.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read_descriptor_verified(&desc_str, &args).is_err());
        std::fs::set_permissions(&files.dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        // A symlink planted where the descriptor should be is refused
        // (O_NOFOLLOW), even when it points at a file we own.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let target = files.dir.join("decoy");
            std::fs::write(&target, b"COPY\0/attacker\0/etc/shadow").unwrap();
            std::fs::remove_file(&files.desc).unwrap();
            std::os::unix::fs::symlink(&target, &files.desc).unwrap();
            assert!(read_descriptor_verified(&desc_str, &args).is_err());
            std::fs::remove_file(&target).unwrap();
        }

        // Result write is exclusive: a pre-created file (or planted symlink)
        // at the result path makes the worker fail instead of following it.
        write_result_verified(&files.result.to_string_lossy(), b"ok\x000", &args).unwrap();
        assert!(write_result_verified(&files.result.to_string_lossy(), b"ok\x000", &args).is_err());
        assert_eq!(std::fs::read(&files.result).unwrap(), b"ok\x000");

        files.cleanup();
        assert!(!files.dir.exists(), "cleanup removes the handshake dir");
    }
}
