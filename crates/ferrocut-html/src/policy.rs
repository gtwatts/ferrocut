//! What a page may load, and the content hash of everything it can load.
//!
//! The host (host/src/main.cpp, `NetGuard`) cancels every request except
//! `file://` under the allowed roots and in-memory `data:`/`blob:`/`about:`
//! URLs, unless remote access is explicitly allowed. Because the page can only
//! read files under the roots, hashing every file under them (the page
//! manifest) covers every image, stylesheet, font and script it can use, and
//! editing any of them changes the node hash.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ferrocut_core::FileManifest;

/// Network and file access for an HTML layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NetworkPolicy {
    /// Allow `http(s)://` and `ws(s)://`. Off by default: with it on, frames
    /// depend on the network, not only on the node's inputs, and the node hash
    /// can't capture that. `file://` stays restricted to the roots either way.
    pub allow_remote: bool,
    /// Directories the page may load `file://` resources from in addition to
    /// its own directory. Their contents are hashed into the node too.
    pub extra_roots: Vec<PathBuf>,
}

/// A policy with canonical roots, ready for the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedPolicy {
    pub allow_remote: bool,
    /// Canonical, sorted, deduplicated.
    pub roots: Vec<PathBuf>,
}

impl ResolvedPolicy {
    /// `page_dir`: directory of a local page (its own root), if any.
    pub fn new(page_dir: Option<&Path>, policy: &NetworkPolicy) -> Result<Self, String> {
        let mut roots = Vec::new();
        for r in page_dir.into_iter().chain(policy.extra_roots.iter().map(PathBuf::as_path)) {
            let c = std::fs::canonicalize(r).map_err(|e| format!("allowed root {}: {e}", r.display()))?;
            if !c.is_dir() {
                return Err(format!("allowed root {} is not a directory", c.display()));
            }
            if c.as_os_str().as_bytes().contains(&b'\n') {
                return Err(format!("allowed root {} contains a newline", c.display()));
            }
            roots.push(c);
        }
        roots.sort();
        roots.dedup();
        Ok(ResolvedPolicy { allow_remote: policy.allow_remote, roots })
    }

    /// Environment for `ferrocut-html-host` (see main.cpp's header).
    pub fn host_env(&self) -> Vec<(OsString, OsString)> {
        let net = if self.allow_remote { "allow-remote" } else { "deny" };
        let mut roots = OsString::new();
        for (i, r) in self.roots.iter().enumerate() {
            if i > 0 {
                roots.push("\n");
            }
            roots.push(r);
        }
        vec![("FERROCUT_HTML_NET".into(), net.into()), ("FERROCUT_HTML_FILE_ROOTS".into(), roots)]
    }

    /// [`FileManifest`] digest of every file under the roots: `(path relative
    /// to its root, blake3)`, so absolute paths never enter the node hash and
    /// moving the project keeps cache keys. One manifest per root, combined
    /// in root order.
    pub fn manifest_digest(&self) -> Result<[u8; 32], String> {
        let mut all = FileManifest::new();
        for (i, root) in self.roots.iter().enumerate() {
            let mut files = Vec::new();
            walk(root, root, &mut files, &mut Budget::default())?;
            let m = FileManifest::from_files(root, &files).map_err(|e| e.to_string())?;
            all.add_bytes(&format!("root{i}"), &m.digest()).map_err(|e| e.to_string())?;
        }
        Ok(all.digest())
    }
}

/// Limits for hashing a page's roots: a page living in a huge directory
/// (home, Downloads) should get its own directory instead.
const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 2 << 30;

#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

/// Regular files under `dir` (recursively). Symlinks count only if they point
/// at a file inside `root` (the host refuses the others); symlinked
/// directories are skipped (their targets inside the root are walked anyway).
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, budget: &mut Budget) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        let ft = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
        let file = if ft.is_dir() {
            walk(root, &path, out, budget)?;
            continue;
        } else if ft.is_file() {
            true
        } else if ft.is_symlink() {
            std::fs::canonicalize(&path).is_ok_and(|t| t.starts_with(root) && t.is_file())
        } else {
            false
        };
        if file {
            budget.files += 1;
            budget.bytes += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if budget.files > MAX_FILES || budget.bytes > MAX_BYTES {
                return Err(format!(
                    "{} holds more than {MAX_FILES} files or {} GiB; give the page its own directory",
                    root.display(),
                    MAX_BYTES >> 30
                ));
            }
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferrocut-html-policy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        d
    }

    #[test]
    fn manifest_tracks_contents_not_location() {
        let a = tmp("a");
        let b = tmp("b");
        for d in [&a, &b] {
            std::fs::write(d.join("index.html"), "<img src=sub/x.svg>").unwrap();
            std::fs::write(d.join("sub/x.svg"), "<svg/>").unwrap();
        }
        let pa = ResolvedPolicy::new(Some(&a), &NetworkPolicy::default()).unwrap();
        let pb = ResolvedPolicy::new(Some(&b), &NetworkPolicy::default()).unwrap();
        let da = pa.manifest_digest().unwrap();
        assert_eq!(da, pb.manifest_digest().unwrap(), "same contents elsewhere: same digest");
        std::fs::write(b.join("sub/x.svg"), "<svg width='1'/>").unwrap();
        assert_ne!(da, pb.manifest_digest().unwrap(), "sub-resource edit changes the digest");
        std::fs::rename(a.join("sub/x.svg"), a.join("sub/y.svg")).unwrap();
        assert_ne!(da, pa.manifest_digest().unwrap(), "rename changes the digest");
        // Symlink pointing outside the root is not part of the page.
        std::fs::rename(a.join("sub/y.svg"), a.join("sub/x.svg")).unwrap();
        std::os::unix::fs::symlink(b.join("index.html"), a.join("escape.html")).unwrap();
        assert_eq!(da, pa.manifest_digest().unwrap());
        for d in [a, b] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn roots_are_canonical_and_env_is_newline_separated() {
        let a = tmp("c");
        let p = ResolvedPolicy::new(
            Some(&a.join("sub/..")),
            &NetworkPolicy { allow_remote: false, extra_roots: vec![a.join("sub"), a.clone()] },
        )
        .unwrap();
        assert_eq!(p.roots, vec![a.canonicalize().unwrap(), a.join("sub").canonicalize().unwrap()]);
        let env = p.host_env();
        assert_eq!(env[0].1, "deny");
        assert_eq!(env[1].1.to_str().unwrap().lines().count(), 2);
        assert!(
            ResolvedPolicy::new(None, &NetworkPolicy { allow_remote: true, extra_roots: vec![a.join("missing")] })
                .is_err()
        );
        std::fs::remove_dir_all(a).unwrap();
    }
}
