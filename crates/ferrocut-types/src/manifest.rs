//! Content hash of the local files a node depends on.
//!
//! Nodes whose output depends on files beyond their own parameters (an HTML
//! page and every image, font, script and stylesheet it loads; a Lottie file and
//! its assets; a LUT) put [`FileManifest::digest`] into their
//! [`NodeHash`](crate::NodeHash), so editing any of those files invalidates
//! exactly the frames that node produced.
//!
//! The digest covers `(relative path, blake3 of bytes)` per file, sorted by
//! path, so it is independent of the order files were discovered in and of
//! where the project lives on disk (absolute paths never enter the hash; moving
//! or renaming the project directory keeps every cache key).
//!
//! **Determinism:** a manifest can only cover local files. A node that lets its
//! content fetch from the network (`http(s)://`, `ws://`, DNS-dependent URLs)
//! is not a function of its inputs, and no hash can fix that. Such nodes
//! must block network access while rendering (e.g. a Chromium request
//! interceptor that fails every non-`file:` request), record the local files
//! actually loaded, and fail the render (Permanent) when a page tries to load
//! a local file that is missing from the manifest.
//!
//! ```
//! # use ferrocut_types::FileManifest;
//! let mut m = FileManifest::new();
//! m.add_bytes("index.html", b"<img src=logo.png>").unwrap();
//! m.add_bytes("logo.png", b"\x89PNG...").unwrap();
//! let mut n = FileManifest::new();
//! n.add_bytes("logo.png", b"\x89PNG...").unwrap();
//! n.add_bytes("index.html", b"<img src=logo.png>").unwrap();
//! assert_eq!(m.digest(), n.digest()); // insertion order doesn't matter
//! ```

use std::collections::BTreeMap;
use std::path::{Component, Path};

use crate::error::NodeError;

/// A set of `(relative path -> blake3 of contents)` entries with a stable digest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileManifest {
    /// Keyed by normalized relative path ('/'-separated, no `.`/`..`).
    entries: BTreeMap<String, [u8; 32]>,
}

