//! Build-script helpers shared by Ferrocut crates (owned by Rusty; SeePlus's
//! crates can adopt it as a `[build-dependencies]` entry).
//!
//! [`emit_ffmpeg_rpaths`] finds the FFmpeg that `ffmpeg-sys-next` links (via
//! pkg-config), embeds rpaths to its libdir so binaries run without
//! `LD_LIBRARY_PATH`, and warns loudly when it isn't the project's LGPL build.
//!
//! Resolution order (every build script sees the same environment):
//!   1. `PKG_CONFIG_PATH` from your environment, if set;
//!   2. otherwise `.cargo/config.toml` points it at `third_party/ffmpeg-lgpl`
//!      (built by `scripts/build-ffmpeg-lgpl.sh`);
//!   3. FALLBACK: pkg-config's default search path (e.g. linuxbrew's FFmpeg,
//!      which is a GPL build: fine for local hacking, NOT for distribution).
//!
//! The rpath is relocatable where possible: the libdir is canonicalized (so a
//! symlinked checkout path never gets baked in), and when both it and the
//! target dir live inside the workspace the binary gets `$ORIGIN`-relative
//! entries for `target/<profile>/` (CLIs) and `target/<profile>/{deps,examples}/`
//! (test and example binaries). A libdir outside the workspace is embedded as
//! its canonical absolute path. The project FFmpeg's own libs carry RUNPATH
//! `$ORIGIN/../lib`, so their inter-library deps resolve too.
//!
//! Environment: `FERROCUT_FFMPEG_LIBDIR` overrides the libdir (empty string
//! disables the rpath); `FERROCUT_REQUIRE_LGPL_FFMPEG=1` turns the fallback
//! warning into a build error.
//!
//! In a `build.rs` `main`:
//!
//! ```no_run
//! let rpaths = ferrocut_build::emit_ffmpeg_rpaths();
//! // Optional: export them via `links` metadata for dependents' binaries.
//! println!("cargo:rpaths={}", rpaths.join(";"));
//! ```

use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Environment variables the FFmpeg lookup depends on.
pub const ENV_VARS: &[&str] = &[
    "FERROCUT_FFMPEG_LIBDIR",
    "FERROCUT_LGPL_FFMPEG_PREFIX",
    "FERROCUT_REQUIRE_LGPL_FFMPEG",
    "PKG_CONFIG_PATH",
];

/// What [`resolve_ffmpeg`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegLink {
    /// pkg-config's libdir for libavcodec, if pkg-config found it.
    pub pkg_libdir: Option<String>,
    /// The libdir to embed (override, else pkg-config's), if any.
    pub libdir: Option<String>,
    /// Set when the linked FFmpeg is not the project's LGPL build.
    pub fallback_warning: Option<String>,
}

