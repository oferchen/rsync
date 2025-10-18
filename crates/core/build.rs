use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn canonicalize_or_fallback(path: PathBuf) -> PathBuf {
    match fs::canonicalize(&path) {
        Ok(resolved) => resolved,
        Err(_) => path,
    }
}

fn workspace_root(manifest_dir: &Path) -> PathBuf {
    if let Some(workspace_dir) = env::var_os("CARGO_WORKSPACE_DIR") {
        let mut candidate: PathBuf = workspace_dir.into();
        if !candidate.is_absolute() {
            candidate = manifest_dir.join(candidate);
        }

        if candidate.is_dir() {
            return canonicalize_or_fallback(candidate);
        }
    }

    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("Cargo.lock");
        if candidate.is_file() {
            return canonicalize_or_fallback(ancestor.to_path_buf());
        }
    }

    canonicalize_or_fallback(manifest_dir.to_path_buf())
}

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_WORKSPACE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");

    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo");
    let manifest_dir = PathBuf::from(manifest_dir);
    let root = workspace_root(&manifest_dir);

    println!("cargo:rustc-env=RSYNC_WORKSPACE_ROOT={}", root.display());
}
