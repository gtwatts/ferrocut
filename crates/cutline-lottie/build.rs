//! Links the static ThorVG built by `scripts/build-thorvg.sh`.
//! If it hasn't been built, the crate compiles as an empty stub (with a warning)
//! so `cargo build --workspace` keeps working for everyone else.
use std::{env, path::PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(cutline_no_thorvg)");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CUTLINE_THORVG_PREFIX");

    let here = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let prefix = env::var("CUTLINE_THORVG_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| here.join("third_party/install"));
    let lib = prefix.join("lib/libthorvg-1.a");
    println!("cargo:rerun-if-changed={}", lib.display());

    if !lib.exists() {
        println!(
            "cargo:warning=cutline-lottie: ThorVG not found at {} -- building an empty stub. \
             Run crates/cutline-lottie/scripts/build-thorvg.sh",
            lib.display()
        );
        println!("cargo:rustc-cfg=cutline_no_thorvg");
        return;
    }
    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    println!("cargo:rustc-link-lib=static=thorvg-1");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=pthread");
}