/// pkg-config's libdir for libavcodec.
pub fn pkg_libdir() -> Option<String> {
    let out = Command::new("pkg-config")
        .args(["--variable=libdir", "libavcodec"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Canonical path if it exists, else as given.
pub fn canon(p: impl AsRef<Path>) -> PathBuf {
    let p = p.as_ref();
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// `to` relative to `from` (both absolute, canonical).
pub fn relative(from: &Path, to: &Path) -> PathBuf {
    let (f, t): (Vec<Component>, Vec<Component>) =
        (from.components().collect(), to.components().collect());
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut r = PathBuf::new();
    for _ in common..f.len() {
        r.push("..");
    }
    for c in &t[common..] {
        r.push(c);
    }
    r
}

/// The workspace root: the nearest ancestor of `manifest_dir` whose
/// `Cargo.toml` has a `[workspace]` table (else `manifest_dir`).
pub fn workspace_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|d| {
            std::fs::read_to_string(d.join("Cargo.toml"))
                .is_ok_and(|t| t.lines().any(|l| l.trim() == "[workspace]"))
        })
        .unwrap_or(manifest_dir)
        .to_path_buf()
}

/// rpath entries for `libdir`, for binaries under `profile_dir`
/// (`target/<profile>`) of `workspace`.
pub fn rpaths_for(libdir: &Path, profile_dir: Option<&Path>, workspace: &Path) -> Vec<String> {
    let lib = canon(libdir);
    let workspace = canon(workspace);
    match profile_dir.map(canon) {
        Some(profile) if lib.starts_with(&workspace) && profile.starts_with(&workspace) => {
            let rel = relative(&profile, &lib);
            vec![
                format!("$ORIGIN/{}", rel.display()),
                format!("$ORIGIN/../{}", rel.display()),
            ]
        }
        _ => vec![lib.display().to_string()],
    }
}

/// `target/<profile>` from a build script's `OUT_DIR`
/// (`<target>/<profile>/build/<pkg>-<hash>/out`).
pub fn profile_dir_of(out_dir: &Path) -> Option<PathBuf> {
    out_dir.ancestors().nth(3).map(Path::to_path_buf)
}

/// Locate FFmpeg the way `ffmpeg-sys-next` does (no output printed).
pub fn resolve_ffmpeg() -> FfmpegLink {
    let pkg = pkg_libdir();
    let mut fallback_warning = None;
    if let (Some(dir), Ok(prefix)) = (&pkg, std::env::var("FERROCUT_LGPL_FFMPEG_PREFIX")) {
        let lgpl_lib = canon(&prefix).join("lib");
        if canon(dir) != lgpl_lib {
            fallback_warning = Some(format!(
                "FALLBACK FFmpeg: linking {dir}, not the LGPL build at {} \
                 (run scripts/build-ffmpeg-lgpl.sh). This may be a GPL build; do not distribute.",
                lgpl_lib.display()
            ));
        }
    }
    let libdir = std::env::var("FERROCUT_FFMPEG_LIBDIR")
        .ok()
        .or_else(|| pkg.clone())
        .filter(|d| !d.is_empty() && !d.starts_with("/usr/lib") && d != "/lib");
    FfmpegLink {
        pkg_libdir: pkg,
        libdir,
        fallback_warning,
    }
}

/// For a build script: print `rerun-if` lines, the LGPL fallback warning (or
/// error with `FERROCUT_REQUIRE_LGPL_FFMPEG=1`) and `-Wl,-rpath` link args.
/// Returns the rpath entries (empty if none apply).
pub fn emit_ffmpeg_rpaths() -> Vec<String> {
    for v in ENV_VARS {
        println!("cargo:rerun-if-env-changed={v}");
    }
    let link = resolve_ffmpeg();
    if let Some(dir) = &link.pkg_libdir {
        println!("cargo:rerun-if-changed={dir}/pkgconfig/libavcodec.pc");
    }
    if let Some(msg) = &link.fallback_warning {
        if std::env::var("FERROCUT_REQUIRE_LGPL_FFMPEG").is_ok_and(|v| v == "1") {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
    }
    let Some(dir) = link.libdir else {
        return Vec::new();
    };
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let workspace = workspace_root(manifest.as_deref().unwrap_or(Path::new(".")));
    let profile = std::env::var_os("OUT_DIR").and_then(|o| profile_dir_of(Path::new(&o)));
    let rps = rpaths_for(Path::new(&dir), profile.as_deref(), &workspace);
    for rp in &rps {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{rp}");
    }
    rps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        let r = relative(
            Path::new("/w/target/release"),
            Path::new("/w/third_party/ff/lib"),
        );
        assert_eq!(r, Path::new("../../third_party/ff/lib"));
        assert_eq!(
            relative(Path::new("/a/b"), Path::new("/a/b")),
            Path::new("")
        );
    }

    #[test]
    fn rpaths_inside_and_outside_the_workspace() {
        let d = std::env::temp_dir().join(format!("ferrocut-build-test-{}", std::process::id()));
        let (w, lib, prof) = (
            d.join("w"),
            d.join("w/third_party/ffmpeg-lgpl/lib"),
            d.join("w/target/release"),
        );
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(w.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::create_dir_all(w.join("crates/x")).unwrap();
        assert_eq!(workspace_root(&w.join("crates/x")), w);
        assert_eq!(
            rpaths_for(&lib, Some(&prof), &w),
            [
                "$ORIGIN/../../third_party/ffmpeg-lgpl/lib",
                "$ORIGIN/../../../third_party/ffmpeg-lgpl/lib"
            ]
        );
        let outside = d.join("elsewhere/lib");
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(
            rpaths_for(&outside, Some(&prof), &w),
            [canon(&outside).display().to_string()]
        );
        assert_eq!(
            profile_dir_of(Path::new("/w/target/debug/build/pkg-123/out")),
            Some(PathBuf::from("/w/target/debug"))
        );
        std::fs::remove_dir_all(&d).unwrap();
    }
}
