//! Locate the FFmpeg that `ffmpeg-sys-next` links (via pkg-config), embed an
//! rpath to its libdir so `cutline` runs without LD_LIBRARY_PATH, and warn
//! loudly when it isn't the project's LGPL build.
//!
//! Resolution order (both build scripts see the same environment):
//!   1. PKG_CONFIG_PATH from your environment, if set;
//!   2. otherwise `.cargo/config.toml` points it at third_party/ffmpeg-lgpl
//!      (built by scripts/build-ffmpeg-lgpl.sh);
//!   3. FALLBACK: pkg-config's default search path (e.g. linuxbrew's FFmpeg,
//!      which is a GPL build: fine for local hacking, NOT for distribution).
//!
//! CUTLINE_FFMPEG_LIBDIR overrides the rpath (empty string disables it).
//! CUTLINE_REQUIRE_LGPL_FFMPEG=1 turns the fallback warning into an error.

use std::path::{Path, PathBuf};
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

fn main() {
    for v in [
        "CUTLINE_FFMPEG_LIBDIR",
        "CUTLINE_LGPL_FFMPEG_PREFIX",
        "CUTLINE_REQUIRE_LGPL_FFMPEG",
        "PKG_CONFIG_PATH",
    ] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    let resolved = pkg_libdir();
    if let (Some(dir), Ok(prefix)) = (&resolved, std::env::var("CUTLINE_LGPL_FFMPEG_PREFIX")) {
        println!("cargo:rerun-if-changed={dir}/pkgconfig/libavcodec.pc");
        let lgpl_lib = canon(&prefix).join("lib");
        if canon(dir) != lgpl_lib {
            let msg = format!(
                "FALLBACK FFmpeg: linking {dir}, not the LGPL build at {} \
                 (run scripts/build-ffmpeg-lgpl.sh). This may be a GPL build; do not distribute.",
                lgpl_lib.display()
            );
            if std::env::var("CUTLINE_REQUIRE_LGPL_FFMPEG").is_ok_and(|v| v == "1") {
                panic!("{msg}");
            }
            println!("cargo:warning={msg}");
        }
    }
    let libdir = std::env::var("CUTLINE_FFMPEG_LIBDIR").ok().or(resolved);
    if let Some(dir) = libdir.filter(|d| !d.is_empty() && !d.starts_with("/usr/lib") && d != "/lib")
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
