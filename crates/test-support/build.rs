//! Bakes the Cargo profile output directory into `test-support`.
//!
//! `CARGO_BIN_EXE_oc-rsync` is only handed to the compiler for targets of the
//! package that declares the `oc-rsync` `[[bin]]`, so `env!` is a hard compile
//! error in every other package. `OUT_DIR`, by contrast, is given to every
//! build script, and Cargo lays it out as
//! `<target-dir>/[<triple>/]<profile>/build/<pkg>-<hash>/out`. Its fourth
//! ancestor is therefore the exact profile directory that the current Cargo
//! invocation links workspace binaries into - including custom profiles,
//! cross-compilation triples, and relocated target directories such as
//! `cargo llvm-cov`'s.
//!
//! Emitting that one directory gives dependent crates a Cargo-provided,
//! compile-time anchor, replacing hand-rolled `target/{debug,release,dist}`
//! probing that can silently select a binary from a different revision.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("OUT_DIR is always set for build scripts"),
    );
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .unwrap_or_else(|| panic!("unexpected OUT_DIR layout: {}", out_dir.display()));

    println!(
        "cargo:rustc-env=OC_RSYNC_TARGET_PROFILE_DIR={}",
        profile_dir.display()
    );
}
