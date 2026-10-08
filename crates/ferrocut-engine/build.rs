//! Locate the FFmpeg that `ffmpeg-sys-next` links (via pkg-config), embed an
//! rpath to its libdir so `ferrocut` runs without LD_LIBRARY_PATH, and warn
//! loudly when it isn't the project's LGPL build.
//!
//! Resolution order (both build scripts see the same environment):
//!   1. PKG_CONFIG_PATH from your environment, if set;
//!   2. otherwise `.cargo/config.toml` points it at third_party/ffmpeg-lgpl
//!      (built by scripts/build-ffmpeg-lgpl.sh);
//!   3. FALLBACK: pkg-config's default search path (e.g. linuxbrew's FFmpeg,
//!      which is a GPL build: fine for local hacking, NOT for distribution).
//!
//! The rpath is relocatable where possible: the libdir is canonicalized (so a
//! symlinked checkout path never gets baked in), and when it lives inside the
//! workspace the binary gets `$ORIGIN`-relative entries for
//! `target/<profile>/` (the CLI) and `target/<profile>/{deps,examples}/` (test
//! and example binaries). A libdir outside the workspace is embedded as its
//! canonical absolute path. The project FFmpeg's own libs carry
//! RUNPATH `$ORIGIN/../lib`, so their inter-library deps resolve too.
//!
//! Crates with their own binaries (ferrocut-mcp) get the same entries via
//! `links` metadata: `DEP_FERROCUT_ENGINE_FFMPEG_RPATHS` (`;`-separated).
//!
//! FERROCUT_FFMPEG_LIBDIR overrides the rpath (empty string disables it).
//! FERROCUT_REQUIRE_LGPL_FFMPEG=1 turns the fallback warning into an error.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

fn pkg_libdir() -> Option<String> {
    let out = Command::new("pkg-config")
        .args(["--variable=libdir", "libavcodec"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn canon(p: &str) -> PathBuf {
    Path::new(p)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(p))
}

/// `to` relative to `from` (both absolute, canonical).
fn relative(from: &Path, to: &Path) -> PathBuf {
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

/// rpath entries for `libdir`.
fn rpaths(libdir: &str) -> Vec<String> {
    let lib = canon(libdir);
    let workspace = canon(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
    let profile = std::env::var("OUT_DIR").ok().and_then(|o| {
        Path::new(&o)
            .ancestors()
            .nth(3)
            .map(|p| canon(&p.to_string_lossy()))
    });
    match profile {
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

fn main() {
    for v in [
        "FERROCUT_FFMPEG_LIBDIR",
        "FERROCUT_LGPL_FFMPEG_PREFIX",
        "FERROCUT_REQUIRE_LGPL_FFMPEG",
        "PKG_CONFIG_PATH",
    ] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    let resolved = pkg_libdir();
    if let (Some(dir), Ok(prefix)) = (&resolved, std::env::var("FERROCUT_LGPL_FFMPEG_PREFIX")) {
        println!("cargo:rerun-if-changed={dir}/pkgconfig/libavcodec.pc");
        let lgpl_lib = canon(&prefix).join("lib");
        if canon(dir) != lgpl_lib {
            let msg = format!(
                "FALLBACK FFmpeg: linking {dir}, not the LGPL build at {} \
                 (run scripts/build-ffmpeg-lgpl.sh). This may be a GPL build; do not distribute.",
                lgpl_lib.display()
            );
            if std::env::var("FERROCUT_REQUIRE_LGPL_FFMPEG").is_ok_and(|v| v == "1") {
                panic!("{msg}");
            }
            println!("cargo:warning={msg}");
        }
    }
    let libdir = std::env::var("FERROCUT_FFMPEG_LIBDIR").ok().or(resolved);
    if let Some(dir) = libdir.filter(|d| !d.is_empty() && !d.starts_with("/usr/lib") && d != "/lib")
    {
        let rps = rpaths(&dir);
        for rp in &rps {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{rp}");
        }
        // `links = "ferrocut-engine-ffmpeg"`: dependents' build scripts (e.g.
        // ferrocut-mcp's binary) read DEP_FERROCUT_ENGINE_FFMPEG_RPATHS.
        println!("cargo:rpaths={}", rps.join(";"));
    }
}
