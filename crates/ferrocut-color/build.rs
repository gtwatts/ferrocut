//! Compiles the C ABI shim (cpp/ocio_shim.cpp) and links a static OpenColorIO.
//!
//! OCIO is located via `OCIO_ROOT` (an install prefix containing
//! include/OpenColorIO and lib/libOpenColorIO.a + lib/ocio-ext/*.a), defaulting
//! to `third_party/install` produced by `scripts/build-ocio.sh`.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ferrocut_no_ocio)");
    println!("cargo:rerun-if-changed=cpp/ocio_shim.cpp");
    println!("cargo:rerun-if-changed=cpp/ocio_shim.h");
    println!("cargo:rerun-if-env-changed=OCIO_ROOT");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = env::var_os("OCIO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("third_party/install"));
    let lib = root.join("lib");
    let ext = lib.join("ocio-ext");
    println!("cargo:rerun-if-changed={}", lib.display());
    if !lib.join("libOpenColorIO.a").exists() || !ext.is_dir() {
        // Don't break `cargo build --workspace` on a fresh clone: build an empty
        // crate and say how to get the real one.
        println!(
            "cargo::warning=ferrocut-color: static OpenColorIO not found under {}; building a STUB crate. \
             Run crates/ferrocut-color/scripts/build-ocio.sh once (cmake, C++17 compiler, git; a few minutes) \
             or set OCIO_ROOT.",
            root.display()
        );
        println!("cargo:rustc-cfg=ferrocut_no_ocio");
        return;
    }
    println!("cargo:rerun-if-changed={}", lib.join("libOpenColorIO.a").display());

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("cpp/ocio_shim.cpp")
        .include(root.join("include"))
        .warnings(true)
        .compile("ferrocut_ocio_shim");

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-search=native={}", ext.display());
    println!("cargo:rustc-link-lib=static=OpenColorIO");
    // Dependency order matters for static archives: users before providers.
    let mut ext_libs: Vec<String> = std::fs::read_dir(&ext)
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            name.strip_prefix("lib")?.strip_suffix(".a").map(str::to_owned)
        })
        .collect();
    let rank = |n: &str| match n {
        n if n.starts_with("yaml-cpp") => 0,
        n if n.starts_with("pystring") => 1,
        n if n.starts_with("minizip") => 2,
        n if n.starts_with("expat") => 3,
        n if n.starts_with("Imath") => 4,
        "z" => 5,
        _ => 6,
    };
    ext_libs.sort_by_key(|n| (rank(n), n.clone()));
    for l in ext_libs {
        println!("cargo:rustc-link-lib=static={l}");
    }
    println!("cargo:root={}", root.display());
}
