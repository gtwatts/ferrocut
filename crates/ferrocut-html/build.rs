//! Builds the out-of-process CEF host (`ferrocut-html-host`) with CMake from the
//! prebuilt CEF distribution fetched by `scripts/fetch-cef.sh`. No network access
//! here; without CEF the crate builds as a stub so fresh clones stay green.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ferrocut_html_no_host)");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cef = env::var_os("FERROCUT_CEF_ROOT").map(PathBuf::from).unwrap_or_else(|| manifest.join("third_party/cef"));
    let marker = cef.join(".ferrocut-version");
    println!("cargo:rerun-if-env-changed=FERROCUT_CEF_ROOT");
    println!("cargo:rerun-if-changed=host");
    println!("cargo:rerun-if-changed={}", marker.display());

    // Shim version is part of node hashes; the host reports it in HELLO too.
    let shim = std::fs::read_to_string(manifest.join("host/src/shim.h")).expect("host/src/shim.h");
    let shim_version = shim
        .lines()
        .find_map(|l| l.strip_prefix("#define FERROCUT_SHIM_VERSION "))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .expect("FERROCUT_SHIM_VERSION in shim.h");
    println!("cargo:rustc-env=FERROCUT_HTML_SHIM_VERSION={shim_version}");

    let Ok(cef_version) = std::fs::read_to_string(&marker) else {
        println!(
            "cargo::warning=ferrocut-html: CEF not found at {}; building a STUB crate. \
             Run crates/ferrocut-html/scripts/fetch-cef.sh once (downloads the pinned prebuilt CEF, ~400 MB).",
            cef.display()
        );
        println!("cargo:rustc-cfg=ferrocut_html_no_host");
        return;
    };
    let dst = cmake::Config::new("host")
        .define("CEF_ROOT", &cef)
        .profile("Release")
        .build_target("ferrocut-html-host")
        .build();
    let exe = dst.join("build/ferrocut-html-host");
    println!("cargo:rustc-env=FERROCUT_HTML_HOST_EXE={}", exe.display());
    println!("cargo:rustc-env=FERROCUT_HTML_CEF_VERSION={}", cef_version.trim());
}
