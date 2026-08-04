//! Bakes the Cargo profile output directory into `test-support`.
//!
//! `CARGO_BIN_EXE_oc-rsync` is only handed to the compiler for targets of the
//! package that declares the `oc-rsync` `[[bin]]`, so `env!` is a hard compile
//! error in every other package. `OUT_DIR`, by contrast, is given to every
//! build script, and Cargo lays it out as
//! `<target-dir>/[<triple>/]<profile>/build/<pkg>-<hash>/out`. The `build`
//! directory Cargo puts build-script output under sits directly inside the
//! profile directory, so that profile directory - the exact directory the
//! current Cargo invocation links workspace binaries into, including custom
//! profiles, cross-compilation triples, and relocated target directories such
//! as `cargo llvm-cov`'s - is the parent of the nearest `build` ancestor. We
//! anchor on that landmark rather than a fixed ancestor count, which a deeper
//! OUT_DIR nesting on newer Cargo toolchains would silently break.
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
    // Cargo lays OUT_DIR out as `<profile>/build/<pkg>-<hash>/out`, but the
    // exact depth between `out` and `build` is not contractual: newer Cargo
    // toolchains nest it one level deeper, which silently broke a hard-coded
    // `ancestors().nth(3)` - it then resolved to `<profile>/build`, so every
    // workspace binary was sought under the build-script output directory and
    // never found (observed on a newer nightly macOS runner). Anchor on the one
    // stable landmark instead: Cargo always writes build-script output into a
    // `build` directory that sits directly inside the profile directory, so the
    // profile directory is that `build` directory's parent - correct regardless
    // of any intervening nesting.
    let profile_dir = out_dir
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "build"))
        .and_then(std::path::Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "unexpected OUT_DIR layout (no `build` ancestor): {}",
                out_dir.display()
            )
        });

    println!(
        "cargo:rustc-env=OC_RSYNC_TARGET_PROFILE_DIR={}",
        profile_dir.display()
    );
}
