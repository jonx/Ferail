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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use feraille_fs_native::file_ops::{
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
        let sources: Vec<PathBuf> = parts
            .filter(|p| !p.is_empty())
            .map(bytes_to_path)
            .collect();
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
    /// `fail\0<KindName>\0<path>` per remaining failure.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"ok\0");
        out.extend_from_slice(self.ok.to_string().as_bytes());
        for (kind, path) in &self.failures {
            out.push(b'\n');
            out.extend_from_slice(b"fail\0");
            out.extend_from_slice(kind_name(*kind).as_bytes());
            out.push(0);
            out.extend_from_slice(&path_to_bytes(path));
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
                    result
                        .failures
                        .push((kind_from_name(&String::from_utf8_lossy(kind)), bytes_to_path(path)));
                }
                _ => {}
            }
        }
        Ok(result)
    }
}

fn unique_temp(ext: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("feraille-elevop-{}-{n}.{ext}", std::process::id()))
}

/// Parent side: serialise `op` to a temp descriptor, run the elevated worker
/// (blocks on the OS auth prompt — call from a background thread), and read
/// back which items still failed. An empty `failures` means everything landed.
pub fn run_elevated_op(op: &ElevatedOp) -> Result<ElevatedResult, String> {
    let desc = unique_temp("desc");
    let res = unique_temp("result");
    std::fs::write(&desc, op.encode()).map_err(|e| format!("write descriptor: {e}"))?;

    let args = vec![
        "--elevated-op".to_string(),
        desc.to_string_lossy().into_owned(),
        "--elevated-result".to_string(),
        res.to_string_lossy().into_owned(),
    ];
    let run = crate::platform_shell::run_elevated_self(&args);
    // The worker exits 0 even when some items fail (per-item status is in the
    // result file); a hard error here means it couldn't be launched/elevated.
    let outcome = run.and_then(|_code| {
        std::fs::read(&res)
            .map_err(|e| format!("read result: {e}"))
            .and_then(|b| ElevatedResult::decode(&b))
    });

    let _ = std::fs::remove_file(&desc);
    let _ = std::fs::remove_file(&res);
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
    let bytes = match std::fs::read(&desc_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("--elevated-op: read descriptor: {e}");
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

    if let Err(e) = std::fs::write(&result_path, result.encode()) {
        eprintln!("--elevated-op: write result: {e}");
        return 1;
    }
    0
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
                (FileOpErrorKind::PermissionDenied, PathBuf::from("/y/root.cfg")),
            ],
        };
        let decoded = ElevatedResult::decode(&r.encode()).unwrap();
        assert_eq!(decoded.ok, 2);
        assert_eq!(decoded.failures.len(), 2);
        assert_eq!(decoded.failures[0].0, FileOpErrorKind::Locked);
        assert_eq!(decoded.failures[1].1, PathBuf::from("/y/root.cfg"));
    }
}
