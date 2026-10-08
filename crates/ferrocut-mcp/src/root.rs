//! Project-root sandbox: every path a tool reads or writes must resolve
//! inside one directory.
//!
//! The root is `--root DIR`, else `$FERROCUT_MCP_ROOT`, else the server's
//! working directory, canonicalized once at startup. A path argument is
//! joined onto the root when relative, then:
//!
//! * if it exists, it is canonicalized (every symlink resolved, `..` folded);
//! * if not (a new output), its nearest existing ancestor is canonicalized and
//!   the missing tail appended; a `..` in the missing tail is rejected, and a
//!   dangling symlink anywhere is rejected (writing through it would create
//!   its target, possibly outside).
//!
//! The result must lie under the canonical root, so `../x`, absolute paths
//! elsewhere and symlinks (file or directory) pointing out of the root are
//! all rejected. Tools then use the canonical path. Timeline media sources
//! are checked the same way before anything decodes, probes or hashes them.
//!
//! Limits: this is a guard against agent mistakes and prompt-injected paths,
//! not an OS sandbox. A symlink swapped in between the check and the use
//! (TOCTOU) or a hard link to an outside file is not caught; run the server
//! as a user that cannot reach what it must not touch if that matters.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use ferrocut_engine::Timeline;

/// Environment variable naming the project root (overridden by `--root`).
pub const ENV: &str = "FERROCUT_MCP_ROOT";

#[derive(Clone, Debug)]
pub struct Root {
    dir: PathBuf,
}

impl Root {
    /// `dir` must be an existing directory; it is canonicalized.
    pub fn new(dir: &Path) -> anyhow::Result<Root> {
        let dir = std::fs::canonicalize(dir)
            .with_context(|| format!("project root {}", dir.display()))?;
        ensure!(
            dir.is_dir(),
            "project root {} is not a directory",
            dir.display()
        );
        Ok(Root { dir })
    }

    /// `--root` if given, else `$FERROCUT_MCP_ROOT` (if set and non-empty), else the cwd.
    pub fn from_args(arg: Option<PathBuf>) -> anyhow::Result<Root> {
        let dir = match arg {
            Some(d) => d,
            None => match std::env::var_os(ENV).filter(|v| !v.is_empty()) {
                Some(d) => PathBuf::from(d),
                None => std::env::current_dir().context("current directory")?,
            },
        };
        Root::new(&dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The canonical form of `p` (relative paths are relative to the root),
    /// or an error if it escapes the root. `p` need not exist.
    pub fn check(&self, p: &Path) -> anyhow::Result<PathBuf> {
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.dir.join(p)
        };
        let mut base = joined.clone();
        let mut tail: Vec<OsString> = Vec::new();
        while std::fs::symlink_metadata(&base).is_err() {
            match base.components().next_back() {
                Some(Component::Normal(n)) => {
                    tail.push(n.to_owned());
                    base.pop();
                }
                Some(Component::CurDir) => {
                    base.pop();
                }
                Some(Component::ParentDir) => {
                    bail!("{}: `..` after a missing directory", p.display())
                }
                _ => bail!("{}: no existing ancestor", p.display()),
            }
        }
        let canon = std::fs::canonicalize(&base).with_context(|| {
            format!(
                "{}: cannot resolve {} (dangling symlink?)",
                p.display(),
                base.display()
            )
        })?;
        if !canon.starts_with(&self.dir) {
            bail!(
                "{} is outside the project root {} (resolves to {})",
                p.display(),
                self.dir.display(),
                canon.join(tail.iter().rev().collect::<PathBuf>()).display()
            );
        }
        Ok(tail.iter().rev().fold(canon, |acc, n| acc.join(n)))
    }

    /// Optional path argument.
    pub fn check_opt(&self, p: Option<PathBuf>) -> anyhow::Result<Option<PathBuf>> {
        p.map(|p| self.check(&p)).transpose()
    }

    /// Every clip source of a loaded timeline (sources resolved against its
    /// directory, as [`Timeline::load`] does) must be inside the root,
    /// including the sources of nested comps (at any depth).
    pub fn check_sources(&self, tl: &Timeline) -> anyhow::Result<()> {
        let own = |tl: &Timeline| -> anyhow::Result<()> {
            let mut tl = tl.clone();
            for s in tl.sources_mut() {
                self.check(s)
                    .with_context(|| format!("clip source {}", s.display()))?;
            }
            Ok(())
        };
        own(tl)?;
        ferrocut_engine::comp::visit(tl, &mut |p, inner| {
            own(inner).with_context(|| format!("nested composition {}", p.display()))
        })
    }

    /// Load a timeline (path checked) and check its media sources.
    pub fn load_timeline(&self, p: &Path) -> anyhow::Result<(PathBuf, Timeline)> {
        let p = self.check(p)?;
        let tl = Timeline::load(&p)?;
        self.check_sources(&tl)?;
        Ok((p, tl))
    }
}
