//! Embed rpaths to the FFmpeg that `ffmpeg-sys-next` links (relocatable,
//! `$ORIGIN`-relative inside the workspace) and warn when it isn't the
//! project's LGPL build. The logic lives in `ferrocut-build` (shared with any
//! crate that links FFmpeg); see its docs for the resolution order and the
//! `FERROCUT_FFMPEG_LIBDIR` / `FERROCUT_REQUIRE_LGPL_FFMPEG` knobs.
//!
//! Crates with their own binaries (ferrocut-mcp) get the same entries via
//! `links` metadata: `DEP_FERROCUT_ENGINE_FFMPEG_RPATHS` (`;`-separated).

fn main() {
    let rps = ferrocut_build::emit_ffmpeg_rpaths();
    if !rps.is_empty() {
        // `links = "ferrocut-engine-ffmpeg"`: dependents' build scripts read
        // DEP_FERROCUT_ENGINE_FFMPEG_RPATHS.
        println!("cargo:rpaths={}", rps.join(";"));
    }
}
