use std::env;
use std::path::{Path, PathBuf};

fn workspace_root(manifest_dir: &Path) -> PathBuf {
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("Cargo.lock");
        if candidate.is_file() {
            return ancestor.to_path_buf();
        }
    }

    manifest_dir.to_path_buf()
}

fn main() {
    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo");
    let manifest_dir = PathBuf::from(manifest_dir);
    let root = workspace_root(&manifest_dir);

    println!("cargo:rustc-env=RSYNC_WORKSPACE_ROOT={}", root.display());
}
