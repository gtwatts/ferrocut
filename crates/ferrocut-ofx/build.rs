//! Builds the out-of-process OpenFX host (`ferrocut-ofx-host`) and the test
//! plugin bundles with CMake, from sources fetched by `scripts/fetch-deps.sh`.
//! No network access here.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ferrocut_ofx_no_host)");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let tp = env::var_os("FERROCUT_OFX_THIRD_PARTY").map(PathBuf::from).unwrap_or_else(|| manifest.join("third_party"));
    let openfx = tp.join("src-openfx");
    let expat = tp.join("src-expat");
    println!("cargo:rerun-if-env-changed=FERROCUT_OFX_THIRD_PARTY");
    println!("cargo:rerun-if-changed=host");
    println!("cargo:rerun-if-changed={}", tp.display());
    if !openfx.join("HostSupport").is_dir() || !expat.join("expat").is_dir() {
        // Keep `cargo build --workspace` green on a fresh clone.
        println!(
            "cargo::warning=ferrocut-ofx: OpenFX/libexpat sources not found under {}; building a STUB crate. \
             Run crates/ferrocut-ofx/scripts/fetch-deps.sh once (git clones pinned tags).",
            tp.display()
        );
        println!("cargo:rustc-cfg=ferrocut_ofx_no_host");
        return;
    }
    let dst = cmake::Config::new("host")
        .define("OPENFX_DIR", &openfx)
        .define("EXPAT_DIR", &expat)
        .profile("RelWithDebInfo")
        .build();
    let exe = dst.join("bin/ferrocut-ofx-host");
    let plugins = dst.join("plugins");
    println!("cargo:rustc-env=FERROCUT_OFX_HOST_EXE={}", exe.display());
    println!("cargo:rustc-env=FERROCUT_OFX_BUNDLED_PLUGINS={}", plugins.display());
}
