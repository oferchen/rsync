use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    println!("cargo:rerun-if-env-changed=CARGO_WORKSPACE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-env-changed=OC_RSYNC_BUILD_OVERRIDE");

    if let Some(git_dir) = git_directory(&manifest_dir) {
        emit_rerun_if_exists(&git_dir.join("HEAD"));
        emit_rerun_if_exists(&git_dir.join("refs/heads"));
        emit_rerun_if_exists(&git_dir.join("refs/remotes"));
        emit_rerun_if_exists(&git_dir.join("packed-refs"));
    }

    let workspace_root = workspace_root(&manifest_dir).unwrap_or_else(|| manifest_dir.clone());
    println!(
        "cargo:rustc-env=RSYNC_WORKSPACE_ROOT={}",
        workspace_root.display()
    );

    let build_id = env::var("OC_RSYNC_BUILD_OVERRIDE")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| git_revision(&manifest_dir))
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=OC_RSYNC_BUILD_REV={build_id}");
}

fn git_revision(manifest_dir: &Path) -> Option<String> {
    run_git(manifest_dir, &["rev-parse", "--short", "HEAD"])
}

fn git_directory(manifest_dir: &Path) -> Option<PathBuf> {
    run_git(manifest_dir, &["rev-parse", "--git-dir"]).map(|output| {
        let path = PathBuf::from(output);
        if path.is_relative() {
            manifest_dir.join(path)
        } else {
            path
        }
    })
}

fn workspace_root(manifest_dir: &Path) -> Option<PathBuf> {
    run_git(manifest_dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn run_git(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn emit_rerun_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
