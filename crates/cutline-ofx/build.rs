//! Builds the out-of-process OpenFX host (`cutline-ofx-host`) and the test
//! plugin bundles with CMake, from sources fetched by `scripts/fetch-deps.sh`.
//! No network access here.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(cutline_ofx_no_host)");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let tp = env::var_os("CUTLINE_OFX_THIRD_PARTY").map(PathBuf::from).unwrap_or_else(|| manifest.join("third_party"));
    let openfx = tp.join("src-openfx");
    let expat = tp.join("src-expat");
    println!("cargo:rerun-if-env-changed=CUTLINE_OFX_THIRD_PARTY");
    println!("cargo:rerun-if-changed=host");
    println!("cargo:rerun-if-changed={}", tp.display());
    if !openfx.join("HostSupport").is_dir() || !expat.join("expat").is_dir() {
        // Keep `cargo build --workspace` green on a fresh clone.
        println!(
            "cargo::warning=cutline-ofx: OpenFX/libexpat sources not found under {}; building a STUB crate. \
             Run crates/cutline-ofx/scripts/fetch-deps.sh once (git clones pinned tags).",
            tp.display()
        );
        println!("cargo:rustc-cfg=cutline_ofx_no_host");
        return;
    }
    let dst = cmake::Config::new("host")
        .define("OPENFX_DIR", &openfx)
        .define("EXPAT_DIR", &expat)
        .profile("RelWithDebInfo")
        .build();
    let exe = dst.join("bin/cutline-ofx-host");
    let plugins = dst.join("plugins");
    println!("cargo:rustc-env=CUTLINE_OFX_HOST_EXE={}", exe.display());
    println!("cargo:rustc-env=CUTLINE_OFX_BUNDLED_PLUGINS={}", plugins.display());
}