impl FileManifest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hash `files` (absolute, or relative to `root`), recording each by its
    /// path relative to `root`. Symlinks are resolved first, and every file must
    /// resolve inside `root`. Errors are [`ErrorKind::Permanent`](crate::ErrorKind).
    pub fn from_files<P: AsRef<Path>>(
        root: impl AsRef<Path>,
        files: impl IntoIterator<Item = P>,
    ) -> Result<Self, NodeError> {
        let mut m = Self::new();
        for f in files {
            m.add_file(root.as_ref(), f.as_ref())?;
        }
        Ok(m)
    }

    /// Add `file` (absolute, or relative to `root`) under its path relative to
    /// `root`. Fails if the file is unreadable or resolves outside `root`.
    pub fn add_file(&mut self, root: &Path, file: &Path) -> Result<&mut Self, NodeError> {
        let err = |p: &Path, e: &dyn std::fmt::Display| {
            NodeError::permanent(format!("file manifest: {}: {e}", p.display()))
        };
        let root = std::fs::canonicalize(root).map_err(|e| err(root, &e))?;
        let abs = std::fs::canonicalize(root.join(file)).map_err(|e| err(file, &e))?;
        let rel = abs.strip_prefix(&root).map_err(|_| {
            err(
                file,
                &format!("resolves outside the project root {}", root.display()),
            )
        })?;
        let mut h = blake3::Hasher::new();
        let f = std::fs::File::open(&abs).map_err(|e| err(&abs, &e))?;
        h.update_reader(f).map_err(|e| err(&abs, &e))?;
        self.insert(rel, *h.finalize().as_bytes())
    }

    /// Add in-memory contents under the relative path `rel` (`/`-separated).
    pub fn add_bytes(&mut self, rel: &str, bytes: &[u8]) -> Result<&mut Self, NodeError> {
        self.insert(Path::new(rel), *blake3::hash(bytes).as_bytes())
    }

    fn insert(&mut self, rel: &Path, hash: [u8; 32]) -> Result<&mut Self, NodeError> {
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(s) => parts.push(s.to_str().ok_or_else(|| {
                    NodeError::permanent(format!("file manifest: non-UTF-8 path {}", rel.display()))
                })?),
                Component::CurDir => {}
                _ => {
                    return Err(NodeError::permanent(format!(
                        "file manifest: {} must be relative and stay inside the root",
                        rel.display()
                    )));
                }
            }
        }
        if parts.is_empty() {
            return Err(NodeError::permanent("file manifest: empty path"));
        }
        self.entries.insert(parts.join("/"), hash);
        Ok(self)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `(relative path, blake3 of contents)` in digest order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &[u8; 32])> {
        self.entries.iter().map(|(p, h)| (p.as_str(), h))
    }

    /// Stable digest of the manifest: put it into the node's `NodeHash`.
    pub fn digest(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"ferrocut.files.v1\0");
        h.update(&(self.entries.len() as u64).to_le_bytes());
        for (path, hash) in &self.entries {
            h.update(&(path.len() as u64).to_le_bytes());
            h.update(path.as_bytes());
            h.update(hash);
        }
        *h.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ferrocut-manifest-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("assets")).unwrap();
        d
    }

    #[test]
    fn order_independent_and_location_independent() {
        let (a, b) = (tmp("a"), tmp("b"));
        for d in [&a, &b] {
            std::fs::write(d.join("index.html"), "<p>hi</p>").unwrap();
            std::fs::write(d.join("assets/logo.svg"), "<svg/>").unwrap();
        }
        let m1 = FileManifest::from_files(&a, ["index.html", "assets/logo.svg"]).unwrap();
        // Different order, absolute paths, a different directory: same digest.
        let m2 = FileManifest::from_files(&b, [b.join("assets/./logo.svg"), b.join("index.html")])
            .unwrap();
        assert_eq!(m1.digest(), m2.digest());
        let paths: Vec<&str> = m1.entries().map(|(p, _)| p).collect();
        assert_eq!(paths, ["assets/logo.svg", "index.html"]);
        // Duplicates collapse.
        let m3 =
            FileManifest::from_files(&a, ["index.html", "assets/logo.svg", "index.html"]).unwrap();
        assert_eq!(m3.digest(), m1.digest());
        // Same bytes as add_bytes.
        let mut m4 = FileManifest::new();
        m4.add_bytes("index.html", b"<p>hi</p>").unwrap();
        m4.add_bytes("assets/logo.svg", b"<svg/>").unwrap();
        assert_eq!(m4.digest(), m1.digest());
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn content_and_name_changes_change_the_digest() {
        let d = tmp("c");
        std::fs::write(d.join("index.html"), "<p>hi</p>").unwrap();
        std::fs::write(d.join("assets/x.css"), "p{}").unwrap();
        let base = FileManifest::from_files(&d, ["index.html", "assets/x.css"])
            .unwrap()
            .digest();
        std::fs::write(d.join("assets/x.css"), "p{color:red}").unwrap();
        let edited = FileManifest::from_files(&d, ["index.html", "assets/x.css"])
            .unwrap()
            .digest();
        assert_ne!(base, edited);
        std::fs::rename(d.join("assets/x.css"), d.join("assets/y.css")).unwrap();
        std::fs::write(d.join("assets/y.css"), "p{}").unwrap();
        let renamed = FileManifest::from_files(&d, ["index.html", "assets/y.css"])
            .unwrap()
            .digest();
        assert_ne!(base, renamed);
        // Empty manifest is valid and distinct.
        assert_ne!(FileManifest::new().digest(), base);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rejects_escapes_and_missing_files() {
        let d = tmp("d");
        std::fs::write(d.join("index.html"), "x").unwrap();
        let mut m = FileManifest::new();
        assert!(m.add_bytes("../secret", b"x").is_err());
        assert!(m.add_bytes("/etc/passwd", b"x").is_err());
        assert!(m.add_bytes("", b"x").is_err());
        let e = FileManifest::from_files(d.join("assets"), ["../index.html"]).unwrap_err();
        assert!(e.message.contains("outside the project root"), "{e}");
        let e = FileManifest::from_files(&d, ["nope.png"]).unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::Permanent);
        std::fs::remove_dir_all(&d).ok();
    }
}
