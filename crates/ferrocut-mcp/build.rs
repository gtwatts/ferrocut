//! Embed the same FFmpeg rpaths as the `ferrocut` CLI (exported by
//! ferrocut-engine's build script), so `ferrocut-mcp` runs without
//! LD_LIBRARY_PATH.

fn main() {
    println!("cargo:rerun-if-env-changed=DEP_FERROCUT_ENGINE_FFMPEG_RPATHS");
    if let Ok(rps) = std::env::var("DEP_FERROCUT_ENGINE_FFMPEG_RPATHS") {
        for rp in rps.split(';').filter(|r| !r.is_empty()) {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{rp}");
        }
    }
}
