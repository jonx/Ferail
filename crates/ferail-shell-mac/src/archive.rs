//! Compress files/folders into a `.zip` archive next to the source,
//! matching Finder's right-click "Compress" behaviour.
//!
//! Uses `/usr/bin/ditto` with `-c -k --sequesterRsrc --keepParent`:
//! that's the same combination Finder runs. Produces `<name>.zip`
//! for a single source, or `Archive.zip` for multiples, same
//! naming Finder uses.
//!
//! Synchronous on the calling thread; callers dispatch from a
//! worker per [`docs/UI_NONBLOCKING.md`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// Compress `targets` into a single zip next to the first target's
/// parent. Returns the destination zip path. Errors propagate from
/// `ditto`'s exit status / stderr.
pub fn compress(targets: &[&Path]) -> Result<PathBuf, String> {
    if targets.is_empty() {
        return Err("compress called with no targets".into());
    }

    let parent = targets[0]
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", targets[0].display()))?
        .to_path_buf();

    let dst = pick_zip_name(&parent, targets)
        .ok_or_else(|| "exhausted Archive index range".to_string())?;

    // ditto is the same tool Finder uses. Flags:
    //   -c           create archive
    //   -k           PKZip format (vs xar default)
    //   --sequesterRsrc  preserve resource forks via __MACOSX/
    //   --keepParent     include the parent dir name when given a
    //                    single source: matters for "compress
    //                    Foo" producing Foo.zip containing Foo/.
    let mut cmd = Command::new("/usr/bin/ditto");
    cmd.arg("-c")
        .arg("-k")
        .arg("--sequesterRsrc")
        .arg("--keepParent");
    for t in targets {
        cmd.arg(t);
    }
    cmd.arg(&dst);

    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn ditto: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ditto exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(dst)
}

/// Choose a non-colliding ".zip" path. Single target: "<name>.zip"
/// (or "<name> 2.zip" on collision). Multiple: "Archive.zip" (or
/// "Archive 2.zip"). Mirrors Finder.
fn pick_zip_name(parent: &Path, targets: &[&Path]) -> Option<PathBuf> {
    let stem = if targets.len() == 1 {
        targets[0]
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Archive".to_string())
    } else {
        "Archive".to_string()
    };
    for n in 1..=9999 {
        let name = if n == 1 {
            format!("{stem}.zip")
        } else {
            format!("{stem} {n}.zip")
        };
        let candidate = parent.join(&name);
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
