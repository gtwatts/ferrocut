//! Embed an rpath to the FFmpeg libdir pkg-config resolved, so `cutline` runs
//! against non-system FFmpeg installs (e.g. linuxbrew) without LD_LIBRARY_PATH.
//! Override with CUTLINE_FFMPEG_LIBDIR; set it to an empty string to disable.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CUTLINE_FFMPEG_LIBDIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    let libdir = std::env::var("CUTLINE_FFMPEG_LIBDIR").ok().or_else(|| {
        let out = Command::new("pkg-config")
            .args(["--variable=libdir", "libavcodec"])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    if let Some(dir) = libdir.filter(|d| !d.is_empty() && !d.starts_with("/usr/lib") && d != "/lib")
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
